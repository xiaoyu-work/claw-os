use super::*;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use tokio::sync::Notify;

use super::backend::{Backend, BackendInfo, Operation, ReviewDecision};
use super::protocol::RpcError;
use super::server::{Connection, Service};

pub(super) const SESSION: &str = "ses_0018e23f14a00_a1b2c3d4e5f6";
pub(super) const FORK_SESSION: &str = "ses_0018e23f14a01_b1b2c3d4e5f6";
pub(super) const FRONTEND: &str = "0182afaf-1234-4567-89ab-cdef01234567";
pub(super) const FORK_FRONTEND: &str = "0282afaf-1234-4567-89ab-cdef01234567";
pub(super) const TASK: &str = "00000000-0000-4000-8000-000000000001";

pub(super) fn info() -> BackendInfo {
    BackendInfo {
        home: "/home/claw".into(),
        config_home: "/home/claw/.config/cos".into(),
        provider: "provider-for-tests".to_string(),
        model: "model-for-tests".to_string(),
        models: vec!["model-for-tests".to_string()],
        ready: true,
    }
}

pub(super) fn conversation() -> Value {
    json!({
        "id": SESSION,
        "presentation_id": FRONTEND,
        "title": "Example",
        "created_at": "2026-09-13T12:00:00Z",
        "updated_at": "2026-09-13T12:00:01Z",
        "archived": false,
        "deleted": false,
        "parent_id": null,
        "messages": [],
    })
}

pub(super) fn job(status: &str) -> Value {
    json!({
        "id": TASK,
        "prompt": "Hello Claw",
        "session_id": SESSION,
        "status": status,
        "created_at": "2026-09-13T12:00:00Z",
        "started_at": "2026-09-13T12:00:01Z",
        "finished_at": if matches!(status, "ok" | "error" | "cancelled") {
            Some("2026-09-13T12:00:02Z")
        } else { None },
        "response": if status == "ok" { Some("Hello from Claw") } else { None },
        "error": if status == "error" { Some("The provider failed") } else { None },
        "waiting_on": [],
    })
}

#[derive(Default)]
pub(super) struct MockState {
    pub conversations: Vec<Value>,
    pub create_result: Option<Value>,
    pub jobs: Vec<Value>,
    pub events: HashMap<String, Vec<Value>>,
    pub calls: Vec<(Operation, Value)>,
    pub fail: Option<Operation>,
    pub hold: Option<Operation>,
    pub pending: Vec<Value>,
    pub decisions: HashMap<String, String>,
    pub reviewed: Vec<(String, ReviewDecision)>,
    pub review_fails: bool,
    pub conversations_truncated: bool,
}

pub(super) fn bound_projection(conversation: &Value, jobs: &[Value]) -> Result<Value, RpcError> {
    if conversation["task_bindings_complete"] != true {
        return Ok(conversation.clone());
    }
    let mut view = conversation.clone();
    let rows = conversation["messages"].as_array().unwrap();
    let mut keys = Vec::new();
    for row in rows {
        let key = (
            row["source_session_id"].as_str().unwrap(),
            row["task_id"].as_str().unwrap(),
        );
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    let retained = keys
        .iter()
        .map(|(source, task)| {
            jobs.iter()
                .find(|job| job["id"] == *task && job["session_id"] == *source)
                .cloned()
                .ok_or_else(|| RpcError::unavailable("Bound source evidence is missing"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    view["message_count"] = json!(rows.len());
    view["messages_truncated"] = json!(false);
    view["job_count"] = json!(retained.len());
    view["jobs_truncated"] = json!(false);
    view["jobs"] = json!(retained);
    Ok(view)
}

fn bound_prefix(conversation: &Value, keep: usize) -> Result<Vec<Value>, RpcError> {
    let rows = conversation["messages"].as_array().unwrap();
    let mut seen = 0usize;
    let end = rows
        .iter()
        .position(|row| {
            if row["is_user_prompt"] == true {
                if seen == keep {
                    return true;
                }
                seen += 1;
            }
            false
        })
        .unwrap_or(rows.len());
    if seen < keep {
        return Err(RpcError::params("Too many retained user rows"));
    }
    let prefix = &rows[..end];
    let keys = prefix
        .iter()
        .map(|row| {
            (
                row["source_session_id"].as_str().unwrap(),
                row["task_id"].as_str().unwrap(),
            )
        })
        .collect::<std::collections::HashSet<_>>();
    if rows[end..].iter().any(|row| {
        keys.contains(&(
            row["source_session_id"].as_str().unwrap(),
            row["task_id"].as_str().unwrap(),
        ))
    }) {
        return Err(RpcError::failed(
            "Cannot split a task's recorded retry membership",
        ));
    }
    Ok(prefix.to_vec())
}

pub(super) struct MockBackend {
    info: BackendInfo,
    pub state: StdMutex<MockState>,
    pub blocked: Notify,
    pub release: Notify,
}

impl MockBackend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            info: info(),
            state: StdMutex::new(MockState {
                conversations: vec![conversation()],
                ..MockState::default()
            }),
            blocked: Notify::new(),
            release: Notify::new(),
        })
    }

    pub fn connection(self: &Arc<Self>, options: Options) -> Arc<Connection> {
        Arc::new(Connection::new(Arc::new(Service::new(
            self.clone(),
            options,
        ))))
    }

    pub fn count(&self, operation: Operation) -> usize {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(called, _)| *called == operation)
            .count()
    }

    pub fn seed_job(&self, job: Value, events: Vec<Value>) {
        let mut state = self.state.lock().unwrap();
        state
            .events
            .insert(job["id"].as_str().unwrap().to_string(), events);
        if let Some(conversation) = state
            .conversations
            .iter_mut()
            .find(|conversation| conversation["id"] == job["session_id"])
        {
            let ts_ms = chrono::DateTime::parse_from_rfc3339(job["created_at"].as_str().unwrap())
                .unwrap()
                .timestamp_millis();
            conversation["messages"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "role": "user", "text": job["prompt"], "content": job["prompt"],
                    "tool_calls": [], "tool_results": [], "ts_ms": ts_ms,
                }));
        }
        state.jobs.push(job);
    }
}

