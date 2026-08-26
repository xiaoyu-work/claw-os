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
