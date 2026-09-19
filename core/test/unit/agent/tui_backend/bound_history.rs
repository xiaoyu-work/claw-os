use std::sync::Arc;

use serde_json::{json, Value};

use super::backend::Operation;
use super::tests::{
    bound_projection, conversation, initialized, job, MockBackend, FORK_FRONTEND, FORK_SESSION,
    FRONTEND, SESSION, TASK,
};
use super::{history, Options};

const SECOND: &str = "00000000-0000-4000-8000-000000000002";
const THIRD: &str = "00000000-0000-4000-8000-000000000003";
const PARENT_ACTIVE: &str = "00000000-0000-4000-8000-000000000004";
const CHILD_ACTIVE: &str = "00000000-0000-4000-8000-000000000005";

fn backend() -> Arc<MockBackend> {
    let backend = MockBackend::new();
    let mut view = conversation();
    view["task_bindings_complete"] = json!(true);
    view["history_revision"] = json!("initial-revision");
    backend.state.lock().unwrap().conversations[0] = view;
    backend
}

fn record(backend: &MockBackend, session: &str, task_id: &str, row_start: i64, attempts: usize) {
    let answer = format!("answer-for-{task_id}");
    let mut task = job("ok");
    task["id"] = json!(task_id);
    task["session_id"] = json!(session);
    task["response"] = json!(answer);
    // Membership order, not these deliberately reversed timestamps, orders turns.
    task["created_at"] = json!(if task_id == TASK {
        "2026-09-13T14:00:00Z"
    } else {
        "2026-09-13T12:00:00Z"
    });
    task["started_at"] = task["created_at"].clone();
    task["finished_at"] = json!(if task_id == TASK {
        "2026-09-13T14:00:02Z"
    } else {
        "2026-09-13T12:00:02Z"
    });
    let mut state = backend.state.lock().unwrap();
    state.jobs.push(task);
    state.events.insert(task_id.to_string(), vec![
        json!({ "event": { "kind": "text_delta", "text": answer } }),
        json!({ "event": { "kind": "done", "finish": "stop", "usage": { "input_tokens": 1, "output_tokens": 2 } } }),
    ]);
    let view = state
        .conversations
        .iter_mut()
        .find(|view| view["id"] == session)
        .unwrap();
    let rows = view["messages"].as_array_mut().unwrap();
    for attempt in 0..attempts {
        let outer = row_start + (attempt as i64) * 2;
        for (row_id, is_user) in [(outer, true), (outer + 1, false)] {
            rows.push(json!({
                "id": row_id,
                "role": "user",
                "text": "Literal [tool_result] text does not determine membership",
                "content": "Unrelated to the submitted prompt or journal display",
                "tool_calls": [],
                "tool_results": if is_user { json!([{"is_error": false}]) } else { json!([]) },
                "ts_ms": 1,
                "task_id": task_id,
                "source_session_id": session,
                "source_message_id": row_id,
                "source_user_message_id": outer,
                "is_user_prompt": is_user,
            }));
        }
    }
    view["history_revision"] = json!(format!("recorded-{task_id}"));
}

fn view(backend: &MockBackend, session: &str) -> Value {
    let state = backend.state.lock().unwrap();
    bound_projection(
        state
            .conversations
            .iter()
            .find(|view| view["id"] == session)
            .unwrap(),
        &state.jobs,
    )
    .unwrap()
}

#[tokio::test]
async fn new_bound_history_uses_retained_order_and_actual_source_task_ids() {
    let backend = backend();
    record(&backend, SESSION, TASK, 1, 1);
    record(&backend, SESSION, SECOND, 10, 1);
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let result = connection
        .execute("thread/resume", json!({ "threadId": FRONTEND }))
        .await
        .unwrap();
    let turns = result.result["thread"]["turns"].as_array().unwrap();
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [TASK, SECOND]
    );
    assert_eq!(turns[0]["status"], "completed");
    assert_eq!(turns[0]["items"][1]["text"], format!("answer-for-{TASK}"));
    assert_eq!(backend.count(Operation::TaskSubmit), 0);
}

