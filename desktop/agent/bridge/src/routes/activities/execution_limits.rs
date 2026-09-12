//! Owner-scoped constraint settings, with no task execution or authority grants.

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection, rejection::QueryRejection},
};
use clawd_client::Command;
use cos_agent_protocol::{
    ActivityExecutionLimits, ActivityExecutionLimitsEnabledRequest, ActivityExecutionLimitsQuery,
    ActivityExecutionLimitsResponse, ActivityExecutionLimitsSetRequest,
};
use serde_json::json;

use super::{body, invalid, upstream_error, with_id};
use crate::{
    api_error::ApiError,
    state::{AppState, current_uid},
    translation::execution_limits as translation,
};

fn owner_uid() -> Result<u32, ApiError> {
    current_uid().map_err(|error| {
        ApiError::service_unavailable(format!(
            "could not inspect bridge owner identity: {error:#}"
        ))
    })
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    query: Result<Query<ActivityExecutionLimitsQuery>, QueryRejection>,
) -> Result<Json<ActivityExecutionLimitsResponse>, ApiError> {
    query.map_err(|_| invalid("Execution limits queries accept no owner or state selectors"))?;
    let owner = owner_uid()?;
    let value = state
        .clawd
        .call(
            Command::ActivityExecutionLimitsGet,
            with_id(&id, json!({}))?,
        )
        .await
        .map_err(upstream_error)?;
    translation::get(value, &id, owner)
        .map(Json)
        .map_err(ApiError::bad_gateway)
}

pub async fn set(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<ActivityExecutionLimitsSetRequest>, JsonRejection>,
) -> Result<Json<ActivityExecutionLimits>, ApiError> {
    let request = body(request)?;
    request.validate_shape().map_err(invalid)?;
    let owner = owner_uid()?;
    let value = state
        .clawd
        .call(Command::ActivityExecutionLimitsSet, with_id(&id, &request)?)
        .await
        .map_err(upstream_error)?;
    let limits = translation::policy(value, &id, owner).map_err(ApiError::bad_gateway)?;
    if !limits.matches_set(&id, &request) {
        return Err(ApiError::bad_gateway(
            "Execution limits acknowledgement did not match the submitted limits or revision",
        ));
    }
    Ok(Json(limits))
}

pub async fn enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<ActivityExecutionLimitsEnabledRequest>, JsonRejection>,
) -> Result<Json<ActivityExecutionLimits>, ApiError> {
    let request = body(request)?;
    request.validate_shape().map_err(invalid)?;
    let owner = owner_uid()?;
    let value = state
        .clawd
        .call(
            Command::ActivityExecutionLimitsEnabled,
            with_id(&id, &request)?,
        )
        .await
        .map_err(upstream_error)?;
    let limits = translation::policy(value, &id, owner).map_err(ApiError::bad_gateway)?;
    if !limits.matches_enabled(&id, &request) {
        return Err(ApiError::bad_gateway(
            "Execution limits acknowledgement did not match the requested enabled state or revision",
        ));
    }
    Ok(Json(limits))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/activities/execution_limits.rs"
    ));
}
