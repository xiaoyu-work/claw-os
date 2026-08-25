//! User-visible streaming projection.
//!
//! Provider events and tool progress carry complete inputs, outputs, and
//! evidence markers because the runtime needs them for tool execution,
//! memory, audit, and evidence verification. User-facing sinks receive this
//! projection instead: answer text without evidence markers, tool identity
//! without arguments, and lifecycle status without result bodies.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::agent::llm::accumulate::StreamSink;
use crate::agent::llm::{ContentBlock, StreamEvent};

use super::evidence;
use super::progress::ProgressSink;

const EVIDENCE_PREFIX: &str = "[evidence:";

pub fn user_visible_stream_sink(inner: Arc<dyn StreamSink>) -> Arc<dyn StreamSink> {
    Arc::new(UserVisibleStreamSink {
        inner,
        evidence_filter: Mutex::new(EvidenceMarkerFilter::default()),
    })
}

pub fn user_visible_progress_sink(inner: Arc<dyn ProgressSink>) -> Arc<dyn ProgressSink> {
    Arc::new(UserVisibleProgressSink { inner })
}

struct UserVisibleStreamSink {
    inner: Arc<dyn StreamSink>,
    evidence_filter: Mutex<EvidenceMarkerFilter>,
}

impl StreamSink for UserVisibleStreamSink {
    fn on_event(&self, event: &StreamEvent) {
        match event {
            StreamEvent::TextDelta { text } => {
                let visible = self
                    .evidence_filter
                    .lock()
                    .expect("evidence stream filter lock")
                    .push(text);
                if !visible.is_empty() {
                    self.inner
                        .on_event(&StreamEvent::TextDelta { text: visible });
                }
            }
            StreamEvent::ToolInputDelta { .. } => {}
            StreamEvent::ToolUse(call) => {
                let mut visible = call.clone();
                visible.input = Value::Null;
                self.inner.on_event(&StreamEvent::ToolUse(visible));
            }
            StreamEvent::Message(response) => {
                let mut visible = response.clone();
                for block in &mut visible.content {
                    match block {
                        ContentBlock::Text { text } => {
                            *text = evidence::strip_markers(text);
                        }
                        ContentBlock::ToolUse { input, .. } => {
                            *input = Value::Null;
                        }
                        _ => {}
                    }
                }
                for call in &mut visible.tool_calls {
                    call.input = Value::Null;
                }
                self.inner.on_event(&StreamEvent::Message(visible));
            }
            StreamEvent::Done { .. } => {
                let tail = self
                    .evidence_filter
                    .lock()
                    .expect("evidence stream filter lock")
                    .finish();
                if !tail.is_empty() {
                    self.inner
                        .on_event(&StreamEvent::TextDelta { text: tail });
                }
                self.inner.on_event(event);
            }
            _ => self.inner.on_event(event),
        }
    }
}

struct UserVisibleProgressSink {
    inner: Arc<dyn ProgressSink>,
}

impl ProgressSink for UserVisibleProgressSink {
    fn on_tool_start(&self, id: &str, name: &str, _input: &Value) {
        self.inner.on_tool_start(id, name, &Value::Null);
    }

    fn on_tool_result(
        &self,
        id: &str,
        name: &str,
        ok: bool,
        latency_ms: u64,
        bytes_returned: usize,
        _content_preview: &str,
    ) {
        self.inner
            .on_tool_result(id, name, ok, latency_ms, bytes_returned, "");
    }
}

#[derive(Default)]
struct EvidenceMarkerFilter {
    pending: String,
}

impl EvidenceMarkerFilter {
    fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let mut visible = String::new();

        loop {
            if let Some(start) = self.pending.find(EVIDENCE_PREFIX) {
                let marker_body = start + EVIDENCE_PREFIX.len();
                let Some(relative_end) = self.pending[marker_body..].find(']') else {
                    if start > 0 {
                        let mut retain_start = start;
                        while retain_start > 0
                            && matches!(
                                self.pending.as_bytes()[retain_start - 1],
                                b' ' | b'\t'
                            )
                        {
                            retain_start -= 1;
                        }
                        visible.push_str(&self.pending[..retain_start]);
                        self.pending.drain(..retain_start);
                    }
                    return visible;
                };
                visible.push_str(&self.pending[..start]);
                trim_horizontal_whitespace(&mut visible);
                let end = marker_body + relative_end + 1;
                self.pending.drain(..end);
                continue;
            }

            let prefix_bytes = longest_marker_prefix_suffix(&self.pending);
            let mut emit_end = self.pending.len().saturating_sub(prefix_bytes);
            while emit_end > 0
                && matches!(self.pending.as_bytes()[emit_end - 1], b' ' | b'\t')
            {
                emit_end -= 1;
            }
            visible.push_str(&self.pending[..emit_end]);
            self.pending.drain(..emit_end);
            return visible;
        }
    }

    fn finish(&mut self) -> String {
        let mut pending = std::mem::take(&mut self.pending);
        if let Some(start) = pending.find(EVIDENCE_PREFIX) {
            pending.truncate(start);
            trim_horizontal_whitespace(&mut pending);
            return pending;
        }
        let prefix_bytes = longest_marker_prefix_suffix(&pending);
        if prefix_bytes > 0 {
            pending.truncate(pending.len() - prefix_bytes);
            trim_horizontal_whitespace(&mut pending);
        }
        pending
    }
}

