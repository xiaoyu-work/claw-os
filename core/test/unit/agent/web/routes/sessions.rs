use super::*;

fn metadata(id: &str, title: &str) -> Value {
    json!({
        "id": id,
        "presentation_id": "078ed458-0e17-882b-b0aa-ca1088683b25",
        "title": title,
        "created_at": "2026-09-22T12:00:00Z",
        "updated_at": "2026-09-22T12:30:00Z",
        "archived": false,
        "deleted": false,
    })
}

#[test]
fn list_projection_preserves_canonical_conversation_metadata() {
    let projected = project_list(
        json!({
            "conversations": [metadata("ses_0000000000001_000000000001", "Release review")],
            "conversation_count": 3,
            "conversations_truncated": true,
        }),
        Some(json!({
            "n": 2,
            "sessions": [
                {
                    "id": "ses_0000000000001_000000000001",
                    "title": "Generated title",
                    "last_ts_ms": 100,
                    "message_count": 2,
                },
                {
                    "id": "legacy-session",
                    "title": "Previous conversation",
                    "last_ts_ms": 90,
                    "message_count": 4,
                }
            ],
        })),
    )
    .unwrap();

    assert_eq!(projected["n"], 2);
    assert_eq!(projected["total"], 4);
    assert_eq!(projected["truncated"], true);
    assert_eq!(
        projected["sessions"][0]["id"],
        "ses_0000000000001_000000000001"
    );
    assert_eq!(projected["sessions"][0]["title"], "Release review");
    assert_eq!(projected["sessions"][0]["manageable"], true);
    assert_eq!(projected["sessions"][1]["id"], "legacy-session");
    assert_eq!(projected["sessions"][1]["manageable"], false);
    assert_eq!(projected["sessions"][1]["legacy"], true);
}

#[test]
fn conversation_projection_keeps_bounded_history_metadata() {
    let conversation = parse_conversation(json!({
        "conversation": {
            "id": "ses_0000000000001_000000000001",
            "presentation_id": "078ed458-0e17-882b-b0aa-ca1088683b25",
            "title": "Release review",
            "created_at": "2026-09-22T12:00:00Z",
            "updated_at": "2026-09-22T12:30:00Z",
            "archived": false,
            "deleted": false,
            "messages": [{"id": 1, "role": "user", "text": "Review it"}],
            "message_count": 7,
            "messages_truncated": true,
            "jobs": [{
                "id": "job-live",
                "status": "running",
                "prompt": "Continue after refresh",
                "created_at": "2026-09-22T12:30:00Z",
                "session_id": "ses_0000000000001_000000000001",
            }],
            "job_count": 1,
            "jobs_truncated": false,
            "task_bindings_complete": true,
        }
    }))
    .unwrap();

    assert_eq!(conversation.metadata.title, "Release review");
    assert_eq!(conversation.messages.len(), 1);
    assert_eq!(conversation.message_count, 7);
    assert!(conversation.messages_truncated);
    assert_eq!(conversation.jobs.len(), 1);
    assert_eq!(conversation.jobs[0].id, "job-live");
    assert!(conversation.task_bindings_complete);
}

#[test]
fn malformed_broker_responses_and_update_fields_are_rejected() {
    let error = project_list(json!({ "sessions": [] }), None).unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_GATEWAY);

    let error = project_list(
        json!({
            "conversations": [],
            "conversation_count": 0,
            "conversations_truncated": false,
        }),
        Some(json!({ "sessions": "not a list" })),
    )
    .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_GATEWAY);

    assert!(serde_json::from_value::<UpdateBody>(json!({
        "title": "Allowed",
        "owner_uid": 1000,
    }))
    .is_err());
}
