//! `GET /api/approvals/*` and `POST /api/approvals/:id/{approve,deny}`
//! — surfaces the consent queue so the user can answer prompts that
//! would otherwise block a clawd-routed agent job.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::web::state::AppState;
use crate::clawd::routes::Command;

pub(super) mod authorization;
mod presentation;
pub use authorization::{
    authorization_response, authorization_status, cancel_authorization, start_authorization,
    start_system_authorization,
};
pub use presentation::{
    cancel_system_review, decide_review, pending_reviews, recent_reviews, system_review,
    task_reviews,
};

pub async fn pending(
    State(_state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::clawd::request(Command::PermissionPending, json!({ "limit": 100 }))
        .await
        .map(Json)
        .map_err(super::clawd::RpcError::into_api_error)
}

pub async fn recent(
    State(_state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let result = super::clawd::request(Command::PermissionRecent, json!({ "limit": 50 }))
        .await
        .map_err(super::clawd::RpcError::into_api_error)?;
    let entries = result
        .get("requests")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(Json(json!({ "n": entries.len(), "entries": entries })))
}

#[derive(Debug, Deserialize, Default)]
pub struct ApproveBody {
    #[serde(default)]
    pub duration: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn approve(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<ApproveBody>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let duration = body
        .duration
        .as_deref()
        .unwrap_or("once")
        .trim()
        .to_ascii_lowercase();
    if !matches!(duration.as_str(), "once" | "session" | "forever") {
        return Err(bad_request(format!(
            "unknown permission grant duration: {duration}"
        )));
    }
    validate_decision_input(&id, body.note.as_deref())?;
    Err((
        StatusCode::CONFLICT,
        Json(json!({
            "error": "Use the Web OS authorization dialog. Refresh this page before authorizing.",
            "code": "authorization_dialog_required",
        })),
    ))
}

#[derive(Debug, Deserialize, Default)]
pub struct DenyBody {
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn deny(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<DenyBody>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    validate_decision_input(&id, body.note.as_deref())?;
    let mut params = json!({ "id": id, "decision": "deny" });
    if let Some(note) = body.note.filter(|note| !note.trim().is_empty()) {
        params["note"] = json!(note);
    }
    super::clawd::request(Command::PermissionDecide, params)
        .await
        .map(Json)
        .map_err(super::clawd::RpcError::into_api_error)
}

fn validate_decision_input(id: &str, note: Option<&str>) -> Result<(), (StatusCode, Json<Value>)> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(bad_request("invalid approval id".to_string()));
    }
    if note.is_some_and(|note| note.len() > 1_024) {
        return Err(bad_request("approval note is too long".to_string()));
    }
    Ok(())
}

fn bad_request(msg: String) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}

fn internal(msg: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": msg })),
    )
}
