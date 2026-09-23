//! `GET /api/tasks` and friends — the durable `clawd` task queue.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::web::state::AppState;
use crate::clawd::routes::Command;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowUpRequest {
    prompt: String,
    #[serde(default)]
    attachments: Vec<crate::agent::attachments::AttachmentInput>,
    #[serde(default = "default_true")]
    use_memory: bool,
}

fn default_true() -> bool {
    true
}

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

pub async fn follow_up(
    State(_state): State<AppState>,
    Path(predecessor_id): Path<String>,
    Json(request): Json<FollowUpRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if request.prompt.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "empty prompt" })),
        ));
    }
    crate::agent::attachments::normalize(request.attachments.clone())
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
    let predecessor = super::clawd::request(Command::TaskGet, json!({ "id": predecessor_id }))
        .await
        .map_err(super::clawd::RpcError::into_api_error)?;
    let params = follow_up_params(&predecessor, request).map_err(invalid_task_response)?;
    super::clawd::request(Command::TaskSubmit, params)
        .await
        .map(Json)
        .map_err(super::clawd::RpcError::into_api_error)
}

fn follow_up_params(predecessor: &Value, request: FollowUpRequest) -> Result<Value, String> {
    let predecessor_id = predecessor
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "predecessor task has no id".to_string())?;
    let session_id = predecessor
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "predecessor task has no conversation".to_string())?;
    let mut params = json!({
        "prompt": request.prompt,
        "session_id": session_id,
        "after_task_id": predecessor_id,
        "use_memory": request.use_memory,
    });
    if !request.attachments.is_empty() {
        params["attachments"] =
            serde_json::to_value(request.attachments).map_err(|error| error.to_string())?;
    }
    Ok(params)
}

fn invalid_task_response(message: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({ "error": format!("invalid clawd task: {message}") })),
    )
}

fn preview(value: &str, max: usize) -> String {
    let compact = value.replace('\n', " ");
    if compact.chars().count() <= max {
        compact
    } else {
        format!("{}...", compact.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/routes/tasks.rs"
    ));
}
