//! `/api/sessions` — list / get / delete persisted chat sessions.
//!
//! Stub: returns an empty list. Will subprocess
//! `cos agent service list --status done` (or similar) once wired.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub updated_at: String,
}

pub async fn list(State(_state): State<AppState>) -> Json<Vec<Session>> {
    Json(Vec::new())
}

pub async fn get(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Session>, StatusCode> {
    let _ = id;
    Err(StatusCode::NOT_FOUND)
}

pub async fn delete_one(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> StatusCode {
    let _ = id;
    StatusCode::NO_CONTENT
}
