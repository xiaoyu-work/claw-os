//! `GET /api/sessions` and friends — memory DB session inspection.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::agent::memory::history::load_history;
use crate::agent::memory::sqlite_fts::MemoryDb;

pub async fn list() -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let db = MemoryDb::open_default()
        .map_err(|e| internal(format!("open memory: {e}")))?;
    let rows = db
        .sessions(200)
        .map_err(|e| internal(format!("read sessions: {e}")))?;

    let mut sessions = Vec::with_capacity(rows.len());
    for s in rows {
        sessions.push(json!({
            "id": s.session_id,
            "title": s.title,
            "last_ts_ms": s.last_ts_ms,
            "message_count": s.message_count,
        }));
    }
    Ok(Json(json!({ "n": sessions.len(), "sessions": sessions })))
}

pub async fn detail(
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let db = MemoryDb::open_default()
        .map_err(|e| internal(format!("open memory: {e}")))?;
    let title = db
        .title_for(&id)
        .map_err(|e| internal(format!("title: {e}")))?;
    Ok(Json(json!({
        "id": id,
        "title": title,
    })))
}

pub async fn history(
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let db = MemoryDb::open_default()
        .map_err(|e| internal(format!("open memory: {e}")))?;
    // Shared with clawd's `memory.history` command — the desktop agent
    // UI and the web client both render history off the same shape.
    let mut messages = load_history(&db, &id, 500)
        .map_err(|e| internal(format!("read history: {e}")))?;
    messages.retain(|message| message.role != "system");
    Ok(Json(json!({
        "session_id": id,
        "n": messages.len(),
        "messages": messages,
    })))
}

fn internal(msg: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": msg })),
    )
}

