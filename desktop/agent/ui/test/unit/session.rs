use super::*;

fn summary(id: &str, title: &str, count: i64) -> SessionSummary {
    SessionSummary {
        id: id.into(),
        title: title.into(),
        last_ts_ms: Some(42),
        message_count: count,
    }
}

#[test]
fn remote_reconciliation_deduplicates_provisional_sessions() {
    crate::localize::localize();
    let mut state = SessionState::default();
    state.capture_provisional(0, Some("remote-1"));
    state.merge_remote(vec![summary("remote-1", "Recovered", 3)]);

    assert_eq!(state.iter().count(), 1);
    let session = state.active().unwrap();
    assert_eq!(session.title, "Recovered");
    assert_eq!(session.message_count, 3);
}

#[test]
fn history_reconciliation_ignores_system_rows_and_refreshes_markdown() {
    let mut state = SessionState::default();
    state.merge_remote(vec![summary("remote-1", "Remote", 2)]);
    state.apply_history(
        "remote-1",
        Ok(vec![
            HistoryMessage {
                role: "system".into(),
                text: "hidden".into(),
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
                ts_ms: 0,
            },
            HistoryMessage {
                role: "assistant".into(),
                text: "**visible**".into(),
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
                ts_ms: 1,
            },
        ]),
    );

    let remote = state.iter().find(|session| session.remote_id.as_deref() == Some("remote-1")).unwrap();
    assert_eq!(remote.messages.len(), 1);
    assert!(remote.messages[0].parsed_markdown.is_some());
}

#[test]
fn retry_and_branch_context_keep_recent_turns_within_limit() {
    crate::localize::localize();
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

    let mut state = SessionState::default();
    state.active_mut().unwrap().messages = vec![
        ChatMessage::user("first".into()),
        ChatMessage::assistant_streaming(),
        ChatMessage::user("second".into()),
    ];
    assert_eq!(state.retry_branch(1).unwrap().1, "first");
    assert_eq!(state.retry_branch(3).unwrap().1, "second");
}

#[test]
fn relative_labels_use_real_timestamps() {
    crate::localize::localize();
    assert_eq!(relative_time_label(1_000_000, 1_020_000), "now");
    let plain = |label: String| label.replace(['\u{2068}', '\u{2069}'], "");
    assert_eq!(plain(relative_time_label(1_000_000, 1_300_000)), "5m");
    assert_eq!(plain(relative_time_label(1_000_000, 8_200_000)), "2h");
}
