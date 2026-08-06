//! `GET /api/tasks` and friends — durable session lifecycle.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::agent::lifecycle;
use crate::agent::web::state::AppState;

pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle::ls_for_owner(&[], state.inner.owner_uid)
        .map(Json)
        .map_err(bad_request)
}

pub async fn show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle::show_for_owner(&[id], state.inner.owner_uid)
        .map(Json)
        .map_err(bad_request)
}

pub async fn stop(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle::stop_for_owner(&[id], state.inner.owner_uid)
        .map(Json)
        .map_err(bad_request)
}

pub async fn undo(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle::undo_for_owner(&[id], state.inner.owner_uid)
        .map(Json)
        .map_err(bad_request)
}

pub async fn resume(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle::resume_for_owner(&[id], state.inner.owner_uid)
        .map(Json)
        .map_err(bad_request)
}

fn bad_request(msg: String) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}
