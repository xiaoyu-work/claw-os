use std::collections::HashMap;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::backend::{Backend, Operation, ReviewDecision};
use super::protocol::{
    backend_string, notification, safe_text, RequestId, RpcError, MAX_PENDING_APPROVALS,
};

#[derive(Clone, Debug)]
struct Pending {
    id: String,
    task_id: String,
    session_id: String,
    thread_id: String,
}

#[derive(Default)]
pub(super) struct Approvals {
    pending: Mutex<HashMap<RequestId, Pending>>,
}

impl Approvals {
    pub async fn present(
        &self,
        backend: &dyn Backend,
        thread_id: &str,
        session_id: &str,
        task_id: &str,
        ids: &[String],
    ) -> Result<Vec<Value>, RpcError> {
        let result = backend
            .call(Operation::PermissionPending, json!({ "limit": 1000 }))
            .await?;
        let requests = result
            .get("requests")
            .and_then(Value::as_array)
            .ok_or_else(|| RpcError::backend("Claw approval list has no requests"))?;
        let mut messages = Vec::new();
        let mut pending = self.pending.lock().await;
        for id in ids {
            if pending
                .values()
                .any(|request| request.id == *id && request.task_id == task_id)
            {
                continue;
            }
            if pending.len() >= MAX_PENDING_APPROVALS {
                return Err(RpcError::capacity());
            }
            let Some(request) = requests.iter().find(|request| request["id"] == *id) else {
                messages.push(notification("warning", json!({
                    "threadId": thread_id,
                    "message": "A Claw approval changed or is not present in the bounded pending view; review it with `cos approval pending`.",
                })));
                continue;
            };
            let verb = backend_string(request, "verb")?;
            let scope = request
                .get("scope")
                .ok_or_else(|| RpcError::backend("Approval request has no scope"))?;
            let reason = backend_string(request, "reason")?;
            let request_id = RequestId::String(format!("claw-approval-{}", uuid::Uuid::new_v4()));
            let question = safe_text(&format!(
                "Claw requests {verb} on {scope}.\n{reason}\nRequest: {id}\nChoosing Authorize once opens the system authorization helper. Only its root-owned decision can grant this exact request."
            ));
            messages.push(json!({
                "id": request_id,
                "method": "item/tool/requestUserInput",
                "params": {
                    "threadId": thread_id,
                    "turnId": task_id,
                    "itemId": format!("{task_id}:approval:{id}"),
                    "isBlocking": true,
                    "autoResolutionMs": null,
                    "questions": [{
                        "id": id,
                        "header": "Permission",
                        "question": question,
                        "isOther": false,
                        "isSecret": false,
                        "options": [
                            { "label": "Authorize once", "description": "Ask the system to authorize only this pending capability request" },
                            { "label": "Deny", "description": "Ask the system to deny this request" },
                        ],
                    }],
                },
            }));
            pending.insert(
                request_id,
                Pending {
                    id: id.clone(),
                    task_id: task_id.to_string(),
                    session_id: session_id.to_string(),
                    thread_id: thread_id.to_string(),
                },
            );
        }
        Ok(messages)
    }

    pub async fn respond(
        &self,
        backend: &dyn Backend,
        request_id: RequestId,
        result: Result<Value, Value>,
    ) -> Result<Vec<Value>, RpcError> {
        let pending = self
            .pending
            .lock()
            .await
            .remove(&request_id)
            .ok_or_else(|| RpcError::stale("Unknown or already answered approval request"))?;
        let result = result.map_err(|_| {
            RpcError::failed("The TUI did not answer the approval. Review the unchanged root request with `cos approval pending`")
        })?;
        let answers = result
            .as_object()
            .filter(|object| object.len() == 1)
            .and_then(|object| object.get("answers"))
            .and_then(Value::as_object)
            .filter(|answers| answers.len() == 1)
            .and_then(|answers| answers.get(&pending.id))
            .and_then(Value::as_object)
            .filter(|answer| answer.len() == 1)
            .and_then(|answer| answer.get("answers"))
            .and_then(Value::as_array)
            .filter(|answers| answers.len() == 1)
            .ok_or_else(|| {
                RpcError::params(
                    "Approval requires exactly one answer for the pending root request",
                )
            })?;
        let decision = match answers[0].as_str() {
            Some("Authorize once") => ReviewDecision::ApproveOnce,
            Some("Deny") => ReviewDecision::Deny,
            _ => {
                return Err(RpcError::params(
                    "Unsupported approval answer; no decision was made",
                ))
            }
        };
        let job = backend
            .call(Operation::TaskGet, json!({ "id": pending.task_id }))
            .await?;
        if job.get("session_id").and_then(Value::as_str) != Some(&pending.session_id)
            || job.get("status").and_then(Value::as_str) != Some("waiting_approval")
            || !job
                .get("waiting_on")
                .and_then(Value::as_array)
                .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(&pending.id)))
        {
            return Err(RpcError::stale(
                "The task is no longer waiting for this approval",
            ));
        }
        let statuses = backend
            .call(Operation::PermissionStatus, json!({ "ids": [pending.id] }))
            .await?;
        if !has_status(&statuses, &pending.id, "pending") {
            return Err(RpcError::stale(
                "The root approval request is no longer pending",
            ));
        }
        backend.review(&pending.id, decision).await?;
        let statuses = backend
            .call(Operation::PermissionStatus, json!({ "ids": [pending.id] }))
            .await?;
        let expected = match decision {
            ReviewDecision::ApproveOnce => "approved",
            ReviewDecision::Deny => "denied",
        };
        if !has_status(&statuses, &pending.id, expected) {
            return Err(RpcError::failed(
                "The root decision could not be confirmed; no approval is inferred",
            ));
        }
        Ok(vec![notification(
            "serverRequest/resolved",
            json!({
                "threadId": pending.thread_id,
                "requestId": request_id,
            }),
        )])
    }

    pub async fn resolve_task(&self, task_id: &str) -> Vec<Value> {
        let mut pending = self.pending.lock().await;
        let resolved: Vec<RequestId> = pending
            .iter()
            .filter(|(_, pending)| pending.task_id == task_id)
            .map(|(id, _)| id.clone())
            .collect();
        resolved
            .into_iter()
            .filter_map(|id| {
                pending.remove(&id).map(|pending| {
                    notification(
                        "serverRequest/resolved",
                        json!({
                            "threadId": pending.thread_id,
                            "requestId": id,
                        }),
                    )
                })
            })
            .collect()
    }
}

fn has_status(value: &Value, id: &str, expected: &str) -> bool {
    value
        .get("statuses")
        .and_then(Value::as_array)
        .is_some_and(|statuses| {
            statuses.iter().any(|status| {
                status.get("id").and_then(Value::as_str) == Some(id)
                    && status.get("status").and_then(Value::as_str) == Some(expected)
            })
        })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/approvals.rs"
    ));
}
