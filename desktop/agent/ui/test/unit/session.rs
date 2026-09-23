use super::*;

fn summary(id: &str, title: &str, count: i64) -> SessionSummary {
    SessionSummary {
        id: id.into(),
        title: title.into(),
        last_ts_ms: Some(42),
        message_count: count,
        ..SessionSummary::default()
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
fn canonical_summary_updates_presentation_controls_and_can_select_a_fork() {
    crate::localize::localize();
    let mut state = SessionState::default();
    let mut original = summary("session-1", "Original", 2);
    original.manageable = true;
    state.merge_remote(vec![original]);

    let mut renamed = summary("session-1", "Renamed", 2);
    renamed.manageable = true;
    renamed.archived = true;
    let index = state.apply_remote_summary(renamed, false);
    let session = state.get(index).unwrap();
    assert_eq!(session.title, "Renamed");
    assert!(session.archived);
    assert!(session.manageable);
    assert!(!session.legacy);

    let mut fork = summary("session-2", "Renamed", 2);
    fork.manageable = true;
    let fork_index = state.apply_remote_summary(fork, true);
    assert_eq!(state.active_index(), fork_index);
    assert_eq!(
        state
            .active()
            .and_then(|session| session.remote_id.as_deref()),
        Some("session-2")
    );
}

#[test]
fn history_reconciliation_ignores_system_rows_and_refreshes_markdown() {
    let mut state = SessionState::default();
    state.merge_remote(vec![summary("remote-1", "Remote", 2)]);
    state.apply_history(
        "remote-1",
        Ok(HistoryResponse {
            session_id: "remote-1".into(),
            messages: vec![
                HistoryMessage {
                    role: "system".into(),
                    text: "hidden".into(),
                    ..HistoryMessage::default()
                },
                HistoryMessage {
                    role: "assistant".into(),
                    text: "**visible**".into(),
                    ts_ms: 1,
                    ..HistoryMessage::default()
                },
            ],
            ..HistoryResponse::default()
        }),
    );

    let remote = state
        .iter()
        .find(|session| session.remote_id.as_deref() == Some("remote-1"))
        .unwrap();
    assert_eq!(remote.messages.len(), 1);
    assert!(remote.messages[0].parsed_markdown.is_some());
}

#[test]
fn active_history_rebuilds_one_task_and_preserves_the_bound_user_prompt() {
    let mut state = SessionState::default();
    state.apply_remote_summary(summary("remote-1", "Remote", 3), true);
    let reattach = state
        .apply_history(
            "remote-1",
            Ok(HistoryResponse {
                session_id: "remote-1".into(),
                messages: vec![
                    HistoryMessage {
                        id: 1,
                        role: "user".into(),
                        text: "Continue after reconnect".into(),
                        task_id: Some("job-live".into()),
                        is_user_prompt: Some(true),
                        ..HistoryMessage::default()
                    },
                    HistoryMessage {
                        id: 2,
                        role: "assistant".into(),
                        task_id: Some("job-live".into()),
                        tool_calls: vec![ToolCallView {
                            id: "duplicate".into(),
                            name: "old".into(),
                            input: serde_json::Value::Null,
                            partial_json: String::new(),
                            in_progress: false,
                        }],
                        ..HistoryMessage::default()
                    },
                ],
                jobs: vec![
                    ConversationJob {
                        id: "job-live".into(),
                        status: "running".into(),
                        prompt: "Continue after reconnect".into(),
                        session_id: "remote-1".into(),
                        created_at: "2026-09-22T12:00:00Z".into(),
                    },
                    ConversationJob {
                        id: "job-next".into(),
                        status: "pending".into(),
                        prompt: "Then summarize".into(),
                        session_id: "remote-1".into(),
                        created_at: "2026-09-22T12:01:00Z".into(),
                    },
                ],
                task_bindings_complete: true,
                ..HistoryResponse::default()
            }),
        )
        .expect("active task");
    assert_eq!(reattach.id, "job-live");
    assert_eq!(reattach.tail_id, "job-next");
    assert_eq!(reattach.queued_after, 1);

    let session = state
        .iter()
        .find(|session| session.remote_id.as_deref() == Some("remote-1"))
        .unwrap();
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].content, "Continue after reconnect");
    assert!(session.messages[1].in_progress);
    assert!(session.messages[1].tool_calls.is_empty());
}

#[test]
fn active_history_uses_the_job_prompt_when_the_bound_row_is_not_persisted_yet() {
    let mut state = SessionState::default();
    state.apply_remote_summary(summary("remote-1", "Remote", 0), true);
    let reattach = state
        .apply_history(
            "remote-1",
            Ok(HistoryResponse {
                session_id: "remote-1".into(),
                jobs: vec![ConversationJob {
                    id: "job-live".into(),
                    status: "pending".into(),
                    prompt: "Persist me once".into(),
                    session_id: "remote-1".into(),
                    created_at: "2026-09-22T12:00:00Z".into(),
                }],
                task_bindings_complete: true,
                ..HistoryResponse::default()
            }),
        )
        .expect("pending task");

    assert_eq!(reattach.id, "job-live");
    let session = state.active().unwrap();
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].content, "Persist me once");
    assert_eq!(session.messages[0].role(), ChatRole::User);
    assert!(session.messages[1].in_progress);
}

#[test]
fn active_history_refuses_incomplete_task_bindings() {
    let mut state = SessionState::default();
    state.apply_remote_summary(summary("remote-1", "Remote", 1), true);
    let reattach = state.apply_history(
        "remote-1",
        Ok(HistoryResponse {
            session_id: "remote-1".into(),
            jobs: vec![ConversationJob {
                id: "job-live".into(),
                status: "running".into(),
                prompt: "Do not guess".into(),
                session_id: "remote-1".into(),
                created_at: "2026-09-22T12:00:00Z".into(),
            }],
            task_bindings_complete: false,
            task_bindings_error: Some("verified bindings unavailable".into()),
            ..HistoryResponse::default()
        }),
    );

    assert!(reattach.is_none());
    assert!(matches!(
        &state.active().unwrap().history,
        HistoryState::Failed(error) if error == "verified bindings unavailable"
    ));
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
