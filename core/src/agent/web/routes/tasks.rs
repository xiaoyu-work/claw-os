//! `GET /api/tasks` and friends — durable session lifecycle.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::agent::lifecycle;

pub async fn list() -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle::ls(&[]).map(Json).map_err(bad_request)
}

pub async fn show(
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle::show(&[id]).map(Json).map_err(bad_request)
}

pub async fn stop(
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle::stop(&[id]).map(Json).map_err(bad_request)
}

pub async fn undo(
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle::undo(&[id]).map(Json).map_err(bad_request)
}

pub async fn resume(
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    lifecycle::resume(&[id]).map(Json).map_err(bad_request)
}

fn bad_request(msg: String) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}
