//! `/api/sessions` — list / get / delete persisted chat sessions.

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
    pub id: String,
    pub title: String,
    pub updated_at: String,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Session>>, StatusCode> {
    let value = clawd::request(&state.clawd_socket, "task.list", json!({ "limit": 100 }))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let jobs = value
        .get("jobs")
        .and_then(Value::as_array)
        .ok_or(StatusCode::BAD_GATEWAY)?;
    let sessions = jobs.iter().filter_map(session_from_job).collect::<Vec<_>>();
    Ok(Json(sessions))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Session>, StatusCode> {
    let value = clawd::request(&state.clawd_socket, "task.get", json!({ "id": id }))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    session_from_job(&value)
        .map(Json)
        .ok_or(StatusCode::BAD_GATEWAY)
}

pub async fn delete_one(State(state): State<AppState>, Path(id): Path<String>) -> StatusCode {
    match clawd::request(&state.clawd_socket, "task.cancel", json!({ "id": id })).await {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

fn session_from_job(job: &Value) -> Option<Session> {
    let id = job.get("id")?.as_str()?.to_string();
    let prompt = job
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("Agent task");
    let updated_at = job
        .get("finished_at")
        .or_else(|| job.get("started_at"))
        .or_else(|| job.get("created_at"))?
        .as_str()?
        .to_string();
    Some(Session {
        id,
        title: title_from_prompt(prompt),
        updated_at,
    })
}

fn title_from_prompt(prompt: &str) -> String {
    let compact = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 80 {
        compact
    } else {
        format!("{}...", compact.chars().take(80).collect::<String>())
    }
}
