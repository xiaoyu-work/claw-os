//! `GET /api/approvals/*` and `POST /api/approvals/:id/{approve,deny}`
//! — surfaces the consent queue so the user can answer prompts that
//! would otherwise block a clawd-routed agent job.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::approvals::{self, GrantDuration};
use crate::agent::web::state::AppState;

pub async fn pending(State(state): State<AppState>) -> Json<Value> {
    let reqs = approvals::list_pending_for_owner(Some(state.inner.owner_uid));
    Json(json!({
        "n": reqs.len(),
        "requests": reqs,
    }))
}

pub async fn recent(State(state): State<AppState>) -> Json<Value> {
    let entries = approvals::list_recent_for_owner(50, Some(state.inner.owner_uid));
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
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<ApproveBody>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let duration = body
        .duration
        .as_deref()
        .and_then(GrantDuration::parse)
        .unwrap_or(GrantDuration::Once);
    let resolved = approvals::approve_for_owner(
        &id,
        duration,
        Some(format!("web:uid:{}", state.inner.owner_uid)),
        body.note,
        Some(state.inner.owner_uid),
    )
    .map_err(bad_request)?;
    Ok(Json(json!({ "ok": true, "resolved": resolved })))
}

#[derive(Debug, Deserialize, Default)]
pub struct DenyBody {
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn deny(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<DenyBody>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let resolved = approvals::deny_for_owner(
        &id,
        Some(format!("web:uid:{}", state.inner.owner_uid)),
        body.note,
        Some(state.inner.owner_uid),
    )
    .map_err(bad_request)?;
    Ok(Json(json!({ "ok": true, "resolved": resolved })))
}

fn bad_request(msg: String) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}
