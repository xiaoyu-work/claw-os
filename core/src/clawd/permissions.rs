use serde_json::{json, Value};

use crate::approvals::{self, GrantDuration};
use crate::caps::{Scope, Verb};

pub fn request(params: Value) -> Result<Value, String> {
    let verb_raw = required_string(&params, "verb")?;
    let verb =
        Verb::parse(&verb_raw).ok_or_else(|| format!("unknown capability verb: {verb_raw}"))?;
    let scope_value = params
        .get("scope")
        .ok_or_else(|| "missing required parameter: scope".to_string())?;
    let scope = serde_json::from_value::<Scope>(scope_value.clone())
        .map_err(|err| format!("invalid scope: {err}"))?;
    let session = params
        .get("session")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "clawd".to_string());
    let reason = required_string(&params, "reason")?;
    let requester = params
        .get("requester")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let id = approvals::submit(
        verb,
        scope.clone(),
        session.clone(),
        reason.clone(),
        requester,
    )
    .map_err(|err| err.to_string())?;
    Ok(json!({
        "id": id,
        "status": "pending",
        "verb": verb.as_str(),
        "scope": scope,
        "session": session,
        "reason": reason,
    }))
}

pub fn pending(params: Value) -> Result<Value, String> {
    let limit = optional_limit(&params)?;
    let mut requests = approvals::list_pending();
    requests.truncate(limit);
    Ok(json!({ "requests": requests }))
}

pub fn recent(params: Value) -> Result<Value, String> {
    let limit = optional_limit(&params)?;
    let requests = approvals::list_recent(limit);
    Ok(json!({ "requests": requests }))
}

pub fn decide(params: Value) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let decision = required_string(&params, "decision")?;
    let decided_by = params
        .get("decided_by")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "clawd".to_string());
    let note = params
        .get("note")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    match decision.trim().to_ascii_lowercase().as_str() {
        "approve" | "allow" => {
            if approvals::lookup_pending(&id).is_none() {
                return Err(format!("permission request not pending: {id}"));
            }
            let resolved =
                approvals::approve(&id, duration_from_params(&params)?, Some(decided_by), note)?;
            Ok(json!({
                "id": resolved.request.id,
                "decision": "approved",
                "duration": resolved.decision.duration,
            }))
        }
        "deny" | "reject" => {
            let resolved = approvals::deny(&id, Some(decided_by), note)?;
            Ok(json!({
                "id": resolved.request.id,
                "decision": "denied",
            }))
        }
        other => Err(format!("unknown permission decision: {other}")),
    }
}

fn duration_from_params(params: &Value) -> Result<GrantDuration, String> {
    let raw = params
        .get("duration")
        .and_then(Value::as_str)
        .unwrap_or("once")
        .trim()
        .to_ascii_lowercase();

    match raw.as_str() {
        "once" => Ok(GrantDuration::Once),
        "session" | "task" => Ok(GrantDuration::Session),
        "forever" | "always" => Ok(GrantDuration::Forever),
        other => Err(format!("unknown permission grant duration: {other}")),
    }
}

fn optional_limit(params: &Value) -> Result<usize, String> {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| {
            usize::try_from(limit)
                .map_err(|_| format!("limit is too large for this platform: {limit}"))
        })
        .transpose()
        .map(|limit| limit.unwrap_or(100))
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("missing required string parameter: {key}"))
}
