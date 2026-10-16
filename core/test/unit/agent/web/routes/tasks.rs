use super::*;

#[test]
fn follow_up_uses_only_the_owner_checked_predecessor_conversation() {
    let params = follow_up_params(
        &json!({
            "id": "job-predecessor",
            "session_id": "ses_0000000000001_000000000001",
            "owner_uid": 1000,
            "status": "running",
        }),
        FollowUpRequest {
            prompt: "Refine the answer".to_string(),
            attachments: Vec::new(),
            use_memory: true,
        },
    )
    .unwrap();

    assert_eq!(
        params,
        json!({
            "prompt": "Refine the answer",
            "session_id": "ses_0000000000001_000000000001",
            "after_task_id": "job-predecessor",
            "use_memory": true,
        })
    );
    assert!(params.get("owner_uid").is_none());
}

#[test]
fn follow_up_body_and_predecessor_shape_are_closed() {
    assert!(serde_json::from_value::<FollowUpRequest>(json!({
        "prompt": "Refine",
        "session_id": "caller-selected",
    }))
    .is_err());
    assert!(follow_up_params(
        &json!({ "id": "job-predecessor" }),
        FollowUpRequest {
            prompt: "Refine".to_string(),
            attachments: Vec::new(),
            use_memory: true,
        },
    )
    .is_err());
}

#[test]
fn queue_projection_excludes_current_foreign_and_terminal_jobs() {
    let view=json!({"jobs":[
        {"id":"current","session_id":"session-1","status":"running","prompt":"now","after_task_id":"earlier"},
        {"id":"next","session_id":"session-1","status":"pending","prompt":"then","after_task_id":"current","created_at":"2026-10-13T12:00:00Z"},
        {"id":"foreign","session_id":"session-other","status":"pending","prompt":"private","after_task_id":"another"},
        {"id":"cancelled","session_id":"session-1","status":"cancelled","prompt":"discarded","after_task_id":"current"}
    ]});
    let queue=queue_projection(&view,"session-1","current").unwrap();
    assert_eq!(queue.len(),1);
    assert_eq!(queue[0]["id"],"next");
    assert!(!serde_json::to_string(&queue).unwrap().contains("private"));
}
