//! Drain a [`crate::agent::llm::Provider::chat_stream`] response into a
//! complete [`crate::agent::llm::ChatResponse`] while forwarding each
//! event to a [`StreamSink`] for live UI / logging.
//!
//! This is the bridge between the streaming provider surface (live
//! token / tool-input deltas) and the existing single-response runtime
//! loop. Callers that want live token feeds plug a [`StreamSink`] in
//! through [`super::providers::common::accumulate_stream`] (this
//! function); callers that don't care about live events can keep
//! using `provider.chat()` directly.
//!
//! Behaviour:
//! - Text deltas concatenate into one `Text` content block.
//! - `ToolUseStart` opens a new `ToolUse` block; `ToolInputDelta`
//!   accumulates partial JSON into that block; the final `ToolUse`
//!   event seals the input as parsed JSON.
//! - A `Message` event short-circuits — providers that don't truly
//!   stream emit a single `Message` followed by `Done`; we adopt the
//!   message verbatim and ignore further block-level events.
//! - `Done` records `finish` + `usage` and terminates accumulation.
//! - The first `Err(LlmError::...)` from the stream propagates and
//!   aborts. Sinks still receive any successful events that arrived
//!   before the error.
//! - `Warning` events are forwarded to the sink but do not appear in
//!   the assembled `ChatResponse`.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::stream::BoxStream;
use futures_util::StreamExt;

use super::types::{ChatResponse, ContentBlock, FinishReason, StreamEvent, ToolCall, Usage};
use super::{LlmError, Result};

/// A consumer that wants to be notified of each [`StreamEvent`] as it
/// flows past. Implementations must be `Send + Sync` because the
/// stream is awaited inside a tokio task; they should also be cheap
/// (the sink is invoked synchronously between event dequeue and
/// accumulator update — slow sinks back-pressure the stream).
pub trait StreamSink: Send + Sync {
    fn on_event(&self, event: &StreamEvent);
}

/// Convenience: a `StreamSink` impl that does nothing. Useful as a
/// default when callers want streaming semantics without a UI.
pub struct NullSink;

impl StreamSink for NullSink {
    fn on_event(&self, _event: &StreamEvent) {}
}

/// Drain the entire stream, forwarding each event to `sink`, and
/// return a fully-assembled [`ChatResponse`].
///
/// `model` is used as the response's `model` field if no explicit
/// `Message` event is received.
pub async fn accumulate_stream(
    mut stream: BoxStream<'_, Result<StreamEvent>>,
    sink: Arc<dyn StreamSink>,
    model: &str,
) -> Result<ChatResponse> {
    let mut accumulator = Accumulator::new(model);

    while let Some(item) = stream.next().await {
        let event = item?;
        sink.on_event(&event);
        if accumulator.feed(event) {
            // `feed` returns true on terminal events. Stop draining.
            break;
        }
    }

    Ok(accumulator.finish())
}

/// State machine for assembling streamed events into a single
/// `ChatResponse`. Public to the crate so callers can build their own
/// driver loops without going through `accumulate_stream`.
pub(crate) struct Accumulator {
    model: String,
    /// Accumulated text — flushed into one `Text` block at finish().
    text: String,
    /// Tool blocks indexed by `tool_use.id` so out-of-order
    /// ToolInputDelta events still target the right block. BTreeMap
    /// preserves insertion order's deterministic-by-id but ToolUse
    /// blocks are emitted in the order ToolUseStart events arrived.
    tool_use_starts: Vec<(String, String)>, // (id, name) in arrival order
    tool_input_partials: BTreeMap<String, String>, // id → accumulated partial JSON
    /// Final ToolUse events override the partial JSON (provider-validated).
    finalised_tools: BTreeMap<String, serde_json::Value>,
    /// Provider-owned reasoning items that must survive into conversation
    /// history for a subsequent tool-result request.
    reasoning: Vec<ContentBlock>,
    tool_state: Vec<ContentBlock>,
    /// If a `Message` event arrives, we adopt the response verbatim
    /// and skip the assembled blocks/tools.
    explicit_message: Option<ChatResponse>,
    finish: FinishReason,
    usage: Usage,
    saw_done: bool,
}

