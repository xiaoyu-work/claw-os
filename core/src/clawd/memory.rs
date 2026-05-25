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