#[tokio::test]
async fn fork_replays_inherited_source_journals_without_creating_child_jobs() {
    let backend = backend();
    record(&backend, SESSION, TASK, 1, 1);
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let fork = connection
        .execute("thread/fork", json!({ "threadId": FRONTEND }))
        .await
        .unwrap();
    assert_eq!(fork.result["thread"]["sessionId"], FORK_SESSION);
    assert_eq!(fork.result["thread"]["turns"][0]["id"], TASK);
    assert_eq!(backend.count(Operation::TaskSubmit), 0);
    let state = backend.state.lock().unwrap();
    assert_eq!(state.jobs.len(), 1);
    assert_eq!(state.jobs[0]["session_id"], SESSION);
    let copied = &state.conversations[1]["messages"][0];
    assert_eq!(copied["id"], 10_001);
    assert_eq!(copied["source_message_id"], 1);
    assert_eq!(copied["source_session_id"], SESSION);
}

#[tokio::test]
async fn reverted_inherited_prefix_then_append_never_resurrects_removed_task_evidence() {
    let backend = backend();
    record(&backend, SESSION, TASK, 1, 1);
    record(&backend, SESSION, SECOND, 10, 1);
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    connection
        .execute(
            "thread/fork",
            json!({ "threadId": FRONTEND, "excludeTurns": true }),
        )
        .await
        .unwrap();
    connection
        .execute(
            "thread/revert",
            json!({
                "threadId": FORK_FRONTEND, "beforeTurnId": SECOND,
            }),
        )
        .await
        .unwrap();
    record(&backend, FORK_SESSION, THIRD, 20_000, 1);
    backend.state.lock().unwrap().calls.clear();
    let result = connection
        .execute(
            "thread/turns/list",
            json!({
                "threadId": FORK_FRONTEND, "sortDirection": "asc", "itemsView": "full",
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        result.result["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|turn| turn["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [TASK, THIRD]
    );
    let state = backend.state.lock().unwrap();
    assert!(state.jobs.iter().any(|job| job["id"] == SECOND));
    assert!(!state
        .calls
        .iter()
        .any(|(operation, params)| *operation == Operation::TaskStream && params["id"] == SECOND));
}

#[tokio::test]
async fn retry_outer_rows_are_counted_explicitly_and_last_turn_fork_keeps_the_whole_task() {
    let backend = backend();
    record(&backend, SESSION, TASK, 1, 2);
    record(&backend, SESSION, SECOND, 10, 1);
    let source = view(&backend, SESSION);
    let jobs = source["jobs"].as_array().unwrap();
    assert_eq!(
        history::task_user_turn_range(&source, jobs, TASK).unwrap(),
        (0, 2, 3)
    );
    assert_eq!(
        history::task_user_turn_range(&source, jobs, SECOND).unwrap(),
        (2, 3, 3)
    );
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    connection
        .execute(
            "thread/fork",
            json!({
                "threadId": FRONTEND, "lastTurnId": TASK, "excludeTurns": true,
            }),
        )
        .await
        .unwrap();
    let state = backend.state.lock().unwrap();
    let request = &state
        .calls
        .iter()
        .find(|(operation, _)| *operation == Operation::ConversationFork)
        .unwrap()
        .1;
    assert_eq!(request["before_user_turn"], 2);
    assert_eq!(request["expected_revision"], source["history_revision"]);
}

#[tokio::test]
async fn cold_bound_resume_uses_root_uuid_lookup_even_when_recent_list_is_unavailable() {
    let backend = backend();
    record(&backend, SESSION, TASK, 1, 1);
    backend.state.lock().unwrap().fail = Some(Operation::ConversationList);
    for _ in 0..2 {
        let connection = backend.connection(Options::default());
        initialized(&connection).await;
        let result = connection
            .execute("thread/resume", json!({ "threadId": FRONTEND }))
            .await
            .unwrap();
        assert_eq!(result.result["thread"]["turns"][0]["id"], TASK);
    }
    assert_eq!(backend.count(Operation::ConversationList), 0);
}

#[tokio::test]
async fn ancestor_tasks_cannot_be_interrupted_from_the_fork_and_unrecorded_native_tasks_remain_visible(
) {
    let backend = backend();
    record(&backend, SESSION, TASK, 1, 1);
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    connection
        .execute(
            "thread/fork",
            json!({ "threadId": FRONTEND, "excludeTurns": true }),
        )
        .await
        .unwrap();
    {
        let mut state = backend.state.lock().unwrap();
        let mut ancestor = job("running");
        ancestor["id"] = json!(PARENT_ACTIVE);
        state.jobs.push(ancestor);
        let mut native = job("pending");
        native["id"] = json!(CHILD_ACTIVE);
        native["session_id"] = json!(FORK_SESSION);
        state.jobs.push(native);
    }
    assert!(connection
        .execute(
            "turn/interrupt",
            json!({
                "threadId": FORK_FRONTEND, "turnId": PARENT_ACTIVE,
            })
        )
        .await
        .is_err());
    assert_eq!(backend.count(Operation::TaskCancel), 0);
    connection
        .execute(
            "turn/interrupt",
            json!({
                "threadId": FORK_FRONTEND, "turnId": CHILD_ACTIVE,
            }),
        )
        .await
        .unwrap();
    let state = backend.state.lock().unwrap();
    assert_eq!(
        state
            .calls
            .iter()
            .find(|(operation, _)| *operation == Operation::TaskCancel)
            .unwrap()
            .1["id"],
        CHILD_ACTIVE
    );
}

#[test]
fn partial_unbound_truncated_and_mismatched_memberships_are_rejected() {
    let backend = backend();
    record(&backend, SESSION, TASK, 1, 1);
    let valid = view(&backend, SESSION);
    history::verify_visible_history(&valid, valid["jobs"].as_array().unwrap()).unwrap();
    for (field, value) in [
        ("task_bindings_complete", json!(false)),
        ("messages_truncated", json!(true)),
        ("jobs_truncated", json!(true)),
        ("message_count", json!(9)),
    ] {
        let mut bad = valid.clone();
        bad[field] = value;
        assert!(history::verify_visible_history(&bad, bad["jobs"].as_array().unwrap()).is_err());
    }
    let mut bad = valid.clone();
    bad["messages"][1]["source_user_message_id"] = json!(999);
    assert!(history::verify_visible_history(&bad, bad["jobs"].as_array().unwrap()).is_err());
    let mut bad = valid;
    bad["messages"][0]
        .as_object_mut()
        .unwrap()
        .remove("task_id");
    assert!(history::verify_visible_history(&bad, bad["jobs"].as_array().unwrap()).is_err());
}

#[tokio::test]
async fn a_changed_canonical_revision_cannot_apply_the_old_selected_turn_cutoff() {
    let backend = backend();
    record(&backend, SESSION, TASK, 1, 1);
    record(&backend, SESSION, SECOND, 10, 1);
    backend.state.lock().unwrap().hold = Some(Operation::ConversationRevert);
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let task = tokio::spawn(async move {
        connection
            .execute(
                "thread/revert",
                json!({ "threadId": FRONTEND, "beforeTurnId": SECOND }),
            )
            .await
    });
    backend.blocked.notified().await;
    backend.state.lock().unwrap().conversations[0]["history_revision"] = json!("changed");
    backend.release.notify_one();
    assert_eq!(task.await.unwrap().err().unwrap().code, -32003);
    assert_eq!(
        view(&backend, SESSION)["messages"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn missing_revision_keeps_selected_turn_mutation_unavailable() {
    let backend = backend();
    record(&backend, SESSION, TASK, 1, 1);
    let mut valid = view(&backend, SESSION);
    valid.as_object_mut().unwrap().remove("history_revision");
    assert_eq!(history::mutation_revision(&valid).unwrap_err().code, -32005);
}
