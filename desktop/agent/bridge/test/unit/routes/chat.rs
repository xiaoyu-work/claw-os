use super::*;

#[test]
fn typed_protocol_event_builds_an_sse_frame() {
    let _event = protocol_event(StreamEvent::Delta(DeltaPayload::new("hello")));
}

#[test]
fn image_attachments_are_forwarded_without_desktop_authority_fields() {
    let attachment = cos_agent_protocol::ChatAttachment::from_bytes(
        "screen.png".to_string(),
        b"\x89PNG\r\n\x1a\nfixture",
    )
    .unwrap();
    let request = ChatRequest {
        prompt: Some("inspect".to_string()),
        attachments: vec![attachment.clone()],
        session_id: Some("session-a".to_string()),
        ..ChatRequest::default()
    };
    let params = task_submit_params(&request, "inspect", request.session_id.as_deref()).unwrap();
    assert_eq!(
        params["attachments"][0],
        serde_json::to_value(attachment).unwrap()
    );
    assert_eq!(params["session_id"], "session-a");
    assert!(params.get("owner_uid").is_none());
    assert!(params.get("workspace").is_none());
}

#[test]
fn follow_up_uses_the_predecessors_session_instead_of_the_request_session() {
    let request = ChatRequest {
        prompt: Some("continue".to_string()),
        session_id: Some("caller-controlled".to_string()),
        ..ChatRequest::default()
    };
    let params =
        follow_up_submit_params(&request, "continue", "task-previous", "session-verified").unwrap();
    assert_eq!(params["session_id"], "session-verified");
    assert_eq!(params["after_task_id"], "task-previous");
}