#[async_trait]
impl Backend for MockBackend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    async fn call(&self, operation: Operation, params: Value) -> Result<Value, RpcError> {
        let hold = {
            let mut state = self.state.lock().unwrap();
            state.calls.push((operation, params.clone()));
            if state.fail == Some(operation) {
                return Err(RpcError::unavailable("Claw is unavailable"));
            }
            if state.hold == Some(operation) {
                state.hold = None;
                true
            } else {
                false
            }
        };
        if hold {
            self.blocked.notify_one();
            self.release.notified().await;
        }
        let mut state = self.state.lock().unwrap();
        match operation {
            Operation::ConversationCreate => {
                let conversation = state
                    .create_result
                    .take()
                    .unwrap_or_else(|| state.conversations[0].clone());
                if !state
                    .conversations
                    .iter()
                    .any(|existing| existing["id"] == conversation["id"])
                {
                    state.conversations.push(conversation.clone());
                }
                Ok(json!({ "conversation": bound_projection(&conversation, &state.jobs)? }))
            }
            Operation::ConversationGet => {
                let id = params["id"].as_str().unwrap();
                let conversation = state
                    .conversations
                    .iter()
                    .find(|conversation| {
                        conversation["id"] == id || conversation["presentation_id"] == id
                    })
                    .ok_or_else(|| RpcError::stale("Conversation not found"))?;
                Ok(json!({ "conversation": bound_projection(conversation, &state.jobs)? }))
            }
            Operation::ConversationList => {
                let archived = params["archived"].as_bool().unwrap_or(false);
                Ok(json!({ "conversations": state.conversations.iter()
                    .filter(|conversation| conversation["archived"] == archived)
                    .cloned().collect::<Vec<_>>(),
                    "conversation_count": state.conversations.len(),
                    "conversations_truncated": state.conversations_truncated,
                }))
            }
            Operation::ConversationUpdate => {
                let id = params["id"].as_str().unwrap();
                let conversation = state
                    .conversations
                    .iter_mut()
                    .find(|conversation| conversation["id"] == id)
                    .ok_or_else(|| RpcError::stale("Conversation not found"))?;
                for key in ["title", "archived", "deleted"] {
                    if let Some(value) = params.get(key) {
                        conversation[key] = value.clone();
                    }
                }
                Ok(json!({ "conversation": conversation }))
            }
            Operation::ConversationFork => {
                let source = state
                    .conversations
                    .iter()
                    .find(|conversation| conversation["id"] == params["id"])
                    .ok_or_else(|| RpcError::stale("Source conversation not found"))?;
                if params.get("expected_revision").is_some()
                    && params["expected_revision"] != source["history_revision"]
                {
                    return Err(RpcError::stale("Canonical history revision changed"));
                }
                let mut fork = source.clone();
                if source["task_bindings_complete"] == true {
                    let rows = if let Some(keep) = params["before_user_turn"].as_u64() {
                        bound_prefix(source, keep as usize)?
                    } else {
                        source["messages"].as_array().unwrap().clone()
                    };
                    fork["messages"] = json!(rows
                        .into_iter()
                        .map(|mut row| {
                            row["id"] = json!(row["id"].as_i64().unwrap() + 10_000);
                            row
                        })
                        .collect::<Vec<_>>());
                    fork["history_revision"] = json!("fork-revision");
                }
                fork["id"] = json!(FORK_SESSION);
                fork["presentation_id"] = json!(FORK_FRONTEND);
                fork["parent_id"] = params["id"].clone();
                state.conversations.push(fork.clone());
                Ok(json!({ "conversation": bound_projection(&fork, &state.jobs)? }))
            }
            Operation::ConversationRevert => {
                let index = state
                    .conversations
                    .iter()
                    .position(|conversation| conversation["id"] == params["id"])
                    .ok_or_else(|| RpcError::stale("Conversation not found"))?;
                let source = &state.conversations[index];
                if source["task_bindings_complete"] != true {
                    return Ok(json!({ "conversation": source }));
                }
                if params["expected_revision"] != source["history_revision"] {
                    return Err(RpcError::stale("Canonical history revision changed"));
                }
                let total = source["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|row| row["is_user_prompt"] == true)
                    .count();
                let remove = params["user_turns"].as_u64().unwrap() as usize;
                if remove == 0 || remove > total {
                    return Err(RpcError::params("Invalid user turn count"));
                }
                let prefix = bound_prefix(source, total - remove)?;
                state.conversations[index]["messages"] = json!(prefix);
                state.conversations[index]["history_revision"] = json!("reverted-revision");
                Ok(
                    json!({ "conversation": bound_projection(&state.conversations[index], &state.jobs)? }),
                )
            }
            Operation::TaskList => Ok(json!({ "jobs": state.jobs })),
            Operation::TaskSubmit => {
                let mut task = job("pending");
                task["id"] = json!(format!(
                    "00000000-0000-4000-8000-{:012}",
                    state.jobs.len() + 1
                ));
                task["prompt"] = params["prompt"].clone();
                task["session_id"] = params["session_id"].clone();
                task["requested_model"] = params["model"].clone();
                state.jobs.push(task.clone());
                Ok(task)
            }
            Operation::TaskGet => state
                .jobs
                .iter()
                .find(|job| job["id"] == params["id"])
                .cloned()
                .ok_or_else(|| RpcError::stale("Task not found")),
            Operation::TaskCancel => {
                let task = state
                    .jobs
                    .iter_mut()
                    .find(|job| job["id"] == params["id"])
                    .ok_or_else(|| RpcError::stale("Task not found"))?;
                let mut result = task.clone();
                result["cancelled"] = json!(false);
                result["cancel_requested"] = json!(true);
                Ok(result)
            }
            Operation::TaskStream => {
                let task = state
                    .jobs
                    .iter()
                    .find(|job| job["id"] == params["id"])
                    .ok_or_else(|| RpcError::stale("Task not found"))?;
                let events = state
                    .events
                    .get(params["id"].as_str().unwrap())
                    .cloned()
                    .unwrap_or_default();
                let cursor = params["cursor"].as_u64().unwrap_or(0) as usize;
                if cursor > events.len() {
                    return Err(RpcError::stale("Stream cursor is stale"));
                }
                Ok(json!({
                    "cursor": events.len(),
                    "events": events[cursor..],
                    "terminal": matches!(task["status"].as_str(), Some("ok" | "error" | "cancelled")),
                    "job": task,
                }))
            }
            Operation::PermissionPending => Ok(json!({ "requests": state.pending })),
            Operation::PermissionStatus => {
                let statuses = params["ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|id| {
                        let id = id.as_str().unwrap();
                        let status = state
                            .decisions
                            .get(id)
                            .map(String::as_str)
                            .unwrap_or("pending");
                        json!({ "id": id, "status": status })
                    })
                    .collect::<Vec<_>>();
                Ok(json!({ "statuses": statuses }))
            }
            Operation::SkillsList => {
                Ok(json!({ "data": [{ "cwd": "/home/claw", "skills": [], "errors": [] }] }))
            }
        }
    }

    async fn review(&self, id: &str, decision: ReviewDecision) -> Result<(), RpcError> {
        let mut state = self.state.lock().unwrap();
        state.reviewed.push((id.to_string(), decision));
        if state.review_fails {
            return Err(RpcError::failed("System authorization denied"));
        }
        state.decisions.insert(
            id.to_string(),
            match decision {
                ReviewDecision::ApproveOnce => "approved".to_string(),
                ReviewDecision::Deny => "denied".to_string(),
            },
        );
        Ok(())
    }
}

