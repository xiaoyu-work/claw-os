use super::super::tests::{job, MockBackend, SESSION, TASK};
use super::*;

fn backend() -> std::sync::Arc<MockBackend> {
    let backend = MockBackend::new();
    let mut job = job("waiting_approval");
    job["waiting_on"] = json!(["approval-1"]);
    backend.seed_job(job, Vec::new());
    backend.state.lock().unwrap().pending.push(json!({
        "id": "approval-1", "verb": "fs.write", "scope": { "path": "/home/claw/output" },
        "session": SESSION, "reason": "Write the requested output",
    }));
    backend
}

#[tokio::test]
async fn structured_response_requires_root_authorization_and_state_confirmation() {
    let backend = backend();
    let approvals = Approvals::default();
    let messages = approvals
        .present(
            backend.as_ref(),
            "thread-1",
            SESSION,
            TASK,
            &["approval-1".into()],
        )
        .await
        .unwrap();
    assert_eq!(messages[0]["method"], "item/tool/requestUserInput");
    assert!(backend.state.lock().unwrap().reviewed.is_empty());
    let id: RequestId = serde_json::from_value(messages[0]["id"].clone()).unwrap();
    let resolved = approvals
        .respond(
            backend.as_ref(),
            id.clone(),
            Ok(json!({
                "answers": { "approval-1": { "answers": ["Authorize once"] } },
            })),
        )
        .await
        .unwrap();
    assert_eq!(
        backend.state.lock().unwrap().reviewed,
        vec![("approval-1".into(), ReviewDecision::ApproveOnce)]
    );
    assert_eq!(resolved[0]["method"], "serverRequest/resolved");
    assert_eq!(resolved[0]["params"]["requestId"], json!(id));
    assert!(approvals
        .respond(backend.as_ref(), id, Ok(json!({})))
        .await
        .is_err());
}

#[tokio::test]
async fn stale_task_or_extra_answers_cannot_invoke_the_helper() {
    let backend = backend();
    let approvals = Approvals::default();
    for value in [
        json!({ "answers": { "approval-1": { "answers": ["Authorize once", "Deny"] } } }),
        json!({ "answers": { "different": { "answers": ["Authorize once"] } } }),
        json!({ "answers": { "approval-1": { "answers": ["Always"] } } }),
    ] {
        let messages = approvals
            .present(
                backend.as_ref(),
                "thread-1",
                SESSION,
                TASK,
                &["approval-1".into()],
            )
            .await
            .unwrap();
        let id = serde_json::from_value(messages[0]["id"].clone()).unwrap();
        assert!(approvals
            .respond(backend.as_ref(), id, Ok(value))
            .await
            .is_err());
    }
    let messages = approvals
        .present(
            backend.as_ref(),
            "thread-1",
            SESSION,
            TASK,
            &["approval-1".into()],
        )
        .await
        .unwrap();
    let id = serde_json::from_value(messages[0]["id"].clone()).unwrap();
    backend.state.lock().unwrap().jobs[0]["status"] = json!("cancelled");
    assert!(approvals
        .respond(
            backend.as_ref(),
            id,
            Ok(json!({
                "answers": { "approval-1": { "answers": ["Authorize once"] } },
            }))
        )
        .await
        .is_err());
    assert!(backend.state.lock().unwrap().reviewed.is_empty());
}

#[tokio::test]
async fn failed_authorization_never_emits_a_resolved_approval() {
    let backend = backend();
    backend.state.lock().unwrap().review_fails = true;
    let approvals = Approvals::default();
    let messages = approvals
        .present(
            backend.as_ref(),
            "thread-1",
            SESSION,
            TASK,
            &["approval-1".into()],
        )
        .await
        .unwrap();
    let id = serde_json::from_value(messages[0]["id"].clone()).unwrap();
    let result = approvals
        .respond(
            backend.as_ref(),
            id,
            Ok(json!({
                "answers": { "approval-1": { "answers": ["Authorize once"] } },
            })),
        )
        .await;
    assert!(result.is_err());
    assert!(!backend
        .state
        .lock()
        .unwrap()
        .decisions
        .contains_key("approval-1"));
}