fn longest_marker_prefix_suffix(value: &str) -> usize {
    let max = value.len().min(EVIDENCE_PREFIX.len().saturating_sub(1));
    (1..=max)
        .rev()
        .find(|length| value.ends_with(&EVIDENCE_PREFIX[..*length]))
        .unwrap_or(0)
}

fn trim_horizontal_whitespace(value: &mut String) {
    while value.ends_with([' ', '\t']) {
        value.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{FinishReason, ToolCall, Usage};

    #[derive(Default)]
    struct CapturingStreamSink {
        events: Mutex<Vec<StreamEvent>>,
    }

    impl StreamSink for CapturingStreamSink {
        fn on_event(&self, event: &StreamEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
    }

    #[derive(Default)]
    struct CapturingProgressSink {
        input: Mutex<Option<Value>>,
        preview: Mutex<Option<String>>,
    }

    impl ProgressSink for CapturingProgressSink {
        fn on_tool_start(&self, _id: &str, _name: &str, input: &Value) {
            *self.input.lock().unwrap() = Some(input.clone());
        }

        fn on_tool_result(
            &self,
            _id: &str,
            _name: &str,
            _ok: bool,
            _latency_ms: u64,
            _bytes_returned: usize,
            content_preview: &str,
        ) {
            *self.preview.lock().unwrap() = Some(content_preview.to_string());
        }
    }

    #[test]
    fn split_evidence_markers_are_removed_from_visible_text() {
        let captured = Arc::new(CapturingStreamSink::default());
        let sink = user_visible_stream_sink(captured.clone());

        sink.on_event(&StreamEvent::TextDelta {
            text: "Network is fast [evi".into(),
        });
        sink.on_event(&StreamEvent::TextDelta {
            text: "dence:call-1 confidence=0.99] and stable.".into(),
        });
        sink.on_event(&StreamEvent::Done {
            finish: FinishReason::Stop,
            usage: Usage::default(),
        });

        let text = captured
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(text, "Network is fast and stable.");
    }

    #[test]
    fn tool_payloads_are_removed_from_visible_events() {
        let captured = Arc::new(CapturingStreamSink::default());
        let sink = user_visible_stream_sink(captured.clone());

        sink.on_event(&StreamEvent::ToolInputDelta {
            id: "call-1".into(),
            partial_json: r#"{"secret":"value"}"#.into(),
        });
        sink.on_event(&StreamEvent::ToolUse(ToolCall {
            id: "call-1".into(),
            name: "cos_sysinfo".into(),
            input: serde_json::json!({"secret": "value"}),
        }));

        let events = captured.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolUse(call) => {
                assert_eq!(call.name, "cos_sysinfo");
                assert!(call.input.is_null());
            }
            other => panic!("expected visible tool call, got {other:?}"),
        }
    }

    #[test]
    fn tool_progress_hides_inputs_and_result_previews() {
        let captured = Arc::new(CapturingProgressSink::default());
        let sink = user_visible_progress_sink(captured.clone());

        sink.on_tool_start(
            "call-1",
            "cos_sysinfo",
            &serde_json::json!({"path": "/private"}),
        );
        sink.on_tool_result(
            "call-1",
            "cos_sysinfo",
            true,
            12,
            128,
            "private result",
        );

        assert_eq!(*captured.input.lock().unwrap(), Some(Value::Null));
        assert_eq!(captured.preview.lock().unwrap().as_deref(), Some(""));
    }

    #[test]
    fn malformed_marker_is_hidden_when_stream_finishes() {
        let mut filter = EvidenceMarkerFilter::default();
        assert_eq!(filter.push("literal [evidence:unfinished"), "literal");
        assert_eq!(filter.finish(), "");
    }
}
