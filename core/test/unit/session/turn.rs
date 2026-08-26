use super::*;

#[test]
fn text_constructor_defaults() {
    let t = Turn::text(TurnRole::User, "hello");
    assert_eq!(t.role, TurnRole::User);
    assert_eq!(t.content, "hello");
    assert_eq!(t.seq, 0);
    assert!(t.at.is_empty());
    assert!(t.tool_calls.is_empty());
}

#[test]
fn role_serializes_kebab() {
    assert_eq!(serde_json::to_string(&TurnRole::User).unwrap(), "\"user\"");
    assert_eq!(serde_json::to_string(&TurnRole::Assistant).unwrap(), "\"assistant\"");
    assert_eq!(serde_json::to_string(&TurnRole::Tool).unwrap(), "\"tool\"");
}

#[test]
fn stamp_default_time_only_when_empty() {
    let mut t = Turn::text(TurnRole::User, "a");
    t.stamp_default_time();
    assert!(!t.at.is_empty());
    let kept = t.at.clone();
    t.stamp_default_time();
    assert_eq!(t.at, kept, "second stamp must be a no-op");
}

#[test]
fn round_trip_with_tool_calls() {
    let t = Turn {
        seq: 7,
        at: "2026-01-01T00:00:00Z".into(),
        role: TurnRole::Assistant,
        content: "let me check".into(),
        runtime: Some("cos-agent".into()),
        tool_calls: vec![serde_json::json!({
            "id": "call_1",
            "name": "fs.read",
            "arguments": { "path": "/etc/hosts" }
        })],
        tool_call_id: None,
        usage: Some(serde_json::json!({
            "input_tokens": 12,
            "output_tokens": 4
        })),
    };
    let json = serde_json::to_string(&t).unwrap();
    let back: Turn = serde_json::from_str(&json).unwrap();
    assert_eq!(t, back);
}

#[test]
fn unknown_runtime_can_round_trip_minimal_turn() {
    // Simulate a minimal turn a non-claw runtime might produce.
    let raw = r#"{"role":"user","content":"hi"}"#;
    let t: Turn = serde_json::from_str(raw).unwrap();
    assert_eq!(t.role, TurnRole::User);
    assert_eq!(t.content, "hi");
    // Optional fields default cleanly.
    assert_eq!(t.seq, 0);
    assert!(t.runtime.is_none());
    assert!(t.tool_calls.is_empty());
}
