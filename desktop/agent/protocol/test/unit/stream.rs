use super::*;

#[test]
fn every_event_has_a_round_trip_codec() {
    let events = [
        StreamEvent::TaskStarted(TaskStarted {
            task_id: "task-1".into(),
            session_id: Some("session-1".into()),
        }),
        StreamEvent::Delta(DeltaPayload::new("hello")),
        StreamEvent::ToolUseStart(ToolUseStartPayload {
            id: "tool-1".into(),
            name: "fs.read".into(),
        }),
        StreamEvent::ToolInputDelta(ToolInputDeltaPayload {
            id: "tool-1".into(),
            delta: r#"{"path":"#.into(),
        }),
        StreamEvent::ToolUse(ToolUsePayload {
            id: "tool-1".into(),
            name: "fs.read".into(),
            input: Some(serde_json::json!({"path": "/home/user"})),
        }),
        StreamEvent::ToolStart(ToolStartPayload {
            kind: Some("tool_start".into()),
            id: "tool-1".into(),
            name: "fs.read".into(),
            input: None,
        }),
        StreamEvent::ToolResult(ToolResultPayload {
            kind: Some("tool_result".into()),
            id: "tool-1".into(),
            name: "fs.read".into(),
            ok: Some(true),
            preview: None,
            output: None,
            content: None,
            text: None,
            is_error: None,
        }),
        StreamEvent::Warning(WarningPayload {
            message: "careful".into(),
        }),
        StreamEvent::TurnDone(TurnDonePayload {
            finish: Some("stop".into()),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 2,
                ..Usage::default()
            },
        }),
        StreamEvent::Done(DonePayload {
            event_type: "done".into(),
            task_id: "task-1".into(),
            session_id: Some("session-1".into()),
            answer: Some("hello".into()),
            response: Some("hello".into()),
            turns_used: Some(1),
            provider: Some("provider".into()),
            model: Some("model".into()),
        }),
        StreamEvent::Error(StreamError::new("failed")),
    ];

    for event in events {
        let decoded = StreamEvent::from_json(event.event_name(), &event.to_json().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(decoded, event);
    }
}

#[test]
fn golden_delta_and_done_shapes_remain_compatible() {
    assert_eq!(
        StreamEvent::Delta(DeltaPayload::new("hello"))
            .to_json()
            .unwrap(),
        r#"{"type":"delta","text":"hello"}"#
    );
    let done = StreamEvent::from_json(
        "done",
        r#"{"answer":"hello","task_id":"task-1","session_id":"session-1"}"#,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        done,
        StreamEvent::Done(DonePayload {
            answer: Some(answer),
            ..
        }) if answer == "hello"
    ));
}

#[test]
fn aliases_and_unknown_events_are_backward_compatible() {
    assert_eq!(
        StreamEvent::from_json("text", r#"{"delta":"legacy"}"#)
            .unwrap()
            .unwrap(),
        StreamEvent::Delta(DeltaPayload::new("legacy"))
    );
    assert_eq!(StreamEvent::from_json("future", "{}").unwrap(), None);
    let partial = StreamEvent::from_json(
        "tool_input_delta",
        r#"{"id":"tool-1","partial":"{}"}"#,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        partial,
        StreamEvent::ToolInputDelta(ToolInputDeltaPayload { delta, .. }) if delta == "{}"
    ));
}
