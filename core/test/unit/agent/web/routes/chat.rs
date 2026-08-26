use super::*;
use crate::agent::llm::ToolCall;

#[test]
fn user_facing_tool_events_omit_inputs_and_results() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = SseSink::new(tx);

    sink.on_event(&StreamEvent::ToolInputDelta {
        id: "call-1".into(),
        partial_json: r#"{"secret":"input"}"#.into(),
    });
    sink.on_event(&StreamEvent::ToolUse(ToolCall {
        id: "call-1".into(),
        name: "cos_sysinfo".into(),
        input: json!({"secret": "input"}),
    }));
    sink.on_tool_start(
        "call-1",
        "cos_sysinfo",
        &json!({"secret": "input"}),
    );
    sink.on_tool_result(
        "call-1",
        "cos_sysinfo",
        true,
        12,
        128,
        "secret result",
    );
    drop(sink);

    let mut output = String::new();
    while let Ok(frame) = rx.try_recv() {
        output.push_str(std::str::from_utf8(&frame.unwrap()).unwrap());
    }
    assert!(output.contains("cos_sysinfo"));
    assert!(!output.contains("secret"));
    assert!(!output.contains("\"input\""));
    assert!(!output.contains("preview"));
    assert!(!output.contains("tool_input_delta"));
}
