//! Presentation controls for the broker-owned Activity monetary ledger.

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection, rejection::QueryRejection},
};
use clawd_client::Command;
use cos_agent_protocol::{
    ActivityMonetaryBudget, ActivityMonetaryBudgetEnabledRequest, ActivityMonetaryBudgetQuery,
    ActivityMonetaryBudgetResponse, ActivityMonetaryBudgetSetRequest,
};
use serde_json::json;

use super::{body, invalid, upstream_error, with_id};
use crate::{
    api_error::ApiError,
    state::{AppState, current_uid},
    translation::monetary_budget as translation,
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
    query: Result<Query<ActivityMonetaryBudgetQuery>, QueryRejection>,
) -> Result<Json<ActivityMonetaryBudgetResponse>, ApiError> {
    query.map_err(|_| invalid("Monetary-budget queries accept no owner or state selectors"))?;
    let owner = owner_uid()?;
    let value = state
        .clawd
        .call(Command::ActivityMonetaryBudgetGet, with_id(&id, json!({}))?)
        .await
        .map_err(upstream_error)?;
    translation::get(value, &id, owner)
        .map(Json)
        .map_err(ApiError::bad_gateway)
}

pub async fn set(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<ActivityMonetaryBudgetSetRequest>, JsonRejection>,
) -> Result<Json<ActivityMonetaryBudget>, ApiError> {
    let request = body(request)?;
    request.validate_shape().map_err(invalid)?;
    let owner = owner_uid()?;
    let value = state
        .clawd
        .call(Command::ActivityMonetaryBudgetSet, with_id(&id, &request)?)
        .await
        .map_err(upstream_error)?;
    let budget = translation::policy(value, &id, owner).map_err(ApiError::bad_gateway)?;
    if !budget.matches_set(&id, &request) {
        return Err(ApiError::bad_gateway(
            "Monetary-budget acknowledgement did not match the submitted policy or revision",
        ));
    }
    Ok(Json(budget))
}

pub async fn enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<ActivityMonetaryBudgetEnabledRequest>, JsonRejection>,
) -> Result<Json<ActivityMonetaryBudget>, ApiError> {
    let request = body(request)?;
    request.validate_shape().map_err(invalid)?;
    let owner = owner_uid()?;
    let value = state
        .clawd
        .call(
            Command::ActivityMonetaryBudgetEnabled,
            with_id(&id, &request)?,
        )
        .await
        .map_err(upstream_error)?;
    let budget = translation::policy(value, &id, owner).map_err(ApiError::bad_gateway)?;
    if !budget.matches_enabled(&id, &request) {
        return Err(ApiError::bad_gateway(
            "Monetary-budget acknowledgement did not match the requested enabled state or revision",
        ));
    }
    Ok(Json(budget))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/activities/monetary_budget.rs"
    ));
}
