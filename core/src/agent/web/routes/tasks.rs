//! `GET /api/tasks` and friends — the durable `clawd` task queue.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::agent::web::state::AppState;
use crate::clawd::routes::Command;

pub async fn list(
    State(_state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let result = super::clawd::request(Command::TaskList, json!({ "summary": true, "limit": 50 }))
        .await
        .map_err(super::clawd::RpcError::into_api_error)?;
    let mut tasks = result
        .get("jobs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for task in &mut tasks {
        if task.get("title").is_none() {
            task["title"] = json!(task
                .get("prompt")
                .and_then(Value::as_str)
                .map(|prompt| preview(prompt, 80))
                .unwrap_or_else(|| "Agent task".to_string()));
        }
    }
    Ok(Json(json!({ "n": tasks.len(), "tasks": tasks })))
}

pub async fn show(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::clawd::request(Command::TaskGet, json!({ "id": id }))
        .await
        .map(Json)
        .map_err(super::clawd::RpcError::into_api_error)
}

pub async fn stop(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::clawd::request(Command::TaskCancel, json!({ "id": id }))
        .await
        .map(Json)
        .map_err(super::clawd::RpcError::into_api_error)
}

pub async fn resume(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::clawd::request(Command::TaskRetry, json!({ "id": id }))
        .await
        .map(Json)
        .map_err(super::clawd::RpcError::into_api_error)
}

fn preview(value: &str, max: usize) -> String {
    let compact = value.replace('\n', " ");
    if compact.chars().count() <= max {
        compact
    } else {
        format!("{}...", compact.chars().take(max).collect::<String>())
    }
}
