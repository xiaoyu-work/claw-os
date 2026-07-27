use chrono::Utc;
use serde_json::{json, Value};
use std::str::FromStr;

use crate::caps::Role;
use crate::session::{self, RollbackOutcome, SessionId, Status as SessionStatus};

use super::client_identity::ClientIdentity;
use super::state::{DaemonState, TransactionHandle};

pub fn begin(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    let purpose = required_string(&params, "purpose")?;
    let session_id = session::create(&purpose).map_err(|err| err.to_string())?;
    session::update_meta(&session_id, |meta| {
        meta.creator_runtime = Some("clawd".to_string());
        meta.role = Some(Role::Observer);
        meta.status = SessionStatus::Running;
    })
    .map_err(|err| err.to_string())?;
    let caps = super::system_caps::readonly_task_caps();
    session::set_caps(&session_id, &caps).map_err(|err| err.to_string())?;

    let lease = session::try_acquire(&session_id).map_err(|err| err.to_string())?;
    state.insert_transaction(TransactionHandle {
        session_id: session_id.clone(),
        purpose: purpose.clone(),
        started_at: Utc::now(),
        owner_uid,
        lease,
    })?;

    Ok(json!({
        "id": session_id.as_str(),
        "purpose": purpose,
        "status": "running",
        "owner_uid": owner_uid,
    }))
}

pub fn list(state: &DaemonState, client: &ClientIdentity) -> Result<Value, String> {
    let transactions = state
        .list_transactions_for_owner(owner_filter(client)?)
        .into_iter()
        .map(|tx| {
            json!({
                "id": tx.id,
                "purpose": tx.purpose,
                "started_at": tx.started_at,
                "status": "running",
                "owner_uid": tx.owner_uid,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({ "transactions": transactions }))
}

pub fn commit(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let session_id = parse_session_id(&id)?;
    let handle = state
        .take_transaction_for_owner(session_id.as_str(), owner_filter(client)?)?
        .ok_or_else(|| format!("transaction is not active: {}", session_id.as_str()))?;

    session::end(&handle.session_id, SessionStatus::Done).map_err(|err| err.to_string())?;
    drop(handle);

    Ok(json!({
        "id": session_id.as_str(),
        "status": "committed",
    }))
}

pub fn rollback(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let session_id = parse_session_id(&id)?;
    let owner_uid = owner_filter(client)?;
    state.require_transaction_owner(session_id.as_str(), owner_uid)?;
    let _scope = super::session_scope::ProcSessionGuard::enter(&session_id, "clawd-rollback")
        .map_err(|err| format!("transaction rollback session scope: {err}"))?;
    let handle = state
        .take_transaction_for_owner(session_id.as_str(), owner_uid)?
        .ok_or_else(|| format!("transaction is not active: {}", session_id.as_str()))?;
    let rolled_back = session::rollback(&session_id).map_err(|err| err.to_string())?;
    let entries = rolled_back.into_iter().map(entry_value).collect::<Vec<_>>();

    session::end(&handle.session_id, SessionStatus::Failed).map_err(|err| err.to_string())?;
    drop(handle);

    Ok(json!({
        "id": session_id.as_str(),
        "status": "rolled_back",
        "entries": entries,
    }))
}

fn owner_filter(client: &ClientIdentity) -> Result<Option<u32>, String> {
    let uid = client.require_uid()?;
    Ok((uid != 0).then_some(uid))
}

fn entry_value(entry: RollbackOutcome) -> Value {
    serde_json::to_value(entry).unwrap_or_else(|err| {
        json!({
            "kind": "serialization_error",
            "error": err.to_string(),
        })
    })
}

fn parse_session_id(raw: &str) -> Result<SessionId, String> {
    SessionId::from_str(raw).map_err(|err| err.to_string())
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
