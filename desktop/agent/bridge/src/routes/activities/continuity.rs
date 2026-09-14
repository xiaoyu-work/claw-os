//! Authenticated owner-scoped Activity continuity presentation routes.

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection, rejection::QueryRejection},
};
use clawd_client::Command;
use cos_agent_protocol::{
    ActivityContinuityDocument, ActivityContinuityExportQuery,
    ActivityContinuityImportAcknowledgement, ActivityContinuityImportRequest,
};
use serde_json::json;

use super::{body, invalid, upstream_error, with_id};
use crate::{
    api_error::ApiError,
    state::{AppState, current_uid},
    translation::continuity as translation,
};

fn owner_uid() -> Result<u32, ApiError> {
    current_uid().map_err(|error| {
        ApiError::service_unavailable(format!(
            "could not inspect bridge owner identity: {error:#}"
        ))
    })
}

pub async fn export(
    State(state): State<AppState>,
    Path(id): Path<String>,
    query: Result<Query<ActivityContinuityExportQuery>, QueryRejection>,
) -> Result<Json<ActivityContinuityDocument>, ApiError> {
    query.map_err(|_| {
        invalid("Activity continuity export accepts no owner or authority selectors")
    })?;
    let _authenticated_owner = owner_uid()?;
    let value = state
        .clawd
        .call(Command::ActivityContinuityExport, with_id(&id, json!({}))?)
        .await
        .map_err(upstream_error)?;
    translation::export(value)
        .map(Json)
        .map_err(ApiError::bad_gateway)
}

pub async fn import(
    State(state): State<AppState>,
    request: Result<Json<ActivityContinuityImportRequest>, JsonRejection>,
) -> Result<Json<ActivityContinuityImportAcknowledgement>, ApiError> {
    let request = body(request)?;
    let document = request.validated_document().map_err(|error| {
        ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            cos_agent_protocol::ErrorCode::InvalidRequest,
            error,
        )
    })?;
    let canonical = document.to_json().map_err(|error| {
        ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            cos_agent_protocol::ErrorCode::InvalidRequest,
            error,
        )
    })?;
    let owner = owner_uid()?;
    let value = state
        .clawd
        .call(
            Command::ActivityContinuityImport,
            import_params(request.placement, canonical),
        )
        .await
        .map_err(upstream_error)?;
    translation::import(value, owner, &document, request.placement)
        .map(Json)
        .map_err(ApiError::bad_gateway)
}

fn import_params(
    placement: cos_agent_protocol::ActivityExecutionPlacement,
    document: String,
) -> serde_json::Value {
    json!({
        "placement": placement,
        "document": document,
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/activities/continuity.rs"
    ));
}
