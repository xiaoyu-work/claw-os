use std::collections::VecDeque;
use std::fs;
use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};

use crate::agent::service::Job;
use crate::approvals::{Request as ApprovalRequest, Resolved as ResolvedApproval};
use crate::audit_policy::{self, ProtocolFailureFacts, RequestFacts, ResponseFacts};

use super::client_identity::ClientIdentity;

/// Journal the broker's own view of a dispatched request.
///
/// This journal is a user-visible projection that also feeds agent
/// context, so it is handed exactly the [`RequestFacts`] the broker
/// audit log receives. Neither sink can see the raw `params`.
pub fn record_clawd_request(
    request: &RequestFacts,
    outcome: &ResponseFacts,
    duration: Duration,
    client: &ClientIdentity,
) {
    let _ = append(clawd_request_record(request, outcome, duration, client));
}

fn clawd_request_record(
    request: &RequestFacts,
    outcome: &ResponseFacts,
    duration: Duration,
    client: &ClientIdentity,
) -> Value {
    json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "clawd.request",
        "operation": request.command,
        "ok": outcome.ok,
        "duration_ms": duration.as_millis(),
        "client": client,
        "request": request,
        "error": outcome.error,
    })
}

pub fn record_protocol_failure(
    request: &ProtocolFailureFacts,
    outcome: &ResponseFacts,
    duration: Duration,
    client: &ClientIdentity,
) {
    let _ = append(protocol_failure_record(request, outcome, duration, client));
}

fn protocol_failure_record(
    request: &ProtocolFailureFacts,
    outcome: &ResponseFacts,
    duration: Duration,
    client: &ClientIdentity,
) -> Value {
    json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "clawd.protocol-failure",
        "operation": request.class,
        "ok": outcome.ok,
        "duration_ms": duration.as_millis(),
        "client": client,
        "request": request,
        "error": outcome.error,
    })
}

pub fn record_cap_decision(entry: &Value) {
    let record = json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "caps.require",
        "operation": entry.get("verb").cloned().unwrap_or(Value::Null),
        "ok": entry
            .get("decision")
            .and_then(Value::as_str)
            .map(|decision| decision == "allow")
            .unwrap_or(false),
        "session_id": entry.get("session_id").cloned().unwrap_or(Value::Null),
        "pid": entry.get("pid").cloned().unwrap_or(Value::Null),
        "agent": entry.get("agent").cloned().unwrap_or(Value::Null),
        "verb": entry.get("verb").cloned().unwrap_or(Value::Null),
        "scope": entry.get("scope").cloned().unwrap_or(Value::Null),
        "target_resource": entry.get("target_resource").cloned().unwrap_or(Value::Null),
        "decision": entry.get("decision").cloned().unwrap_or(Value::Null),
        "reason": entry.get("reason").cloned().unwrap_or(Value::Null),
        "hint": entry.get("hint").cloned().unwrap_or(Value::Null),
        "mode": entry.get("mode").cloned().unwrap_or(Value::Null),
        "owner_uid": entry.get("owner_uid").cloned().unwrap_or(Value::Null),
    });
    let _ = append(record);
}

pub fn record_approval_request(request: &ApprovalRequest) {
    let _ = append(approval_request_record(request));
}

fn approval_request_record(request: &ApprovalRequest) -> Value {
    json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "permission.request",
        "operation": &request.verb,
        "ok": true,
        "approval_id": audit_policy::safe_reference(&request.id),
        "session_id": audit_policy::safe_reference(&request.session),
        "verb": &request.verb,
        "scope": &request.scope,
        // Free text the requester supplied for the user's prompt; the
        // approvals store keeps it, this projection does not.
        "reason": audit_policy::text_digest(&request.reason),
        "requester": &request.requester,
        "owner_uid": request.owner_uid,
    })
}

pub fn record_approval_decision(resolved: &ResolvedApproval) {
    let _ = append(approval_decision_record(resolved));
}

