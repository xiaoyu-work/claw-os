//! Presentation controls for broker-owned Activity pending admission priority.

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection, rejection::QueryRejection},
};
use clawd_client::Command;
use cos_agent_protocol::{
    ActivitySchedulingPolicy, ActivitySchedulingPriorityQuery, ActivitySchedulingPriorityResponse,
    ActivitySchedulingPrioritySetRequest,
};
use serde_json::json;

use super::{body, invalid, upstream_error, with_id};
use crate::{
    api_error::ApiError,
    state::{AppState, current_uid},
    translation::scheduling_priority as translation,
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
    query: Result<Query<ActivitySchedulingPriorityQuery>, QueryRejection>,
) -> Result<Json<ActivitySchedulingPriorityResponse>, ApiError> {
    query.map_err(|_| invalid("Scheduling-priority queries accept no owner or state selectors"))?;
    let owner = owner_uid()?;
    let value = state
        .clawd
        .call(
            Command::ActivitySchedulingPolicyGet,
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
    request: Result<Json<ActivitySchedulingPrioritySetRequest>, JsonRejection>,
) -> Result<Json<ActivitySchedulingPolicy>, ApiError> {
    let request = body(request)?;
    request.validate_shape().map_err(invalid)?;
    let owner = owner_uid()?;
    let value = state
        .clawd
        .call(
            Command::ActivitySchedulingPolicySet,
            with_id(&id, &request)?,
        )
        .await
        .map_err(upstream_error)?;
    let policy = translation::policy(value, &id, owner).map_err(ApiError::bad_gateway)?;
    if !policy.matches_set(&id, &request) {
        return Err(ApiError::bad_gateway(
            "Scheduling-priority acknowledgement did not match the submitted priority or revision",
        ));
    }
    Ok(Json(policy))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/activities/scheduling_priority.rs"
    ));
}
