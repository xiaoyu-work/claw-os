//! `memory.*` clawd commands — read-only views over the agent memory DB.
//!
//! Used by the desktop agent UI (via `cos-agent-bridge`) to load
//! historical conversation rows so users can resume chats across
//! restarts. Same parser as the web `GET /api/sessions/:id/history`
//! route — both reuse `agent::memory::history`.

use serde_json::{json, Value};

use crate::agent::memory::history::load_history;
use crate::agent::memory::sqlite_fts::MemoryDb;

const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 2000;
const DEFAULT_SESSION_LIMIT: usize = 200;
const MAX_SESSION_LIMIT: usize = 1000;

/// `memory.history` — return the most recent rows for `session_id`,
/// each pre-parsed into `{ role, text, tool_calls, tool_results, ts_ms }`.
pub fn history(params: Value) -> Result<Value, String> {
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

    let db = MemoryDb::open_default().map_err(|err| format!("open memory: {err}"))?;
    let messages =
        load_history(&db, session_id, limit).map_err(|err| format!("read history: {err}"))?;

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
pub fn sessions(params: Value) -> Result<Value, String> {
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_SESSION_LIMIT)
        .clamp(1, MAX_SESSION_LIMIT);

    let db = MemoryDb::open_default().map_err(|err| format!("open memory: {err}"))?;
    let rows = db
        .sessions(limit)
        .map_err(|err| format!("list sessions: {err}"))?;

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

