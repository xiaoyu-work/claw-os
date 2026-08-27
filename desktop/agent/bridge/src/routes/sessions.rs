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
use cos_agent_protocol::{ErrorCode, HistoryResponse, SessionSummary};
use serde_json::json;

use crate::{api_error::ApiError, state::AppState, translation};
use clawd_client::Command;

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<SessionSummary>>, ApiError> {
    let value = state
        .clawd
        .call(Command::MemorySessions, json!({ "limit": 200 }))
        .await
        .map_err(|error| ApiError::service_unavailable(error.to_string()))?;
    translation::sessions(value)
        .map(Json)
        .map_err(ApiError::bad_gateway)
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionSummary>, ApiError> {
    // No dedicated single-session lookup yet — page through the
    // recent list. 200 entries is more than the panel sidebar will
    // ever show, so this stays fast.
    let value = state
        .clawd
        .call(Command::MemorySessions, json!({ "limit": 200 }))
        .await
        .map_err(|error| ApiError::service_unavailable(error.to_string()))?;
    translation::sessions(value)
        .map_err(ApiError::bad_gateway)?
        .into_iter()
        .find(|session| session.id == id)
        .map(Json)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                ErrorCode::NotFound,
                "session not found",
            )
        })
}

pub async fn delete_one(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let _ = (state, id);
    // A session id is not a task id. Calling `task.cancel` here used to
    // cancel an unrelated job when the strings happened to collide and
    // never deleted the memory row. Do not advertise deletion until
    // clawd exposes an actual memory-session purge command.
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        ErrorCode::NotImplemented,
        "session deletion is not implemented",
    ))
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
) -> Result<Json<HistoryResponse>, ApiError> {
    let value = state
        .clawd
        .call(
            Command::MemoryHistory,
            json!({ "session_id": id, "limit": 500 }),
        )
        .await
        .map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    translation::history(value)
        .map(Json)
        .map_err(ApiError::bad_gateway)
}
