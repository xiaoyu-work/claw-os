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
    use super::*;
    use futures_util::stream;
    use std::sync::Mutex;

    fn s(events: Vec<Result<StreamEvent>>) -> BoxStream<'static, Result<StreamEvent>> {
        Box::pin(stream::iter(events))
    }

    #[derive(Default)]
    struct CountingSink {
        events: Mutex<Vec<StreamEvent>>,
    }
    impl StreamSink for CountingSink {
        fn on_event(&self, event: &StreamEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn assembles_text_only_response() {
        let sink: Arc<CountingSink> = Arc::default();
        let stream = s(vec![
            Ok(StreamEvent::TextDelta {
                text: "Hello".into(),
            }),
            Ok(StreamEvent::TextDelta {
                text: " world".into(),
            }),
            Ok(StreamEvent::Done {
                finish: FinishReason::Stop,
                usage: Usage {
                    input_tokens: 5,
                    output_tokens: 2,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
            }),
        ]);
        let resp = rt()
            .block_on(accumulate_stream(stream, sink.clone(), "test-model"))
            .unwrap();
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello world"),
            other => panic!("expected Text block, got {other:?}"),
        }
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 2);
        assert!(matches!(resp.finish_reason, FinishReason::Stop));

        // Sink saw all 3 events.
        let seen = sink.events.lock().unwrap();
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn assembles_tool_use_with_streamed_input() {
        let sink = Arc::new(NullSink);
        let stream = s(vec![
            Ok(StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "echo".into(),
            }),
            Ok(StreamEvent::ToolInputDelta {
                id: "t1".into(),
                partial_json: "{\"text\":\"hi".into(),
            }),
            Ok(StreamEvent::ToolInputDelta {
                id: "t1".into(),
                partial_json: "\"}".into(),
            }),
            Ok(StreamEvent::Done {
                finish: FinishReason::ToolUse,
                usage: Usage::default(),
            }),
        ]);
        let resp = rt()
            .block_on(accumulate_stream(stream, sink, "test-model"))
            .unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "echo");
        assert_eq!(resp.tool_calls[0].input, serde_json::json!({"text": "hi"}));
        assert!(matches!(resp.finish_reason, FinishReason::ToolUse));
    }

    #[test]
    fn preserves_responses_reasoning_before_tool_use() {
        let sink = Arc::new(NullSink);
        let stream = s(vec![
            Ok(StreamEvent::Reasoning {
                id: "rs_1".into(),
                summary: vec!["Need to inspect the file.".into()],
                encrypted_content: Some("opaque-ciphertext".into()),
            }),
            Ok(StreamEvent::ToolState {
                tool_use_id: "call_1".into(),
                thought_signature: "opaque-thought-signature".into(),
            }),
            Ok(StreamEvent::ToolUse(ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "/tmp/a"}),
            })),
            Ok(StreamEvent::Done {
                finish: FinishReason::ToolUse,
                usage: Usage::default(),
            }),
        ]);
        let response = rt()
            .block_on(accumulate_stream(stream, sink, "gpt-5.6-sol"))
            .unwrap();

        assert!(matches!(
            &response.content[0],
            ContentBlock::Reasoning {
                id,
                encrypted_content: Some(content),
                ..
            } if id == "rs_1" && content == "opaque-ciphertext"
        ));
        assert!(matches!(
            &response.content[1],
            ContentBlock::ToolState {
                tool_use_id,
                thought_signature,
            } if tool_use_id == "call_1" && thought_signature == "opaque-thought-signature"
        ));
        assert!(matches!(
            &response.content[2],
            ContentBlock::ToolUse { id, .. } if id == "call_1"
        ));
    }

    #[test]
    fn final_tool_use_event_overrides_partial_json() {
        let sink = Arc::new(NullSink);
        let stream = s(vec![
            Ok(StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "echo".into(),
            }),
            Ok(StreamEvent::ToolInputDelta {
                id: "t1".into(),
                partial_json: "incomplete".into(),
            }),
            Ok(StreamEvent::ToolUse(ToolCall {
                id: "t1".into(),
                name: "echo".into(),
                input: serde_json::json!({"final": true}),
            })),
            Ok(StreamEvent::Done {
                finish: FinishReason::ToolUse,
                usage: Usage::default(),
            }),
        ]);
        let resp = rt()
            .block_on(accumulate_stream(stream, sink, "test-model"))
            .unwrap();
        assert_eq!(resp.tool_calls[0].input, serde_json::json!({"final": true}));
    }

    #[test]
    fn explicit_message_short_circuits_block_assembly() {
        let sink = Arc::new(NullSink);
        let mut explicit = ChatResponse {
            model: "explicit-model".into(),
            content: vec![ContentBlock::Text {
                text: "from message".into(),
            }],
            tool_calls: vec![],
            finish_reason: FinishReason::Length,
            usage: Usage {
                input_tokens: 100,
                ..Default::default()
            },
        };
        // Provider sends both Message and (less-trusted) text deltas.
        let stream = s(vec![
            Ok(StreamEvent::TextDelta {
                text: "noise".into(),
            }),
            Ok(StreamEvent::Message(explicit.clone())),
            Ok(StreamEvent::Done {
                finish: FinishReason::Stop,
                usage: Usage {
                    input_tokens: 200, // overrides Message's usage
                    ..Default::default()
                },
            }),
        ]);
        let resp = rt()
            .block_on(accumulate_stream(stream, sink, "test-model"))
            .unwrap();
        // Adopted from explicit Message, NOT assembled from deltas.
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "from message"),
            _ => panic!(),
        }
        // But Done overrides Message's usage and finish.
        assert_eq!(resp.usage.input_tokens, 200);
        assert!(matches!(resp.finish_reason, FinishReason::Stop));
        // Mute "unused mut" — explicit was prepared up front for clarity.
        explicit.usage.input_tokens = 0;
    }

    #[test]
    fn done_with_tool_use_finish_reason_propagates() {
        let sink = Arc::new(NullSink);
        let stream = s(vec![
            Ok(StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "echo".into(),
            }),
            Ok(StreamEvent::Done {
                finish: FinishReason::ToolUse,
                usage: Usage::default(),
            }),
        ]);
        let resp = rt()
            .block_on(accumulate_stream(stream, sink, "test-model"))
            .unwrap();
        assert!(matches!(resp.finish_reason, FinishReason::ToolUse));
    }

    #[test]
    fn missing_done_with_tool_use_blocks_upgrades_finish_reason() {
        // Provider streamed a tool_use start but never issued Done
        // — accumulator's default `Stop` should be upgraded so the
        // outer loop continues to dispatch tools.
        let sink = Arc::new(NullSink);
        let stream = s(vec![Ok(StreamEvent::ToolUseStart {
            id: "t1".into(),
            name: "echo".into(),
        })]);
        let resp = rt()
            .block_on(accumulate_stream(stream, sink, "test-model"))
            .unwrap();
        assert!(matches!(resp.finish_reason, FinishReason::ToolUse));
        assert_eq!(resp.tool_calls.len(), 1);
    }

    #[test]
    fn warning_events_are_forwarded_to_sink_but_dont_appear_in_response() {
        let sink: Arc<CountingSink> = Arc::default();
        let stream = s(vec![
            Ok(StreamEvent::Warning {
                message: "rate limit nearing".into(),
            }),
            Ok(StreamEvent::TextDelta { text: "ok".into() }),
            Ok(StreamEvent::Done {
                finish: FinishReason::Stop,
                usage: Usage::default(),
            }),
        ]);
        let resp = rt()
            .block_on(accumulate_stream(stream, sink.clone(), "test-model"))
            .unwrap();
        assert_eq!(resp.content.len(), 1);
        let seen = sink.events.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(matches!(seen[0], StreamEvent::Warning { .. }));
    }

    #[test]
    fn first_error_propagates_and_terminates() {
        let sink: Arc<CountingSink> = Arc::default();
        let stream = s(vec![
            Ok(StreamEvent::TextDelta {
                text: "partial".into(),
            }),
            Err(LlmError::Stream("upstream gone".into())),
            // Should never be observed by the sink (stream short-circuits).
            Ok(StreamEvent::Done {
                finish: FinishReason::Stop,
                usage: Usage::default(),
            }),
        ]);
        let res = rt().block_on(accumulate_stream(stream, sink.clone(), "test-model"));
        assert!(matches!(res, Err(LlmError::Stream(_))));
        let seen = sink.events.lock().unwrap();
        // Sink got 1 successful event before the error terminated drain.
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn out_of_order_tool_input_delta_is_recorded() {
        // ToolInputDelta arrives BEFORE its ToolUseStart — accumulator
        // should still credit the partial JSON to the right id.
        let sink = Arc::new(NullSink);
        let stream = s(vec![
            Ok(StreamEvent::ToolInputDelta {
                id: "t1".into(),
                partial_json: "{\"x\":1}".into(),
            }),
            Ok(StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "echo".into(),
            }),
            Ok(StreamEvent::Done {
                finish: FinishReason::ToolUse,
                usage: Usage::default(),
            }),
        ]);
        let resp = rt()
            .block_on(accumulate_stream(stream, sink, "test-model"))
            .unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].input, serde_json::json!({"x": 1}));
    }

    #[test]
    fn tool_use_without_explicit_start_still_lands_in_response() {
        // Bedrock can send ToolUse(ToolCall) directly without a
        // preceding ToolUseStart. The accumulator must auto-register.
        let sink = Arc::new(NullSink);
        let stream = s(vec![
            Ok(StreamEvent::ToolUse(ToolCall {
                id: "t1".into(),
                name: "echo".into(),
                input: serde_json::json!({"a": 1}),
            })),
            Ok(StreamEvent::Done {
                finish: FinishReason::ToolUse,
                usage: Usage::default(),
            }),
        ]);
        let resp = rt()
            .block_on(accumulate_stream(stream, sink, "test-model"))
            .unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "echo");
    }
}
