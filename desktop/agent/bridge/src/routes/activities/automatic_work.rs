//! Authenticated transport only; all scheduling and state remain in clawd.

use super::{body, invalid, upstream_error, with_id};
use crate::{
    api_error::ApiError,
    state::{AppState, current_uid},
};
use axum::{
    Json,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
};
use clawd_client::Command;
use cos_agent_protocol::{
    ActivityAutoWork, ActivityAutoWorkResponse, AutoWorkEnabledRequest, AutoWorkQuery,
    AutoWorkSetRequest,
};
use serde_json::json;

fn owner() -> Result<u32, ApiError> {
    current_uid().map_err(|error| ApiError::service_unavailable(error.to_string()))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    query: Result<Query<AutoWorkQuery>, QueryRejection>,
) -> Result<Json<ActivityAutoWorkResponse>, ApiError> {
    query.map_err(|_| invalid("Automatic work accepts no owner or state selectors"))?;
    let value = state
        .clawd
        .call(Command::ActivityWorkGet, with_id(&id, json!({}))?)
        .await
        .map_err(upstream_error)?;
    let response: ActivityAutoWorkResponse =
        serde_json::from_value(value).map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    if !response.matches_owner(&id, owner()?) {
        return Err(ApiError::bad_gateway(
            "Activity automatic-work view differs",
        ));
    }
    Ok(Json(response))
}

pub async fn set(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<AutoWorkSetRequest>, JsonRejection>,
) -> Result<Json<ActivityAutoWork>, ApiError> {
    let request = body(request)?;
    request.validate().map_err(invalid)?;
    let value = state
        .clawd
        .call(Command::ActivityWorkSet, with_id(&id, &request)?)
        .await
        .map_err(upstream_error)?;
    let work: ActivityAutoWork =
        serde_json::from_value(value).map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    if !work.matches_owner(&id, owner()?)
        || work.enabled
        || work.config != request.work
        || work.revision != request.expected_revision.unwrap_or(0) + 1
    {
        return Err(ApiError::bad_gateway(
            "Activity work configuration acknowledgement differs",
        ));
    }
    Ok(Json(work))
}

pub async fn enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<AutoWorkEnabledRequest>, JsonRejection>,
) -> Result<Json<ActivityAutoWork>, ApiError> {
    let request = body(request)?;
    request.validate().map_err(invalid)?;
    let value = state
        .clawd
        .call(Command::ActivityWorkEnabled, with_id(&id, &request)?)
        .await
        .map_err(upstream_error)?;
    let work: ActivityAutoWork =
        serde_json::from_value(value).map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    if !work.matches_owner(&id, owner()?)
        || work.enabled != request.enabled
        || work.revision != request.expected_revision + 1
    {
        return Err(ApiError::bad_gateway(
            "Activity work enablement acknowledgement differs",
        ));
    }
    Ok(Json(work))
}
