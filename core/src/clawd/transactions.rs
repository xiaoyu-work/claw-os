use chrono::Utc;
use serde_json::{json, Value};
use std::str::FromStr;

use crate::caps::Role;
use crate::session::{self, RollbackOutcome, SessionId, SessionOrigin, Status as SessionStatus};

use super::client_identity::ClientIdentity;
use super::state::{DaemonState, TransactionHandle};

pub fn begin(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    // Canonical, ownership-checked home — the same derivation the
    // capability baseline and the execution-time clamp use.
    let owner_home = super::system_caps::verified_owner_home(owner_uid)?;
    let purpose = required_string(&params, "purpose")?;
    let session_id = session::create(&purpose).map_err(|err| err.to_string())?;
    if let Err(error) = session::update_meta(&session_id, |meta| {
        meta.creator_runtime = Some("clawd-transaction-pending".to_string());
        meta.role = Some(Role::Observer);
        meta.owner_uid = Some(owner_uid);
        meta.origin = Some(SessionOrigin::SystemAgentTask);
        meta.status = SessionStatus::Running;
    }) {
        return Err(fail_new_session(
            &session_id,
            "initialize transaction metadata",
            error.to_string(),
        ));
    }
    let caps = super::system_caps::system_agent_caps(owner_uid, &owner_home);
    if let Err(error) = session::set_caps(&session_id, &caps) {
        return Err(fail_new_session(
            &session_id,
            "set transaction capabilities",
            error.to_string(),
        ));
    }

    let lease = match session::try_acquire(&session_id) {
        Ok(lease) => lease,
        Err(error) => {
            return Err(fail_new_session(
                &session_id,
                "acquire transaction lease",
                error.to_string(),
            ));
        }
    };
    if let Err(error) = session::update_meta(&session_id, |meta| {
        meta.creator_runtime = Some("clawd-transaction".to_string());
        meta.status = SessionStatus::Running;
    }) {
        drop(lease);
        let cleanup = session::end(&session_id, SessionStatus::Failed);
        return Err(match cleanup {
            Ok(()) => format!("activate transaction metadata: {error}"),
            Err(cleanup) => format!(
                "activate transaction metadata: {error}; marking session failed: {cleanup}"
            ),
        });
    }
    let handle = TransactionHandle {
        session_id: session_id.clone(),
        purpose: purpose.clone(),
        started_at: Utc::now(),
        owner_uid,
        lease,
    };
    if let Err(error) = state.insert_transaction(handle) {
        let cleanup = session::end(&session_id, SessionStatus::Failed);
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup) => format!(
                "{error}; marking orphaned transaction session failed: {cleanup}"
            ),
        });
    }

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

    if let Err(error) = session::end(&handle.session_id, SessionStatus::Done) {
        return Err(restore_handle_after_error(
            state,
            handle,
            "commit",
            error.to_string(),
        ));
    }
    drop(handle);

    Ok(json!({
        "id": session_id.as_str(),
        "status": "committed",
    }))
}

pub async fn rollback(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let session_id = parse_session_id(&id)?;
    let owner_uid = owner_filter(client)?;
    state.require_transaction_owner(session_id.as_str(), owner_uid)?;
    let session_info =
        super::session_scope::trusted_session_info(&session_id, "clawd-rollback")
        .map_err(|err| format!("transaction rollback session scope: {err}"))?;
    crate::proc::with_trusted_session_override(
        session_info,
        async { rollback_scoped(state, session_id, owner_uid) },
    )
    .await
}

fn rollback_scoped(
    state: &DaemonState,
    session_id: SessionId,
    owner_uid: Option<u32>,
) -> Result<Value, String> {
    let handle = state
        .take_transaction_for_owner(session_id.as_str(), owner_uid)?
        .ok_or_else(|| format!("transaction is not active: {}", session_id.as_str()))?;
    let rolled_back = match session::rollback(&session_id) {
        Ok(rolled_back) => rolled_back,
        Err(error) => {
            return Err(restore_handle_after_error(
                state,
                handle,
                "rollback",
                error.to_string(),
            ));
        }
    };
    let incomplete = rolled_back
        .iter()
        .filter(|entry| {
            matches!(
                entry.status,
                crate::session::RollbackStatus::Failed
                    | crate::session::RollbackStatus::Skipped
            )
        })
        .map(|entry| format!("{}#{}: {}", entry.verb, entry.seq, entry.detail))
        .collect::<Vec<_>>();
    if !incomplete.is_empty() {
        return Err(restore_handle_after_error(
            state,
            handle,
            "rollback",
            format!("incomplete entries: {}", incomplete.join("; ")),
        ));
    }
    let entries = rolled_back.into_iter().map(entry_value).collect::<Vec<_>>();

    if let Err(error) = session::end(&handle.session_id, SessionStatus::Failed) {
        return Err(restore_handle_after_error(
            state,
            handle,
            "finish rollback",
            error.to_string(),
        ));
    }
    drop(handle);

    Ok(json!({
        "id": session_id.as_str(),
        "status": "rolled_back",
        "entries": entries,
    }))
}

fn fail_new_session(session_id: &SessionId, stage: &str, error: String) -> String {
    match session::end(session_id, SessionStatus::Failed) {
        Ok(()) => format!("{stage}: {error}"),
        Err(cleanup) => format!("{stage}: {error}; marking session failed: {cleanup}"),
    }
}

fn restore_handle_after_error(
    state: &DaemonState,
    handle: TransactionHandle,
    operation: &str,
    error: String,
) -> String {
    match state.insert_transaction(handle) {
        Ok(()) => format!("transaction {operation} failed: {error}"),
        Err(restore) => {
            format!("transaction {operation} failed: {error}; restoring handle failed: {restore}")
        }
    }
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