impl Accumulator {
    pub(crate) fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            text: String::new(),
            tool_use_starts: Vec::new(),
            tool_input_partials: BTreeMap::new(),
            finalised_tools: BTreeMap::new(),
            reasoning: Vec::new(),
            tool_state: Vec::new(),
            explicit_message: None,
            finish: FinishReason::Stop,
            usage: Usage::default(),
            saw_done: false,
        }
    }

    /// Feed one event. Returns true on terminal events (`Done` /
    /// `Message`-without-following-Done) so callers can stop pulling.
    pub(crate) fn feed(&mut self, event: StreamEvent) -> bool {
        match event {
            StreamEvent::TextDelta { text } => {
                self.text.push_str(&text);
                false
            }
            StreamEvent::ToolUseStart { id, name } => {
                self.tool_use_starts.push((id.clone(), name));
                self.tool_input_partials.entry(id).or_default();
                false
            }
            StreamEvent::ToolInputDelta { id, partial_json } => {
                self.tool_input_partials
                    .entry(id)
                    .or_default()
                    .push_str(&partial_json);
                false
            }
            StreamEvent::ToolUse(tc) => {
                self.finalised_tools.insert(tc.id.clone(), tc.input.clone());
                // Auto-register the start in case the provider
                // skipped ToolUseStart (e.g. Bedrock when the JSON
                // arrives all at once).
                if !self.tool_use_starts.iter().any(|(id, _)| id == &tc.id) {
                    self.tool_use_starts.push((tc.id.clone(), tc.name.clone()));
                }
                false
            }
            StreamEvent::ToolState {
                tool_use_id,
                thought_signature,
            } => {
                self.tool_state.push(ContentBlock::ToolState {
                    tool_use_id,
                    thought_signature,
                });
                false
            }
            StreamEvent::Reasoning {
                id,
                summary,
                encrypted_content,
            } => {
                self.reasoning.push(ContentBlock::Reasoning {
                    id,
                    summary,
                    encrypted_content,
                });
                false
            }
            StreamEvent::Message(resp) => {
                self.explicit_message = Some(resp);
                false
            }
            StreamEvent::Done { finish, usage } => {
                self.finish = finish;
                self.usage = usage;
                self.saw_done = true;
                true
            }
            StreamEvent::Warning { .. } => {
                // Surfaced to the sink only — does not affect the
                // assembled response.
                false
            }
        }
    }

    pub(crate) fn finish(self) -> ChatResponse {
        if let Some(mut resp) = self.explicit_message {
            // If the stream produced an explicit Message event but
            // also Done with usage, prefer the Done usage (more
            // accurate per provider conventions for the streamed
            // path).
            if self.saw_done {
                resp.finish_reason = self.finish;
                resp.usage = self.usage;
            }
            return resp;
        }

        // Re-emit accumulated text first, then tool_use blocks in
        // the order their starts arrived.
        let mut content = self.reasoning;
        content.extend(self.tool_state);
        if !self.text.is_empty() {
            content.push(ContentBlock::Text { text: self.text });
        }
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        for (id, name) in &self.tool_use_starts {
            // Prefer the finalised value; fall back to parsing the
            // accumulated partial JSON; on parse failure use Null
            // (Anthropic's convention for empty input).
            let input = if let Some(v) = self.finalised_tools.get(id) {
                v.clone()
            } else {
                self.tool_input_partials
                    .get(id)
                    .and_then(|s| {
                        if s.is_empty() {
                            Some(serde_json::Value::Object(Default::default()))
                        } else {
                            serde_json::from_str(s).ok()
                        }
                    })
                    .unwrap_or(serde_json::Value::Null)
            };
            content.push(ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            });
            tool_calls.push(ToolCall {
                id: id.clone(),
                name: name.clone(),
                input,
            });
        }

        // If the stream had tool_use blocks but the provider didn't
        // emit Done with ToolUse finish reason, upgrade.
        let finish = if !tool_calls.is_empty() && matches!(self.finish, FinishReason::Stop) {
            FinishReason::ToolUse
        } else {
            self.finish
        };

        ChatResponse {
            model: self.model,
            content,
            tool_calls,
            finish_reason: finish,
            usage: self.usage,
        }
    }
}

/// Forwarder that swallows all events. Used as the default sink
/// when none is supplied.
pub fn null_sink() -> Arc<dyn StreamSink> {
    Arc::new(NullSink)
}

/// Convert a captured Vec<LlmError> driver error into the right
/// `Result` — used by callers that want to swallow stream-level
/// transport failures vs propagate them.
#[allow(dead_code)]
pub(crate) fn first_err(events: &[Result<StreamEvent>]) -> Option<&LlmError> {
    events.iter().find_map(|r| r.as_ref().err())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/accumulate.rs"
    ));
}