fn approval_decision_record(resolved: &ResolvedApproval) -> Value {
    json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "permission.decision",
        "operation": &resolved.request.verb,
        "ok": resolved.decision.outcome == crate::approvals::Outcome::Approved,
        "approval_id": audit_policy::safe_reference(&resolved.request.id),
        "session_id": audit_policy::safe_reference(&resolved.request.session),
        "verb": &resolved.request.verb,
        "scope": &resolved.request.scope,
        "outcome": resolved.decision.outcome,
        "duration": resolved.decision.duration,
        "decided_by": &resolved.decision.decided_by,
        // The approver's free-text note is never journalled.
        "note": resolved
            .decision
            .note
            .as_deref()
            .map(audit_policy::text_digest),
        "owner_uid": resolved.request.owner_uid,
    })
}

pub fn record_task_event(event: &'static str, job: &Job) {
    let _ = append(task_event_record(event, job));
}

fn task_event_record(event: &'static str, job: &Job) -> Value {
    json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "clawd.task",
        "operation": event,
        "ok": job.error.is_none(),
        "job_id": audit_policy::safe_identity(&job.id),
        "status": job.status.as_str(),
        "session_id": job.session_id.as_deref().map(audit_policy::safe_reference),
        "worker_pid": job.worker_pid,
        "worker_start_time_ticks": job.worker_start_time_ticks,
        "provider": job.provider.as_deref().map(audit_policy::safe_identity),
        "model": job.model.as_deref().map(audit_policy::safe_identity),
        // Worker failures quote provider responses and prompt text.
        "error": audit_policy::optional_text_digest(job.error.as_deref()),
        "owner_uid": job.owner_uid,
    })
}

pub fn record_power_intent(
    action: &str,
    owner_uid: u32,
    session_id: &str,
) -> Result<(), String> {
    append(json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "system.power",
        "operation": action,
        "phase": "intent",
        "ok": true,
        "session_id": session_id,
        "owner_uid": owner_uid,
    }))
}

pub fn query(params: Value) -> Result<Value, String> {
    query_with_owner(params, None)
}

pub fn query_for_client(
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    let uid = client.require_uid()?;
    query_with_owner(params, (uid != 0).then_some(uid))
}

fn query_with_owner(params: Value, owner_uid: Option<u32>) -> Result<Value, String> {
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(100)
        .clamp(1, 1_000);
    let source = params
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let operations = recent_operations(limit, source.as_deref(), owner_uid)?;
    Ok(json!({
        "schema": 1,
        "path": crate::paths::system_operations_log_path(),
        "limit": limit,
        "source": source,
        "operations": operations,
    }))
}

pub fn context_payload(limit: usize) -> Value {
    let operations = recent_operations(limit, None, None).unwrap_or_default();
    json!({
        "path": crate::paths::system_operations_log_path(),
        "recent": operations,
        "recent_limit": limit,
    })
}

fn append(record: Value) -> Result<(), String> {
    let line = serde_json::to_string(&record).map_err(|err| err.to_string())?;
    let path = crate::paths::system_operations_log_path();
    crate::filelock::append_locked(&path, &line).map_err(|err| {
        format!(
            "failed to write system operation journal {}: {err}",
            path.display()
        )
    })
}

fn recent_operations(
    limit: usize,
    source: Option<&str>,
    owner_uid: Option<u32>,
) -> Result<Vec<Value>, String> {
    let path = crate::paths::system_operations_log_path();
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(format!(
                "failed to read system operation journal {}: {err}",
                path.display()
            ));
        }
    };
    let mut out = VecDeque::with_capacity(limit);
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(source) = source {
            if value.get("source").and_then(Value::as_str) != Some(source) {
                continue;
            }
        }
        if !operation_visible_to(&value, owner_uid) {
            continue;
        }
        if out.len() == limit {
            out.pop_front();
        }
        out.push_back(value);
    }
    Ok(out.into_iter().collect())
}

fn operation_visible_to(value: &Value, owner_uid: Option<u32>) -> bool {
    let Some(uid) = owner_uid else {
        return true;
    };
    value
        .pointer("/client/uid")
        .and_then(Value::as_u64)
        .is_some_and(|value| value == uid as u64)
        || value
            .get("owner_uid")
            .and_then(Value::as_u64)
            .is_some_and(|value| value == uid as u64)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/system_journal.rs"
    ));
}
