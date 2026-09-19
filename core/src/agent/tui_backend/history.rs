use std::collections::HashSet;

use serde_json::{json, Value};

use super::backend::{Backend, BackendInfo, Operation};
use super::events::{iso_seconds, Projection};
use super::protocol::{backend_string, safe_text, RpcError, MAX_HISTORY_ROWS, MAX_MESSAGE_BYTES};

/// The broker owns this durable presentation identity and its reverse lookup.
/// Missing metadata is an upgrade error, never an invitation to mint an alias.
pub(super) fn frontend_id(conversation: &Value) -> Result<&str, RpcError> {
    let id = conversation
        .get("presentation_id")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::unavailable(
            "The canonical Claw conversation service must expose a stable presentation_id; upgrade clawd",
        ))?;
    uuid::Uuid::parse_str(id)
        .map_err(|_| RpcError::backend("Claw returned an invalid presentation_id"))?;
    Ok(id)
}

pub(super) fn conversation(result: Value) -> Result<Value, RpcError> {
    let conversation = result
        .get("conversation")
        .filter(|value| value.is_object())
        .ok_or_else(|| RpcError::backend("Claw returned no conversation"))?
        .clone();
    let id = backend_string(&conversation, "id")?;
    super::protocol::canonical_session_id(id)
        .ok_or_else(|| RpcError::backend("Claw returned an invalid canonical session id"))?;
    if conversation.get("deleted").and_then(Value::as_bool) == Some(true) {
        return Err(RpcError::stale("Conversation is deleted"));
    }
    frontend_id(&conversation)?;
    Ok(conversation)
}

pub(super) fn thread_view(
    conversation: &Value,
    info: &BackendInfo,
    loaded: bool,
    active: Option<&Value>,
    turns: Vec<Value>,
) -> Result<Value, RpcError> {
    let id = backend_string(conversation, "id")?;
    super::protocol::canonical_session_id(id)
        .ok_or_else(|| RpcError::backend("Claw returned an invalid canonical session id"))?;
    let frontend_id = frontend_id(conversation)?;
    let parent_frontend_id = match conversation.get("parent_id").and_then(Value::as_str) {
        None => None,
        Some(_) => {
            let parent = conversation
                .get("parent_presentation_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    RpcError::unavailable(
                        "The parent conversation's presentation identity is unavailable",
                    )
                })?;
            uuid::Uuid::parse_str(parent).map_err(|_| {
                RpcError::backend("Claw returned an invalid parent presentation identity")
            })?;
            Some(parent)
        }
    };
    let created = iso_seconds(conversation.get("created_at"))
        .ok_or_else(|| RpcError::backend("Conversation has no valid creation timestamp"))?;
    let updated = iso_seconds(conversation.get("updated_at"))
        .ok_or_else(|| RpcError::backend("Conversation has no valid update timestamp"))?;
    let preview = conversation
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().find_map(|message| {
                (message["role"] == "user"
                    && message
                        .get("tool_results")
                        .and_then(Value::as_array)
                        .is_none_or(Vec::is_empty))
                .then(|| message.get("text").and_then(Value::as_str))
                .flatten()
            })
        })
        .or_else(|| conversation.get("title").and_then(Value::as_str))
        .unwrap_or("");
    let status = match active
        .and_then(|job| job.get("status"))
        .and_then(Value::as_str)
    {
        Some("waiting_approval") => {
            json!({ "type": "active", "activeFlags": ["waitingOnApproval"] })
        }
        Some("pending" | "running") => json!({ "type": "active", "activeFlags": [] }),
        _ if loaded => json!({ "type": "idle" }),
        _ => json!({ "type": "notLoaded" }),
    };
    Ok(json!({
        "id": frontend_id,
        "sessionId": id,
        "forkedFromId": parent_frontend_id,
        "parentThreadId": null,
        "preview": safe_text(&preview.chars().take(160).collect::<String>()),
        "ephemeral": false,
        "section": null,
        "sectionEnteredAt": null,
        "projectId": null,
        "historyMode": "paginated",
        "modelProvider": "claw",
        "model": safe_text(&info.model),
        "reasoningEffort": null,
        "createdAt": created,
        "updatedAt": updated,
        "recencyAt": updated,
        "status": status,
        "path": null,
        "cwd": info.home,
        "cliVersion": env!("CARGO_PKG_VERSION"),
        "originator": "claw",
        "source": "cli",
        "canAcceptDirectInput": loaded,
        "threadSource": "user",
        "agentNickname": null,
        "agentRole": null,
        "gitInfo": null,
        "name": conversation.get("title").and_then(Value::as_str).map(safe_text),
        "turns": turns,
    }))
}

