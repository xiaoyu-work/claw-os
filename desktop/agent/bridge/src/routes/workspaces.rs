//! Human Workspace presentation over the canonical same-owner broker.

use crate::{api_error::ApiError, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use clawd_client::Command;
use cos_agent_protocol::{ErrorCode, workspaces::*};
use serde_json::{Value, json};

fn invalid(error: impl std::fmt::Display) -> ApiError {
    ApiError::bad_gateway(format!("invalid Workspace response: {error}"))
}
fn bad_request(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        ErrorCode::InvalidRequest,
        error.to_string(),
    )
}

pub async fn list(State(state): State<AppState>) -> Result<Json<WorkspaceList>, ApiError> {
    let value = state
        .clawd
        .call(Command::WorkspaceList, json!({}))
        .await
        .map_err(invalid)?;
    let list: WorkspaceList = serde_json::from_value(value).map_err(invalid)?;
    list.validate().map_err(invalid)?;
    Ok(Json(list))
}
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceView>, ApiError> {
    validate_id(&id).map_err(bad_request)?;
    let value = state
        .clawd
        .call(Command::WorkspaceGet, json!({"id":id}))
        .await
        .map_err(invalid)?;
    let view: WorkspaceView = serde_json::from_value(value).map_err(invalid)?;
    view.validate().map_err(invalid)?;
    if view.workspace.id != id {
        return Err(invalid("Workspace identity mismatch"));
    }
    Ok(Json(view))
}
pub async fn control(
    State(state): State<AppState>,
    Json(request): Json<ControlRequest>,
) -> Result<Json<WorkspaceView>, ApiError> {
    request.request.validate().map_err(bad_request)?;
    let id = request.request.id().map(str::to_string);
    let revision = request.request.expected_revision();
    let value = state
        .clawd
        .call(
            Command::WorkspaceControl,
            serde_json::to_value(request).map_err(invalid)?,
        )
        .await
        .map_err(invalid)?;
    let view: WorkspaceView = serde_json::from_value(value).map_err(invalid)?;
    view.validate().map_err(invalid)?;
    if id.is_some_and(|id| id != view.workspace.id)
        || revision.is_some_and(|revision| view.workspace.revision <= revision)
    {
        return Err(invalid("Workspace acknowledgement mismatch"));
    }
    Ok(Json(view))
}
pub async fn read(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ReadBody>,
) -> Result<Json<Value>, ApiError> {
    let request = body.into_request(id).map_err(bad_request)?;
    let operation = request.request.clone();
    let value = state
        .clawd
        .call(
            Command::WorkspaceRead,
            serde_json::to_value(request).map_err(invalid)?,
        )
        .await
        .map_err(invalid)?;
    match operation {
        WorkspaceRead::Note { .. } => serde_json::from_value::<NotePreview>(value.clone())
            .map_err(invalid)?
            .validate()
            .map_err(invalid)?,
        WorkspaceRead::Source { .. } => serde_json::from_value::<SourcePage>(value.clone())
            .map_err(invalid)?
            .validate()
            .map_err(invalid)?,
        WorkspaceRead::Search { .. } => serde_json::from_value::<SearchResult>(value.clone())
            .map_err(invalid)?
            .validate()
            .map_err(invalid)?,
        WorkspaceRead::Index => serde_json::from_value::<IndexStatus>(value.clone())
            .map_err(invalid)?
            .validate()
            .map_err(invalid)?,
        _ => {
            return Err(bad_request(
                "use the Workspace view for shared context and notes",
            ));
        }
    }
    Ok(Json(value))
}
pub async fn index_source(
    State(state): State<AppState>,
    Json(request): Json<IndexRequest>,
) -> Result<Json<IndexSourceResult>, ApiError> {
    validate_id(&request.id).map_err(bad_request)?;
    validate_id(&request.tag_id).map_err(bad_request)?;
    if request.expected_revision == 0 {
        return Err(bad_request("displayed Workspace revision is required"));
    }
    let value = state
        .clawd
        .call(
            Command::WorkspaceIndexSource,
            serde_json::to_value(request).map_err(invalid)?,
        )
        .await
        .map_err(invalid)?;
    let result: IndexSourceResult = serde_json::from_value(value).map_err(invalid)?;
    result.validate().map_err(invalid)?;
    Ok(Json(result))
}