pub(super) async fn initialized(connection: &Connection) {
    connection
        .execute(
            "initialize",
            json!({
                "clientInfo": { "name": "codex-tui", "version": "test", "title": null },
                "capabilities": { "experimentalApi": true },
            }),
        )
        .await
        .unwrap();
    connection.notify("initialized", Value::Null).unwrap();
}

#[tokio::test]
async fn shutdown_preempts_pending_initialization_and_already_closed_watches() {
    let (sender, receiver) = watch::channel(false);
    let waiting = tokio::spawn(initialize_or_shutdown(
        std::future::pending::<Result<(), String>>(),
        receiver,
    ));
    sender.send(true).unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        None,
    );
    let (sender, receiver) = watch::channel(false);
    drop(sender);
    let polled = std::sync::atomic::AtomicBool::new(false);
    let result = initialize_or_shutdown(
        async {
            polled.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok::<(), String>(())
        },
        receiver,
    )
    .await
    .unwrap();
    assert_eq!(result, None);
    assert!(!polled.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn false_watch_updates_do_not_abort_initialization() {
    let (sender, receiver) = watch::channel(false);
    sender.send(false).unwrap();
    let result = initialize_or_shutdown(async { Ok::<_, String>(42) }, receiver)
        .await
        .unwrap();
    assert_eq!(result, Some(42));
}

#[tokio::test]
async fn starts_a_real_conversation_without_a_model_task() {
    let backend = MockBackend::new();
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let result = connection.execute("thread/start", json!({})).await.unwrap();
    assert_eq!(result.result["thread"]["sessionId"], SESSION);
    assert_eq!(result.result["thread"]["id"], FRONTEND);
    assert_eq!(backend.count(Operation::ConversationCreate), 1);
    assert_eq!(backend.count(Operation::TaskSubmit), 0);
}

#[tokio::test]
async fn submission_preserves_defaults_and_rejects_overlapping_turns() {
    let backend = MockBackend::new();
    let connection = backend.connection(Options {
        use_memory: false,
        max_turns: Some(7),
        session_id: None,
    });
    initialized(&connection).await;
    let params = json!({
        "threadId": SESSION, "input": [{ "type": "text", "text": "  exact prompt\n", "text_elements": [] }],
        "clientUserMessageId": "client-1", "model": "model-for-tests",
    });
    let result = connection
        .execute("turn/start", params.clone())
        .await
        .unwrap();
    assert_eq!(result.result["turn"]["id"], TASK);
    let state = backend.state.lock().unwrap();
    let submitted = &state
        .calls
        .iter()
        .find(|(op, _)| *op == Operation::TaskSubmit)
        .unwrap()
        .1;
    assert_eq!(submitted["prompt"], "  exact prompt\n");
    assert_eq!(submitted["session_id"], SESSION);
    assert_eq!(submitted["use_memory"], false);
    assert_eq!(submitted["max_turns"], 7);
    assert_eq!(submitted["model"], "model-for-tests");
    drop(state);
    let error = connection
        .execute("turn/start", params)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, -32004);
    assert_eq!(backend.count(Operation::TaskSubmit), 1);
}