pub(super) fn started_response(thread: Value, info: &BackendInfo) -> Value {
    let model = thread
        .get("model")
        .cloned()
        .unwrap_or_else(|| json!(safe_text(&info.model)));
    json!({
        "thread": thread,
        "model": model,
        "modelProvider": "claw",
        "serviceTier": null,
        "disabledPluginIds": [],
        "cwd": info.home,
        "runtimeWorkspaceRoots": [],
        "instructionSources": [],
        "approvalPolicy": "on-request",
        "approvalsReviewer": "user",
        "sandbox": { "type": "externalSandbox", "networkAccess": "restricted" },
        "activePermissionProfile": null,
        "reasoningEffort": null,
    })
}

/// Job records and their redacted stream journals are the only authority for
/// turn identities and execution outcomes. Memory text alone proves neither.
pub(super) async fn jobs(
    backend: &dyn Backend,
    conversation: &Value,
) -> Result<Vec<Value>, RpcError> {
    let session_id = backend_string(conversation, "id")?;
    let bound = conversation
        .get("task_bindings_complete")
        .and_then(Value::as_bool)
        == Some(true);
    let mut jobs = if let Some(jobs) = conversation.get("jobs") {
        jobs.as_array()
            .ok_or_else(|| RpcError::backend("Conversation jobs must be an array"))?
            .clone()
    } else {
        let result = backend
            .call(Operation::TaskList, json!({ "limit": MAX_HISTORY_ROWS }))
            .await?;
        let jobs = result
            .get("jobs")
            .and_then(Value::as_array)
            .ok_or_else(|| RpcError::backend("Claw returned no job history"))?;
        if jobs.len() >= MAX_HISTORY_ROWS as usize {
            return Err(RpcError::unavailable(
                "Claw's bounded job list cannot prove this conversation's complete history; a session-scoped paginated job-history backend is required",
            ));
        }
        jobs.iter()
            .filter(|job| job.get("session_id").and_then(Value::as_str) == Some(session_id))
            .cloned()
            .collect()
    };
    for job in &jobs {
        super::protocol::token(backend_string(job, "id")?, "task id")?;
        let source = backend_string(job, "session_id")?;
        if super::protocol::canonical_session_id(source).is_none() {
            return Err(RpcError::backend(
                "Task history has an invalid source session",
            ));
        }
        if !bound && source != session_id {
            return Err(RpcError::backend(
                "Conversation job belongs to another session",
            ));
        }
        if iso_seconds(job.get("created_at")).is_none() {
            return Err(RpcError::backend("Job has no valid creation timestamp"));
        }
    }
    if conversation.get("jobs_truncated").and_then(Value::as_bool) == Some(true) {
        return Err(RpcError::unavailable("Claw returned truncated job history"));
    }
    if !bound {
        jobs.sort_by(|left, right| {
            left["created_at"]
                .as_str()
                .cmp(&right["created_at"].as_str())
                .then_with(|| left["id"].as_str().cmp(&right["id"].as_str()))
        });
    }
    Ok(jobs)
}

/// A retained-history projection can omit a newly queued task with no recorded
/// user row. Conversely, it can contain ancestor jobs. Neither is a live task
/// list for the current native session.
pub(super) async fn current_jobs(
    backend: &dyn Backend,
    conversation: &Value,
) -> Result<Vec<Value>, RpcError> {
    if conversation
        .get("task_bindings_complete")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return jobs(backend, conversation).await;
    }
    let canonical = backend_string(conversation, "id")?;
    let result = backend
        .call(Operation::TaskList, json!({ "limit": MAX_HISTORY_ROWS }))
        .await?;
    let records = result
        .get("jobs")
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::backend("Claw returned no current task evidence"))?;
    if records.len() >= MAX_HISTORY_ROWS as usize {
        return Err(RpcError::unavailable(
            "Current task evidence exceeds the bounded owner view",
        ));
    }
    let mut current = conversation.clone();
    current["task_bindings_complete"] = json!(false);
    current["jobs_truncated"] = json!(false);
    current["jobs"] = json!(records
        .iter()
        .filter(|job| job.get("session_id").and_then(Value::as_str) == Some(canonical))
        .cloned()
        .collect::<Vec<_>>());
    jobs(backend, &current).await
}

