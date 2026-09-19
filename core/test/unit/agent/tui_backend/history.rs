use super::super::tests::{
    conversation, info, job, MockBackend, FORK_FRONTEND, FRONTEND, SESSION, TASK,
};
use super::*;

#[test]
fn aliases_come_from_canonical_metadata_not_adapter_derivation() {
    let view = thread_view(&conversation(), &info(), true, None, Vec::new()).unwrap();
    assert_eq!(view["sessionId"], SESSION);
    assert_eq!(view["id"], FRONTEND);
    assert_eq!(view["path"], Value::Null);
    let mut changed = conversation();
    changed["presentation_id"] = json!(FORK_FRONTEND);
    let view = thread_view(&changed, &info(), true, None, Vec::new()).unwrap();
    assert_eq!(view["sessionId"], SESSION);
    assert_eq!(view["id"], FORK_FRONTEND);
}

#[test]
fn missing_or_invalid_presentation_metadata_never_mints_a_local_alias() {
    let mut conversation = conversation();
    conversation
        .as_object_mut()
        .unwrap()
        .remove("presentation_id");
    assert_eq!(frontend_id(&conversation).unwrap_err().code, -32005);
    conversation["presentation_id"] = json!(SESSION);
    assert!(frontend_id(&conversation).is_err());
}

#[tokio::test]
async fn history_retains_actual_job_status_and_task_ids() {
    let backend = MockBackend::new();
    backend.seed_job(job("error"), Vec::new());
    let turn = hydrate(backend.as_ref(), FRONTEND, &job("error"), "full")
        .await
        .unwrap();
    assert_eq!(turn["id"], TASK);
    assert_eq!(turn["status"], "failed");
    assert_eq!(turn["error"]["message"], "The provider failed");
    assert_eq!(turn["items"][0]["id"], format!("{TASK}:user"));
}

#[test]
fn backtrack_requires_an_unambiguous_canonical_user_turn_boundary() {
    let mut conversation = conversation();
    conversation["messages"] = json!([
        { "role": "user", "text": "Hello Claw", "tool_results": [], "task_id": TASK },
        { "role": "assistant", "text": "Hello", "tool_results": [] },
    ]);
    assert!(task_user_turn_range(&conversation, &[job("ok")], TASK).is_err());
    conversation["messages"][0]["text"] = json!("Different original");
    assert!(task_user_turn_range(&conversation, &[job("ok")], TASK).is_err());
    conversation["messages_truncated"] = json!(true);
    assert!(task_user_turn_range(&conversation, &[job("ok")], TASK).is_err());
}

#[tokio::test]
async fn foreign_job_history_is_not_projected_as_the_current_thread() {
    let backend = MockBackend::new();
    let mut conversation = conversation();
    let mut foreign = job("ok");
    foreign["session_id"] = json!("ses_0018e23f14a01_b1b2c3d4e5f6");
    conversation["jobs"] = json!([foreign]);
    assert!(jobs(backend.as_ref(), &conversation).await.is_err());
}

#[test]
fn text_and_time_agreement_cannot_establish_retained_task_history() {
    let mut conversation = conversation();
    assert!(verify_visible_history(&conversation, &[job("ok")]).is_err());
    conversation["messages"] = json!([{
        "role": "user", "text": "Hello Claw", "tool_results": [],
        "ts_ms": 1,
    }]);
    assert!(verify_visible_history(&conversation, &[job("ok")]).is_err());
    conversation["messages"][0]["ts_ms"] =
        json!(chrono::DateTime::parse_from_rfc3339("2026-09-13T12:00:01Z")
            .unwrap()
            .timestamp_millis());
    assert!(verify_visible_history(&conversation, &[job("ok")]).is_err());
}