fn model_backend() -> Arc<MockBackend> {
    let mut backend = MockBackend::new();
    Arc::get_mut(&mut backend)
        .unwrap()
        .info
        .models
        .push("alternate-model".into());
    backend
}

#[tokio::test]
async fn thread_model_selection_is_forwarded_without_mutating_global_defaults() {
    let backend = model_backend();
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let started = connection
        .execute("thread/start", json!({ "model": "alternate-model" }))
        .await
        .unwrap();
    assert_eq!(started.result["model"], "alternate-model");
    assert_eq!(started.result["thread"]["model"], "alternate-model");
    let listed = connection.execute("thread/list", json!({})).await.unwrap();
    assert_eq!(listed.result["data"][0]["model"], "alternate-model");
    connection
        .execute(
            "turn/start",
            json!({
                "threadId": FRONTEND, "input": [{ "type": "text", "text": "Hello" }],
            }),
        )
        .await
        .unwrap();
    let state = backend.state.lock().unwrap();
    let submitted = state
        .calls
        .iter()
        .find(|(operation, _)| *operation == Operation::TaskSubmit)
        .unwrap();
    assert_eq!(submitted.1["model"], "alternate-model");
    assert_eq!(state.jobs[0]["requested_model"], "alternate-model");
    assert_eq!(backend.info.model, "model-for-tests");
    assert_eq!(backend.info.provider, "provider-for-tests");
}

