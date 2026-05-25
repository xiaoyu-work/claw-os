//! `/api/sessions` — list / get / delete persisted chat sessions.
//!
//! Backed by clawd's `memory.sessions` / `memory.history` commands so
//! the desktop UI and the web client see the same persisted history.
//! `task.cancel` is still used for `DELETE` because the memory DB does
//! not expose a row-purge surface yet — deleting a sidebar entry only
//! cancels the most recent task tied to it, not the conversation.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::clawd;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct Session {
    /// Memory session id (stable across restarts; what
    /// `task.submit { session_id }` continues into).
    pub id: String,
    pub title: String,
    /// Last activity, milliseconds since epoch. Optional because
    /// freshly-created sessions may not have any messages yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ts_ms: Option<i64>,
    pub message_count: i64,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Session>>, StatusCode> {
    let value = clawd::request(&state.clawd_socket, "memory.sessions", json!({ "limit": 200 }))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let rows = value
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or(StatusCode::BAD_GATEWAY)?;
    let sessions = rows
        .iter()
        .filter_map(session_from_memory)
        .collect::<Vec<_>>();
    Ok(Json(sessions))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Session>, StatusCode> {
    // No dedicated single-session lookup yet — page through the
    // recent list. 200 entries is more than the panel sidebar will
    // ever show, so this stays fast.
    let value = clawd::request(&state.clawd_socket, "memory.sessions", json!({ "limit": 200 }))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let rows = value
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or(StatusCode::BAD_GATEWAY)?;
    rows.iter()
        .filter_map(session_from_memory)
        .find(|s| s.id == id)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub async fn delete_one(State(state): State<AppState>, Path(id): Path<String>) -> StatusCode {
    // Sessions live in the memory DB; clawd has no purge command yet.
    // Falling back to `task.cancel` to at least stop the in-flight
    // job, if any. The sidebar row will reappear on next refresh
    // until a memory.purge / memory.delete command lands.
    match clawd::request(&state.clawd_socket, "task.cancel", json!({ "id": id })).await {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

/// `GET /api/sessions/:id/history` — proxy to clawd `memory.history`.
///
/// Returns the conversation transcript pre-parsed into structured
/// `messages: [{ role, text, tool_calls, tool_results, ts_ms, ... }]`
/// so the desktop UI can resume a chat session without having to
/// reverse-engineer the `[tool_use:...]` storage format itself.
pub async fn history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let value = clawd::request(
        &state.clawd_socket,
        "memory.history",
        json!({ "session_id": id, "limit": 500 }),
    )
    .await
    .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(value))
}

fn session_from_memory(row: &Value) -> Option<Session> {
    let id = row.get("id")?.as_str()?.to_string();
    let title = row
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| title_from_id(&id));
    let last_ts_ms = row.get("last_ts_ms").and_then(Value::as_i64);
    let message_count = row
        .get("message_count")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Some(Session {
        id,
        title: title_compact(&title),
        last_ts_ms,
        message_count,
    })
}

fn title_from_id(id: &str) -> String {
    // Fallback when the session has no recorded title yet — show a
    // short hash-y stub so the sidebar still has something readable.
    let short: String = id.chars().take(8).collect();
    format!("Session {short}")
}

fn title_compact(title: &str) -> String {
    let compact = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 80 {
        compact
    } else {
        format!("{}...", compact.chars().take(80).collect::<String>())
    }
}

