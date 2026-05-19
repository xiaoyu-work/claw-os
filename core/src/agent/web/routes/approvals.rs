//! `GET /api/approvals/*` and `POST /api/approvals/:id/{approve,deny}`
//! — surfaces the consent queue so the user can answer prompts that
//! would otherwise block a clawd-routed agent job.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::approvals::{self, GrantDuration};

pub async fn pending() -> Json<Value> {
    let reqs = approvals::list_pending();
    Json(json!({
        "n": reqs.len(),
        "requests": reqs,
    }))
}

pub async fn recent() -> Json<Value> {
    let entries = approvals::list_recent(50);
    Json(json!({
        "n": entries.len(),
        "entries": entries,
    }))
}

#[derive(Debug, Deserialize, Default)]
pub struct ApproveBody {
    #[serde(default)]
    pub duration: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn approve(
    Path(id): Path<String>,
    body: Option<Json<ApproveBody>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let duration = body
        .duration
        .as_deref()
        .and_then(GrantDuration::parse)
        .unwrap_or(GrantDuration::Once);
    let resolved = approvals::approve(&id, duration, Some("web".into()), body.note)
        .map_err(bad_request)?;
    Ok(Json(json!({ "ok": true, "resolved": resolved })))
}

#[derive(Debug, Deserialize, Default)]
pub struct DenyBody {
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn deny(
    Path(id): Path<String>,
    body: Option<Json<DenyBody>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let resolved =
        approvals::deny(&id, Some("web".into()), body.note).map_err(bad_request)?;
    Ok(Json(json!({ "ok": true, "resolved": resolved })))
}

fn bad_request(msg: String) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}