pub(super) fn active_job(jobs: &[Value]) -> Result<Option<Value>, RpcError> {
    let active: Vec<_> = jobs
        .iter()
        .filter(|job| {
            matches!(
                job.get("status").and_then(Value::as_str),
                Some("pending" | "running" | "waiting_approval")
            )
        })
        .collect();
    match active.as_slice() {
        [] => Ok(None),
        [job] => Ok(Some((*job).clone())),
        _ => Err(RpcError::unavailable(
            "Claw has multiple active tasks for this conversation; resolve them in `cos task` before using the TUI",
        )),
    }
}

pub(super) fn verify_visible_history(conversation: &Value, jobs: &[Value]) -> Result<(), RpcError> {
    let messages = conversation
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::backend("Conversation has no canonical history"))?;
    if messages.len() > MAX_HISTORY_ROWS as usize
        || conversation
            .get("messages_truncated")
            .and_then(Value::as_bool)
            == Some(true)
    {
        return Err(RpcError::unavailable(
            "Claw returned a clipped conversation history",
        ));
    }
    if messages.is_empty() && jobs.is_empty() {
        return Ok(());
    }
    if conversation
        .get("task_bindings_complete")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(RpcError::unavailable(
            "Claw has not verified complete retained task memberships for this history; legacy/unbound rows cannot be replayed as TUI turns",
        ));
    }
    if conversation.get("jobs_truncated").and_then(Value::as_bool) != Some(false)
        || conversation
            .get("messages_truncated")
            .and_then(Value::as_bool)
            != Some(false)
        || conversation.get("message_count").and_then(Value::as_u64) != Some(messages.len() as u64)
        || conversation.get("job_count").and_then(Value::as_u64) != Some(jobs.len() as u64)
    {
        return Err(RpcError::unavailable(
            "Retained task membership views must be complete and untruncated",
        ));
    }
    let canonical = backend_string(conversation, "id")?;
    let mut expected = Vec::new();
    let mut task_ids = HashSet::new();
    for job in jobs {
        let source = backend_string(job, "session_id")?;
        let task = backend_string(job, "id")?;
        if super::protocol::canonical_session_id(source).is_none() || !task_ids.insert(task) {
            return Err(RpcError::backend(
                "Invalid or duplicate retained task identity",
            ));
        }
        if source != canonical {
            if conversation
                .get("parent_id")
                .and_then(Value::as_str)
                .is_none()
            {
                return Err(RpcError::backend(
                    "Inherited task has no canonical parent lineage",
                ));
            }
            if !matches!(
                job.get("status").and_then(Value::as_str),
                Some("ok" | "error" | "cancelled")
            ) {
                return Err(RpcError::unavailable(
                    "An inherited execution is not terminal",
                ));
            }
        }
        expected.push((source, task));
    }
    let mut row_ids = HashSet::new();
    let mut source_ids = HashSet::new();
    let mut outer_ids = HashSet::new();
    let mut memberships = Vec::new();
    let mut observed = Vec::new();
    for row in messages {
        let id = positive_id(row, "id")?;
        let source = backend_string(row, "source_session_id")?;
        let task = backend_string(row, "task_id")?;
        let source_id = positive_id(row, "source_message_id")?;
        let outer_id = positive_id(row, "source_user_message_id")?;
        let is_user = row
            .get("is_user_prompt")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                RpcError::backend("Canonical message has no explicit outer-user flag")
            })?;
        let key = (source, task);
        if !row_ids.insert(id) || !source_ids.insert((source, source_id)) {
            return Err(RpcError::backend("Duplicate retained message identity"));
        }
        if observed.last().copied() != Some(key) {
            if observed.contains(&key) {
                return Err(RpcError::backend(
                    "Retained task memberships are interleaved",
                ));
            }
            observed.push(key);
        }
        if is_user {
            if source_id != outer_id {
                return Err(RpcError::backend(
                    "Outer user message has inconsistent source identity",
                ));
            }
            outer_ids.insert((source, task, source_id));
        }
        memberships.push((source, task, outer_id));
    }
    if observed != expected || memberships.iter().any(|member| !outer_ids.contains(member)) {
        return Err(RpcError::backend(
            "Retained messages do not match their explicit task/outer-user memberships",
        ));
    }
    Ok(())
}

fn positive_id(row: &Value, field: &str) -> Result<i64, RpcError> {
    row.get(field)
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
        .ok_or_else(|| RpcError::backend(format!("Canonical message has no valid {field}")))
}

