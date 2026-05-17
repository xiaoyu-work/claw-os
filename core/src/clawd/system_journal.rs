use std::collections::VecDeque;
use std::fs;
use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};

use crate::agent::service::Job;
use crate::approvals::{Request as ApprovalRequest, Resolved as ResolvedApproval};

use super::client_identity::ClientIdentity;
use super::protocol::Response;

pub fn record_clawd_request(
    command: &str,
    params: &Value,
    response: &Response,
    duration: Duration,
    client: &ClientIdentity,
) {
    let error = response.error.as_ref().map(|err| {
        json!({
            "code": err.code,
            "message": err.message,
        })
    });
    let record = json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "clawd.request",
        "operation": command,
        "ok": response.ok,
        "duration_ms": duration.as_millis(),
        "client": client,
        "params": params,
        "error": error,
    });
    let _ = append(record);
}

pub fn record_invalid_request(
    raw: &str,
    response: &Response,
    duration: Duration,
    client: &ClientIdentity,
) {
    let error = response.error.as_ref().map(|err| {
        json!({
            "code": err.code,
            "message": err.message,
        })
    });
    let record = json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "clawd.invalid-request",
        "operation": "invalid_json",
        "ok": false,
        "duration_ms": duration.as_millis(),
        "client": client,
        "raw": raw,
        "error": error,
    });
    let _ = append(record);
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
    });
    let _ = append(record);
}

pub fn record_approval_request(request: &ApprovalRequest) {
    let record = json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "permission.request",
        "operation": &request.verb,
        "ok": true,
        "approval_id": &request.id,
        "session_id": &request.session,
        "verb": &request.verb,
        "scope": &request.scope,
        "reason": &request.reason,
        "requester": &request.requester,
    });
    let _ = append(record);
}

pub fn record_approval_decision(resolved: &ResolvedApproval) {
    let record = json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "permission.decision",
        "operation": &resolved.request.verb,
        "ok": resolved.decision.outcome == crate::approvals::Outcome::Approved,
        "approval_id": &resolved.request.id,
        "session_id": &resolved.request.session,
        "verb": &resolved.request.verb,
        "scope": &resolved.request.scope,
        "outcome": resolved.decision.outcome,
        "duration": resolved.decision.duration,
        "decided_by": &resolved.decision.decided_by,
    });
    let _ = append(record);
}

pub fn record_task_event(event: &'static str, job: &Job) {
    let record = json!({
        "ts": Utc::now(),
        "event": "system.operation",
        "source": "clawd.task",
        "operation": event,
        "ok": job.error.is_none(),
        "job_id": &job.id,
        "status": job.status.as_str(),
        "session_id": &job.session_id,
        "worker_pid": job.worker_pid,
        "provider": &job.provider,
        "model": &job.model,
        "error": &job.error,
    });
    let _ = append(record);
}

pub fn query(params: Value) -> Result<Value, String> {
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
    let operations = recent_operations(limit, source.as_deref())?;
    Ok(json!({
        "schema": 1,
        "path": crate::paths::system_operations_log_path(),
        "limit": limit,
        "source": source,
        "operations": operations,
    }))
}

pub fn context_payload(limit: usize) -> Value {
    let operations = recent_operations(limit, None).unwrap_or_default();
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

fn recent_operations(limit: usize, source: Option<&str>) -> Result<Vec<Value>, String> {
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
        if out.len() == limit {
            out.pop_front();
        }
        out.push_back(value);
    }
    Ok(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_returns_recent_operations() {
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("COS_DATA_DIR");
        std::env::set_var("COS_DATA_DIR", tmp.path());

        let client = ClientIdentity::unknown();
        let response = Response::ok(None, json!({"status": "ok"}));
        record_clawd_request(
            "daemon.health",
            &Value::Null,
            &response,
            Duration::from_millis(3),
            &client,
        );
        let result = query(json!({"limit": 10})).unwrap();
        assert_eq!(result["operations"][0]["source"], "clawd.request");
        assert_eq!(result["operations"][0]["operation"], "daemon.health");

        match prev {
            Some(value) => std::env::set_var("COS_DATA_DIR", value),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}
