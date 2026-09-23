use super::*;

#[test]
fn canonical_conversation_summary_keeps_presentation_controls() {
    let result = summary(
        Conversation {
            id: "ses_0000000000001_000000000001".to_string(),
            presentation_id: "078ed458-0e17-882b-b0aa-ca1088683b25".to_string(),
            title: "Release review".to_string(),
            created_at: "2026-09-22T12:00:00Z".to_string(),
            updated_at: "2026-09-22T12:30:00Z".to_string(),
            archived: true,
            deleted: false,
            parent_id: Some("ses_0000000000000_000000000001".to_string()),
            messages: Vec::new(),
            message_count: 7,
            jobs: Vec::new(),
            jobs_truncated: false,
            task_bindings_complete: true,
            task_bindings_error: None,
        },
        true,
        false,
    );
    assert!(result.is_ok());
    let summary = result.ok().unwrap();

    assert!(summary.manageable);
    assert!(summary.archived);
    assert!(!summary.legacy);
    assert_eq!(summary.message_count, 7);
    assert!(summary.last_ts_ms.is_some_and(|timestamp| timestamp > 0));
    assert_eq!(
        summary.parent_id.as_deref(),
        Some("ses_0000000000000_000000000001")
    );
}

#[test]
fn malformed_conversation_identity_and_time_are_not_invented() {
    let result = parse_conversation(json!({ "conversation": { "id": "missing fields" } }));
    assert!(result.is_err());

    let result = summary(
        Conversation {
            id: "session".to_string(),
            presentation_id: "presentation".to_string(),
            title: "Title".to_string(),
            created_at: "2026-09-22T12:00:00Z".to_string(),
            updated_at: "not-a-time".to_string(),
            archived: false,
            deleted: false,
            parent_id: None,
            messages: Vec::new(),
            message_count: 0,
            jobs: Vec::new(),
            jobs_truncated: false,
            task_bindings_complete: true,
            task_bindings_error: None,
        },
        true,
        false,
    );
    assert!(result.is_err());
}