#[tokio::test]
async fn model_settings_apply_to_future_tasks_not_an_active_task() {
    let backend = model_backend();
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    connection
        .execute(
            "turn/start",
            json!({
                "threadId": SESSION, "model": "model-for-tests",
                "input": [{ "type": "text", "text": "First" }],
            }),
        )
        .await
        .unwrap();
    connection
        .execute(
            "thread/settings/update",
            json!({
                "threadId": FRONTEND, "model": "alternate-model",
            }),
        )
        .await
        .unwrap();
    {
        let mut state = backend.state.lock().unwrap();
        assert_eq!(state.jobs[0]["requested_model"], "model-for-tests");
        state.jobs[0]["status"] = json!("ok");
    }
    connection
        .execute(
            "turn/start",
            json!({
                "threadId": SESSION, "input": [{ "type": "text", "text": "Second" }],
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        backend.state.lock().unwrap().jobs[1]["requested_model"],
        "alternate-model"
    );
}

#[tokio::test]
async fn per_turn_model_overrides_thread_selection_and_unknown_models_never_submit() {
    let backend = model_backend();
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    connection
        .execute(
            "thread/settings/update",
            json!({
                "threadId": SESSION, "model": "alternate-model",
            }),
        )
        .await
        .unwrap();
    for params in [
        json!({ "threadId": SESSION, "model": "unlisted-model", "input": [{"type":"text","text":"Hello"}] }),
        json!({ "threadId": SESSION, "modelProvider": "another-provider", "input": [{"type":"text","text":"Hello"}] }),
        json!({ "threadId": SESSION, "model": "alternate-model", "effort": "high", "input": [{"type":"text","text":"Hello"}] }),
    ] {
        assert!(connection.execute("turn/start", params).await.is_err());
    }
    assert_eq!(backend.count(Operation::TaskSubmit), 0);
    connection
        .execute(
            "turn/start",
            json!({
                "threadId": SESSION, "model": "model-for-tests",
                "input": [{ "type": "text", "text": "Explicit choice" }],
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        backend.state.lock().unwrap().jobs[0]["requested_model"],
        "model-for-tests"
    );
}

#[tokio::test]
async fn resume_restores_the_recorded_requested_model_not_a_fallback_provider_result() {
    let backend = model_backend();
    let mut saved = job("ok");
    saved["requested_model"] = json!("alternate-model");
    saved["model"] = json!("fallback-model");
    backend.seed_job(saved, Vec::new());
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let resumed = connection
        .execute(
            "thread/resume",
            json!({
                "threadId": FRONTEND, "excludeTurns": true,
            }),
        )
        .await
        .unwrap();
    assert_eq!(resumed.result["model"], "alternate-model");
    assert_eq!(resumed.result["thread"]["model"], "alternate-model");
    assert_eq!(backend.count(Operation::TaskSubmit), 0);
}

#[tokio::test]
async fn unsupported_features_never_return_empty_success() {
    let backend = MockBackend::new();
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    for method in [
        "collaborationMode/list",
        "review/start",
        "thread/queue/add",
        "thread/compact/start",
        "mcpServerStatus/list",
        "config/batchWrite",
        "account/login/start",
        "command/exec",
        "thread/realtime/start",
        "fs/writeFile",
        "thread/inject_items",
        "experimentalFeature/list",
    ] {
        let error = connection.execute(method, json!({})).await.err().unwrap();
        assert_eq!(error.code, -32601, "{method}");
    }
    assert_eq!(backend.count(Operation::TaskSubmit), 0);
}

#[tokio::test]
async fn missing_backend_and_stale_cancellation_are_errors() {
    let backend = MockBackend::new();
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    backend.state.lock().unwrap().fail = Some(Operation::ConversationCreate);
    assert_eq!(
        connection
            .execute("thread/start", json!({}))
            .await
            .err()
            .unwrap()
            .code,
        -32005
    );
    backend.state.lock().unwrap().fail = None;
    backend.seed_job(job("running"), Vec::new());
    let error = connection
        .execute(
            "turn/interrupt",
            json!({
                "threadId": SESSION, "turnId": "wrong-task",
            }),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, -32003);
    assert_eq!(backend.count(Operation::TaskCancel), 0);
}

#[tokio::test]
async fn multibyte_session_ids_are_rejected_without_panics_or_broker_effects() {
    let malformed = format!("ses_{}é{}", "a".repeat(12), "b".repeat(12));
    assert_eq!(malformed.len(), 30);
    assert!(protocol::canonical_session_id(&malformed).is_none());
    assert!(initial_thread_id(&malformed).await.is_err());
    let backend = MockBackend::new();
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let error = connection
        .execute("thread/read", json!({ "threadId": malformed }))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, -32602);
    assert!(backend.state.lock().unwrap().calls.is_empty());

    let mut bad_metadata = conversation();
    bad_metadata["id"] = json!(malformed);
    assert!(history::conversation(json!({ "conversation": bad_metadata })).is_err());
    assert!(history::thread_view(&bad_metadata, &info(), false, None, Vec::new()).is_err());
    let mut bad_parent = conversation();
    bad_parent["parent_id"] = json!(malformed);
    assert!(connection
        .service
        .with_parent_identity(bad_parent)
        .await
        .is_err());
    assert!(backend.state.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn truncated_canonical_conversation_list_is_not_reported_as_complete() {
    let backend = MockBackend::new();
    backend.state.lock().unwrap().conversations_truncated = true;
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let error = connection
        .execute("thread/list", json!({}))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, -32005);
}

#[tokio::test]
async fn metadata_resume_does_not_conflate_execution_evidence_with_canonical_history() {
    let backend = MockBackend::new();
    backend.seed_job(job("ok"), vec![json!({
        "event": { "kind": "text_delta", "text": "Persisted answer" },
    }), json!({
        "event": { "kind": "done", "finish": "stop", "usage": { "input_tokens": 1, "output_tokens": 2 } },
    })]);
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let result = connection
        .execute(
            "thread/resume",
            json!({ "threadId": SESSION, "excludeTurns": true }),
        )
        .await
        .unwrap();
    assert_eq!(result.result["thread"]["id"], FRONTEND);
    assert_eq!(result.result["thread"]["turns"], json!([]));
    for (method, params) in [
        (
            "thread/read",
            json!({ "threadId": SESSION, "includeTurns": true }),
        ),
        ("thread/turns/list", json!({ "threadId": SESSION })),
        ("thread/items/list", json!({ "threadId": SESSION })),
    ] {
        assert_eq!(
            connection.execute(method, params).await.err().unwrap().code,
            -32005
        );
    }
    assert_eq!(backend.count(Operation::TaskSubmit), 0);
    assert_eq!(backend.count(Operation::TaskStream), 0);
}

#[tokio::test]
async fn resumed_startup_does_not_hijack_the_next_new_conversation() {
    let backend = MockBackend::new();
    let connection = backend.connection(Options {
        session_id: Some(SESSION.into()),
        ..Options::default()
    });
    initialized(&connection).await;
    connection
        .execute(
            "thread/resume",
            json!({ "threadId": SESSION, "excludeTurns": true }),
        )
        .await
        .unwrap();
    connection.execute("thread/start", json!({})).await.unwrap();
    assert_eq!(backend.count(Operation::ConversationCreate), 1);
}

#[tokio::test]
async fn cold_frontend_uuid_lookup_uses_the_canonical_get_route_not_a_recent_list() {
    let backend = MockBackend::new();
    backend.state.lock().unwrap().fail = Some(Operation::ConversationList);
    for _ in 0..2 {
        let connection = backend.connection(Options::default());
        initialized(&connection).await;
        let response = connection
            .execute(
                "thread/resume",
                json!({
                    "threadId": FRONTEND, "excludeTurns": true,
                }),
            )
            .await
            .unwrap();
        assert_eq!(response.result["thread"]["id"], FRONTEND);
        assert_eq!(response.result["thread"]["sessionId"], SESSION);
    }
    let state = backend.state.lock().unwrap();
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|(operation, params)| {
                *operation == Operation::ConversationGet && params["id"] == FRONTEND
            })
            .count(),
        2
    );
    assert!(!state
        .calls
        .iter()
        .any(|(operation, _)| *operation == Operation::ConversationList));
}

#[tokio::test]
async fn thread_start_is_new_even_when_launcher_supplies_a_resume_session() {
    let backend = MockBackend::new();
    backend.seed_job(job("ok"), Vec::new());
    let mut fresh = conversation();
    fresh["id"] = json!(FORK_SESSION);
    fresh["presentation_id"] = json!(FORK_FRONTEND);
    backend.state.lock().unwrap().create_result = Some(fresh);
    let connection = backend.connection(Options {
        session_id: Some(SESSION.into()),
        ..Options::default()
    });
    initialized(&connection).await;
    let response = connection.execute("thread/start", json!({})).await.unwrap();
    assert_eq!(response.result["thread"]["sessionId"], FORK_SESSION);
    assert_eq!(response.result["thread"]["id"], FORK_FRONTEND);
    assert_eq!(response.result["thread"]["turns"], json!([]));
    assert!(response.drive.is_none());
    assert_eq!(backend.count(Operation::ConversationCreate), 1);
    assert_eq!(backend.count(Operation::TaskStream), 0);
    assert_eq!(backend.count(Operation::TaskSubmit), 0);
}

#[tokio::test]
async fn empty_fork_lineage_uses_canonical_parent_presentation_metadata() {
    let backend = MockBackend::new();
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let response = connection
        .execute(
            "thread/fork",
            json!({
                "threadId": FRONTEND, "excludeTurns": true,
            }),
        )
        .await
        .unwrap();
    assert_eq!(response.result["thread"]["id"], FORK_FRONTEND);
    assert_eq!(response.result["thread"]["sessionId"], FORK_SESSION);
    assert_eq!(response.result["thread"]["forkedFromId"], FRONTEND);
    let state = backend.state.lock().unwrap();
    let fork = state
        .calls
        .iter()
        .find(|(operation, _)| *operation == Operation::ConversationFork)
        .unwrap();
    assert_eq!(fork.1["id"], SESSION);
}

#[tokio::test]
async fn missing_root_presentation_metadata_prevents_task_submission() {
    let backend = MockBackend::new();
    backend.state.lock().unwrap().conversations[0]
        .as_object_mut()
        .unwrap()
        .remove("presentation_id");
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    let error = connection
        .execute(
            "turn/start",
            json!({
                "threadId": SESSION, "input": [{ "type": "text", "text": "Hello" }],
            }),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, -32005);
    assert_eq!(backend.count(Operation::TaskSubmit), 0);
}

#[tokio::test]
async fn fork_and_revert_refuse_missing_retained_task_bindings_before_mutation() {
    let backend = MockBackend::new();
    backend.seed_job(job("ok"), Vec::new());
    {
        let mut state = backend.state.lock().unwrap();
        state.conversations[0]["jobs"] = json!(state.jobs);
        state.conversations[0]["jobs_truncated"] = json!(false);
    }
    let connection = backend.connection(Options::default());
    initialized(&connection).await;
    for (method, params) in [
        (
            "thread/fork",
            json!({ "threadId": SESSION, "excludeTurns": true }),
        ),
        (
            "thread/revert",
            json!({ "threadId": SESSION, "beforeTurnId": TASK }),
        ),
    ] {
        assert_eq!(
            connection.execute(method, params).await.err().unwrap().code,
            -32005
        );
    }
    assert_eq!(backend.count(Operation::ConversationFork), 0);
    assert_eq!(backend.count(Operation::ConversationRevert), 0);
}

#[tokio::test]
async fn upstream_schema_fixtures() {
    fn fixture(schema: &str, value: Value) {
        assert!(value.is_object());
        println!(
            "CLAW_TUI_FIXTURE {}",
            json!({ "schema": schema, "value": value })
        );
    }
    let backend = MockBackend::new();
    let connection = backend.connection(Options::default());
    let initialize = connection
        .execute(
            "initialize",
            json!({
                "clientInfo": { "name": "codex-tui", "version": "test" },
                "capabilities": { "experimentalApi": true },
            }),
        )
        .await
        .unwrap();
    fixture("v1/InitializeResponse.json", initialize.result);
    connection.notify("initialized", Value::Null).unwrap();
    for (method, schema) in [
        ("account/read", "GetAccountResponse"),
        ("model/list", "ModelListResponse"),
        ("config/read", "ConfigReadResponse"),
        ("configRequirements/read", "ConfigRequirementsReadResponse"),
        (
            "modelProvider/capabilities/read",
            "ModelProviderCapabilitiesReadResponse",
        ),
        ("thread/list", "ThreadListResponse"),
        ("thread/loaded/list", "ThreadLoadedListResponse"),
        ("skills/list", "SkillsListResponse"),
    ] {
        fixture(
            &format!("v2/{schema}.json"),
            connection.execute(method, json!({})).await.unwrap().result,
        );
    }
    let started = connection.execute("thread/start", json!({})).await.unwrap();
    fixture("v2/ThreadStartResponse.json", started.result);
    fixture(
        "v2/ThreadStartedNotification.json",
        started.notifications[0]["params"].clone(),
    );
    for (method, schema, params) in [
        (
            "thread/read",
            "ThreadReadResponse",
            json!({ "threadId": SESSION }),
        ),
        (
            "thread/resume",
            "ThreadResumeResponse",
            json!({ "threadId": SESSION, "excludeTurns": true }),
        ),
        (
            "thread/turns/list",
            "ThreadTurnsListResponse",
            json!({ "threadId": SESSION }),
        ),
        (
            "thread/items/list",
            "ThreadItemsListResponse",
            json!({ "threadId": SESSION }),
        ),
    ] {
        fixture(
            &format!("v2/{schema}.json"),
            connection.execute(method, params).await.unwrap().result,
        );
    }
    let submitted = connection
        .execute(
            "turn/start",
            json!({
                "threadId": SESSION, "input": [{ "type": "text", "text": "Hello Claw" }],
            }),
        )
        .await
        .unwrap();
    fixture("v2/TurnStartResponse.json", submitted.result);
    assert_eq!(
        connection
            .execute(
                "thread/settings/update",
                json!({
                    "threadId": FRONTEND, "model": "model-for-tests",
                }),
            )
            .await
            .unwrap()
            .result,
        json!({}),
    );
    let mut projection =
        super::events::Projection::new(FRONTEND.to_string(), SESSION.into(), TASK.into());
    let mut messages = projection.begin("Hello Claw", None, &job("running"));
    for record in [
        json!({ "event": { "kind": "text_delta", "text": "Visible answer" } }),
        json!({ "event": { "kind": "tool_use_start", "id": "tool-1", "name": "cos_fs" } }),
        json!({ "event": { "kind": "reasoning", "id": "rs-1", "summary": ["Visible summary"], "encrypted_content": "not-visible" } }),
        json!({ "event": { "kind": "done", "finish": "tool_use", "usage": { "input_tokens": 3, "output_tokens": 4 } } }),
        json!({ "progress": { "kind": "tool_result", "id": "tool-1", "name": "cos_fs", "ok": true } }),
    ] {
        messages.extend(projection.record(&record).unwrap().messages);
    }
    messages.extend(projection.finish(&job("ok")).unwrap());
    for message in messages {
        let schema = match message["method"].as_str().unwrap() {
            "turn/started" => "TurnStartedNotification",
            "turn/completed" => "TurnCompletedNotification",
            "thread/status/changed" => "ThreadStatusChangedNotification",
            "item/started" => "ItemStartedNotification",
            "item/completed" => "ItemCompletedNotification",
            "item/agentMessage/delta" => "AgentMessageDeltaNotification",
            "item/reasoning/summaryPartAdded" => "ReasoningSummaryPartAddedNotification",
            "item/reasoning/summaryTextDelta" => "ReasoningSummaryTextDeltaNotification",
            "thread/tokenUsage/updated" => "ThreadTokenUsageUpdatedNotification",
            method => panic!("unmapped fixture {method}"),
        };
        fixture(&format!("v2/{schema}.json"), message["params"].clone());
    }
    backend.state.lock().unwrap().pending.push(json!({
        "id": "approval-fixture", "verb": "fs.write", "scope": { "path": "/home/claw/output" },
        "session": SESSION, "reason": "Write the requested output",
    }));
    let approvals = super::approvals::Approvals::default();
    let requests = approvals
        .present(
            backend.as_ref(),
            FRONTEND,
            SESSION,
            TASK,
            &["approval-fixture".into()],
        )
        .await
        .unwrap();
    fixture(
        "ToolRequestUserInputParams.json",
        requests[0]["params"].clone(),
    );
    fixture(
        "ToolRequestUserInputResponse.json",
        json!({
            "answers": { "approval-fixture": { "answers": ["Authorize once"] } },
        }),
    );
    let resolved = approvals.resolve_task(TASK).await;
    fixture(
        "v2/ServerRequestResolvedNotification.json",
        resolved[0]["params"].clone(),
    );
}
