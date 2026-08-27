use serde_json::{json, Value};

use crate::approvals::{self, GrantDuration};
use crate::caps::{Scope, Verb};

use super::client_identity::ClientIdentity;

pub fn request(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let uid = client.require_uid()?;
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
    let requester = Some(format!("uid:{uid}"));

    let id = approvals::submit_owned(
        verb,
        scope.clone(),
        session.clone(),
        reason.clone(),
        requester,
        Some(uid),
    )
    .map_err(|err| err.to_string())?;
    Ok(json!({
        "id": id,
        "status": "pending",
        "verb": verb.as_str(),
        "scope": scope,
        "session": session,
        "reason": reason,
        "owner_uid": uid,
    }))
}

pub fn pending(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let limit = optional_limit(&params)?;
    let mut requests = approvals::list_pending_for_owner(owner_filter(client)?);
    requests.truncate(limit);
    Ok(json!({ "requests": requests }))
}

pub fn recent(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let limit = optional_limit(&params)?;
    let requests = approvals::list_recent_for_owner(limit, owner_filter(client)?);
    Ok(json!({ "requests": requests }))
}

/// Decision state for a set of request ids this caller already knows.
///
/// A denied App launch is handed the ids of the requests it filed and
/// waits here for the user's decision. Only the state is returned, and
/// only for requests visible to this owner, so nothing about another
/// launcher's request leaks — and knowing an id authorises nothing:
/// grants are still matched against the daemon-derived launcher
/// identity when the launch is retried.
pub fn status(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let owner = owner_filter(client)?;
    let ids = match params.get("ids") {
        Some(Value::Array(items)) => items,
        _ => return Err("ids must be an array of approval request ids".to_string()),
    };
    if ids.len() > 64 {
        return Err("too many approval request ids".to_string());
    }
    let mut statuses = Vec::with_capacity(ids.len());
    for id in ids {
        let id = id
            .as_str()
            .ok_or_else(|| "ids must contain only strings".to_string())?;
        statuses.push(json!({
            "id": id,
            "status": approvals::status_for_owner(id, owner).as_str(),
        }));
    }
    Ok(json!({ "statuses": statuses }))
}

pub fn decide(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    if client.require_uid()? != 0 {
        return Err("permission decisions require the privileged approval helper".to_string());
    }
    let id = required_string(&params, "id")?;
    let decision = required_string(&params, "decision")?;
    let owner_uid = params
        .get("owner_uid")
        .and_then(Value::as_u64)
        .map(|uid| u32::try_from(uid).map_err(|_| format!("owner_uid is too large: {uid}")))
        .transpose()?;
    let decided_by = owner_uid
        .map(|uid| format!("uid:{uid}"))
        .unwrap_or_else(|| "uid:0".to_string());
    let note = params
        .get("note")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    match decision.trim().to_ascii_lowercase().as_str() {
        "approve" | "allow" => {
            let resolved = approvals::approve_for_owner(
                &id,
                duration_from_params(&params)?,
                Some(decided_by),
                note,
                owner_uid,
            )?;
            Ok(json!({
                "id": resolved.request.id,
                "decision": "approved",
                "duration": resolved.decision.duration,
            }))
        }
        "deny" | "reject" => {
            let resolved = approvals::deny_for_owner(&id, Some(decided_by), note, owner_uid)?;
            Ok(json!({
                "id": resolved.request.id,
                "decision": "denied",
            }))
        }
        other => Err(format!("unknown permission decision: {other}")),
    }
}

fn owner_filter(client: &ClientIdentity) -> Result<Option<u32>, String> {
    let uid = client.require_uid()?;
    Ok((uid != 0).then_some(uid))
}

/// Retire every reusable approval in a scope.
///
/// Root-only, and the route's access class already enforces that, so
/// `owner_uid` here is the privileged helper naming the account it
/// authenticated rather than a peer choosing somebody else's authority.
/// The check is repeated because this function decides whose standing
/// permission disappears.
///
/// Revocation is an increment of a root-owned generation counter, not a
/// flag on a record: every approval minted under an older generation
/// stops being authority immediately, and restoring one of those files
/// from a backup does not bring it back.
pub fn revoke(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    if client.require_uid()? != 0 {
        return Err("permission revocation requires the privileged approval helper".to_string());
    }
    let owner_uid = params
        .get("owner_uid")
        .and_then(Value::as_u64)
        .map(|uid| u32::try_from(uid).map_err(|_| format!("owner_uid is too large: {uid}")))
        .transpose()?;
    let session = params
        .get("session")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let scope = match session.clone() {
        Some(session) => approvals::RevocationScope::Session {
            uid: owner_uid,
            session,
        },
        None => approvals::RevocationScope::Owner { uid: owner_uid },
    };
    let generation = approvals::generations::revoke(&scope)?;
    crate::clawd::audit::record_approval_revocation(
        &scope,
        session.as_deref().unwrap_or("*"),
        generation,
    );
    Ok(json!({
        "revoked": true,
        "scope": scope.kind(),
        "generation": generation,
        "owner_uid": owner_uid,
    }))
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
