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
    run_decision_helper(&id, "approve", Some(&duration), body.note)
        .await
        .map(Json)
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
    run_decision_helper(&id, "deny", None, body.note)
        .await
        .map(Json)
}

async fn run_decision_helper(
    id: &str,
    decision: &str,
    duration: Option<&str>,
    note: Option<String>,
) -> Result<Value, (StatusCode, Json<Value>)> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(bad_request("invalid approval id".to_string()));
    }
    if note.as_ref().is_some_and(|note| note.len() > 1_024) {
        return Err(bad_request("approval note is too long".to_string()));
    }

    let mut command = tokio::process::Command::new("/usr/bin/pkexec");
    command
        .arg("/usr/local/bin/claw-approval-helper")
        .arg("--id")
        .arg(id)
        .arg("--decision")
        .arg(decision)
        .kill_on_drop(true);
    if let Some(duration) = duration {
        command.arg("--duration").arg(duration);
    }
    if let Some(note) = note.filter(|note| !note.trim().is_empty()) {
        command.arg("--note").arg(note);
    }
    let output = command
        .output()
        .await
        .map_err(|error| internal(format!("launch privileged approval helper: {error}")))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": if message.is_empty() {
                    "approval authorization was cancelled or denied"
                } else {
                    message.as_str()
                }
            })),
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| internal(format!("invalid approval helper response: {error}")))
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