pub(super) fn mutation_revision(conversation: &Value) -> Result<&str, RpcError> {
    let revision = conversation.get("history_revision").and_then(Value::as_str)
        .filter(|revision| !revision.is_empty() && revision.len() <= 256)
        .ok_or_else(|| RpcError::unavailable(
            "A caller-bound canonical history revision is required to edit the selected turn safely",
        ))?;
    Ok(revision)
}

pub(super) async fn hydrate(
    backend: &dyn Backend,
    thread_id: &str,
    job: &Value,
    items_view: &str,
) -> Result<Value, RpcError> {
    let task_id = backend_string(job, "id")?;
    let session_id = backend_string(job, "session_id")?;
    let mut projection = Projection::new(
        thread_id.to_string(),
        session_id.to_string(),
        task_id.to_string(),
    );
    projection.begin(backend_string(job, "prompt")?, None, job);
    let frame = backend
        .call(
            Operation::TaskStream,
            json!({ "id": task_id, "cursor": 0, "timeout_ms": 0 }),
        )
        .await?;
    let events = frame
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::backend("Claw returned no stored task events"))?;
    if events.len() > 16_384 {
        return Err(RpcError::capacity());
    }
    for event in events {
        projection.record(event)?;
    }
    let final_job = frame
        .get("job")
        .ok_or_else(|| RpcError::backend("Stored task stream has no job"))?;
    if backend_string(final_job, "id")? != task_id
        || backend_string(final_job, "session_id")? != session_id
    {
        return Err(RpcError::backend("Stored task stream identity changed"));
    }
    let mut turn = match frame.get("terminal").and_then(Value::as_bool) {
        Some(true) => projection
            .finish(final_job)?
            .into_iter()
            .find_map(|event| {
                (event["method"] == "turn/completed").then(|| event["params"]["turn"].clone())
            })
            .ok_or_else(|| RpcError::backend("Terminal task has no completed turn"))?,
        Some(false) => projection.snapshot(final_job),
        None => return Err(RpcError::backend("Stored task stream has no terminal flag")),
    };
    match items_view {
        "full" => {}
        "notLoaded" => {
            turn["items"] = json!([]);
        }
        "summary" => {
            let items = turn["items"]
                .as_array()
                .ok_or_else(|| RpcError::backend("Turn has no items"))?;
            let mut summary = items
                .iter()
                .find(|item| item["type"] == "userMessage")
                .cloned()
                .into_iter()
                .collect::<Vec<_>>();
            if let Some(answer) = items
                .iter()
                .rev()
                .find(|item| item["type"] == "agentMessage")
            {
                summary.push(answer.clone());
            }
            turn["items"] = json!(summary);
        }
        _ => return Err(RpcError::params("Invalid itemsView")),
    }
    turn["itemsView"] = json!(items_view);
    if serde_json::to_vec(&turn)
        .map_err(|_| RpcError::backend("Could not encode history"))?
        .len()
        > MAX_MESSAGE_BYTES * 3 / 4
    {
        return Err(RpcError::capacity());
    }
    Ok(turn)
}

/// Only translate a backtrack boundary when the canonical visible user row is
/// unambiguous. Approval retries, old imports and clipped memory must not cause
/// a guessed destructive history edit.
pub(super) fn task_user_turn_range(
    conversation: &Value,
    jobs: &[Value],
    task_id: &str,
) -> Result<(u32, u32, u32), RpcError> {
    verify_visible_history(conversation, jobs)?;
    let messages = conversation
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::backend("Conversation has no canonical messages"))?;
    let job = jobs
        .iter()
        .find(|job| job["id"].as_str() == Some(task_id))
        .ok_or_else(|| RpcError::stale("Backtrack turn does not belong to this conversation"))?;
    let source = backend_string(job, "session_id")?;
    let mut total = 0u32;
    let mut start = None;
    let mut end = 0u32;
    for row in messages {
        if row.get("is_user_prompt").and_then(Value::as_bool) == Some(true) {
            if row.get("task_id").and_then(Value::as_str) == Some(task_id)
                && row.get("source_session_id").and_then(Value::as_str) == Some(source)
            {
                start.get_or_insert(total);
                end = total + 1;
            }
            total += 1;
        }
    }
    Ok((
        start.ok_or_else(|| RpcError::backend("Task has no recorded outer user row"))?,
        end,
        total,
    ))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/history.rs"
    ));
}
