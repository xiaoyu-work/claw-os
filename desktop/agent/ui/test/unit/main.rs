use super::*;

fn init_i18n() {
    localize::localize();
}

#[test]
fn relative_labels_use_real_timestamps() {
    init_i18n();
    assert_eq!(relative_time_label(1_000_000, 1_020_000), "now");
    let plain = |label: String| label.replace('\u{2068}', "").replace('\u{2069}', "");
    assert_eq!(plain(relative_time_label(1_000_000, 1_300_000)), "5m");
    assert_eq!(plain(relative_time_label(1_000_000, 8_200_000)), "2h");
}

#[test]
fn provider_label_prefers_bridge_label() {
    init_i18n();
    let models = ModelsResponse {
        ready: true,
        provider: "anthropic".into(),
        model: "claude".into(),
        label: "Claude".into(),
        models: Vec::new(),
    };
    assert_eq!(provider_model_label(&models), "Claude");
}

#[test]
fn tool_events_dedupe_by_id() {
    let mut message = ChatMessage::assistant_streaming();
    upsert_tool_call(
        &mut message,
        ToolCallView {
            id: "one".into(),
            name: "first".into(),
            input: serde_json::Value::Null,
            partial_json: String::new(),
            in_progress: true,
        },
    );
    upsert_tool_call(
        &mut message,
        ToolCallView {
            id: "one".into(),
            name: "updated".into(),
            input: serde_json::json!({"ok": true}),
            partial_json: String::new(),
            in_progress: false,
        },
    );
    assert_eq!(message.tool_calls.len(), 1);
    assert_eq!(message.tool_calls[0].name, "updated");
}

#[test]
fn retry_selects_latest_user_prompt() {
    init_i18n();
    let messages = vec![
        ChatMessage::user("first".into()),
        ChatMessage::assistant_streaming(),
        ChatMessage::user("second".into()),
    ];
    let first = retry_branch(&messages, 1, "Session").unwrap();
    assert_eq!(first.1, "first");
    let second = retry_branch(&messages, messages.len(), "Session").unwrap();
    assert_eq!(second.1, "second");
    assert!(second.2.contains("User: first"));
}

#[test]
fn branch_context_keeps_recent_turns_within_limit() {
    let mut messages = Vec::new();
    for index in 0..20 {
        messages.push(ChatMessage::user(format!(
            "turn-{index} {}",
            "x".repeat(3_000)
        )));
    }
    let context = build_branch_context(&messages).unwrap();
    assert!(context.chars().count() <= MAX_BRANCH_CONTEXT_CHARS);
    assert!(context.contains("turn-19"));
    assert!(!context.contains("turn-0 "));
}

#[test]
fn voice_generation_rejects_stale_completion() {
    let state = VoiceState::Processing { generation: 8 };
    assert!(accept_voice_completion(8, &state, 8));
    assert!(!accept_voice_completion(8, &state, 7));
    assert!(!accept_voice_completion(9, &state, 8));
}
