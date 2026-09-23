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
