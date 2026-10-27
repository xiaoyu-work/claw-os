//! `memory.*` clawd commands — owner-scoped views and learned-memory reset.
//!
//! Used by the desktop agent UI (via `cos-agent-bridge`) to load
//! historical conversation rows so users can resume chats across
//! restarts. Same parser as the web `GET /api/sessions/:id/history`
//! route — both reuse `agent::memory::history`.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::mpsc;

use crate::agent::memory::history::load_history;
use crate::agent::memory::maintenance;
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::agent::service::{JobStatus, Store};
use crate::agentd::spawn::ROOT_OWNER_REFUSAL;

use super::client_identity::{ClientIdentity, FsIdentityGuard};

pub(super) mod inspection;

const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 2000;
const DEFAULT_SESSION_LIMIT: usize = 200;
const MAX_SESSION_LIMIT: usize = 1000;

/// `memory.history` — return the most recent rows for `session_id`,
/// each pre-parsed into `{ role, text, tool_calls, tool_results, ts_ms }`.
pub fn history(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let session_id = params
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "session_id is required".to_string())?;

    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);

    let history_session_id = session_id.to_string();
    let messages = read_client_db(client, move |db| {
        db.map(|db| load_history(db, &history_session_id, limit))
            .transpose()
            .map(|messages| messages.unwrap_or_default())
            .map_err(|err| format!("read history: {err}"))
    })?;

    Ok(json!({
        "session_id": session_id,
        "n": messages.len(),
        "messages": messages,
    }))
}

/// `memory.sessions` — list persisted chat sessions newest-first. Each
/// entry carries `{ id, title, last_ts_ms, message_count }`, matching
/// the shape the web `/api/sessions` route returns so the desktop UI
/// can drive a sidebar identical to the web client.
pub fn sessions(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_SESSION_LIMIT)
        .clamp(1, MAX_SESSION_LIMIT);

    let rows = read_client_db(client, move |db| {
        db.map(|db| db.sessions(limit))
            .transpose()
            .map(|sessions| sessions.unwrap_or_default())
            .map_err(|err| format!("list sessions: {err}"))
    })?;

    let sessions: Vec<Value> = rows
        .into_iter()
        .map(|s| {
            json!({
                "id": s.session_id,
                "title": s.title,
                "last_ts_ms": s.last_ts_ms,
                "message_count": s.message_count,
            })
        })
        .collect();

    Ok(json!({
        "n": sessions.len(),
        "sessions": sessions,
    }))
}

/// `memory.reset` — clear learned notes, App memory and the derived semantic
/// index while retaining conversation history and execution evidence.
pub fn reset(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: super::wire::requests::MemoryReset =
        serde_json::from_value(params).map_err(|error| format!("invalid memory reset: {error}"))?;
    if !request.confirm {
        return Err("memory reset requires confirm=true".to_string());
    }
    let uid = client.require_uid()?;
    if uid == 0 {
        return Err(ROOT_OWNER_REFUSAL.to_string());
    }
    refuse_active_tasks(uid)?;
    reset_for_owner(uid)
}

fn refuse_active_tasks(owner_uid: u32) -> Result<(), String> {
    let store = Store::open_default().map_err(|error| error.to_string())?;
    for status in [
        JobStatus::Pending,
        JobStatus::Running,
        JobStatus::WaitingApproval,
    ] {
        if !store
            .list_bucket_for_owner(status, Some(1), Some(owner_uid))
            .map_err(|error| error.to_string())?
            .is_empty()
        {
            return Err(
                "learned memory cannot be reset while this owner has an active Agent task"
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn reset_for_owner(owner_uid: u32) -> Result<Value, String> {
    let state_dir = crate::paths::clawd_user_agent_state_dir(owner_uid);
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("clawd-owner-memory-reset".to_string())
        .spawn(move || {
            let result = FsIdentityGuard::enter(owner_uid)
                .and_then(|_identity| maintenance::reset_at(&state_dir))
                .and_then(|report| serde_json::to_value(report).map_err(|error| error.to_string()));
            let _ = tx.send(result);
        })
        .map_err(|error| format!("start owner memory reset: {error}"))?;
    rx.recv()
        .map_err(|_| "owner memory reset stopped without a result".to_string())?
}

fn read_client_db<T, F>(client: &ClientIdentity, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(Option<&MemoryDb>) -> Result<T, String> + Send + 'static,
{
    let uid = client.require_uid()?;
    let read = move |path: PathBuf| {
        let db = open_owner_db(&path, uid)?;
        operation(db.as_ref())
    };
    if uid == 0 {
        return read(crate::paths::agent_memory_db_path());
    }
    run_as_owner(uid, read)
}

pub(super) fn open_owner_db(
    path: &std::path::Path,
    owner_uid: u32,
) -> Result<Option<MemoryDb>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "inspect owner memory database {}: {error}",
                path.display()
            ))
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("memory database must be a regular, non-symlink file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if owner_uid != 0 && metadata.uid() != owner_uid {
            return Err(format!(
                "Memory database {} is owned by uid {}, not authenticated owner uid {owner_uid}. Existing files were not changed.",
                path.display(), metadata.uid(),
            ));
        }
    }
    MemoryDb::open_read_only(path)
        .map(Some)
        .map_err(|error| format!("open owner memory database {}: {error}", path.display()))
}

pub(super) fn run_as_owner<T, F>(owner_uid: u32, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(PathBuf) -> Result<T, String> + Send + 'static,
{
    if owner_uid == 0 {
        return Err(ROOT_OWNER_REFUSAL.to_string());
    }
    let db_path = crate::paths::clawd_user_memory_db_path(owner_uid);
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("clawd-owner-memory".to_string())
        .spawn(move || {
            let result = FsIdentityGuard::enter(owner_uid).and_then(|_identity| operation(db_path));
            let _ = tx.send(result);
        })
        .map_err(|err| format!("start owner memory operation: {err}"))?;
    rx.recv()
        .map_err(|_| "owner memory operation stopped without a result".to_string())?
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/memory.rs"
    ));
}
