use crate::{
    api_error::ApiError,
    state::{AppState, current_uid},
};
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use clawd_client::Command;
use cos_agent_protocol::{
    ErrorCode,
    nearby::{NearbyRequest, NearbyResponse, NearbyStatusRequest, NearbyView},
};

fn invalid(error: impl std::fmt::Display) -> ApiError {
    ApiError::bad_gateway(format!("Invalid nearby sharing response: {error}"))
}
pub async fn status(
    State(state): State<AppState>,
    Query(_): Query<NearbyStatusRequest>,
) -> Result<Json<NearbyView>, ApiError> {
    let owner = current_uid().map_err(invalid)?;
    let value = state
        .clawd
        .call(Command::NearbyStatus, serde_json::json!({}))
        .await
        .map_err(invalid)?;
    let view: NearbyView = serde_json::from_value(value).map_err(invalid)?;
    view.validate(owner).map_err(invalid)?;
    Ok(Json(view))
}
pub async fn control(
    State(state): State<AppState>,
    Json(body): Json<NearbyRequest>,
) -> Result<Json<NearbyResponse>, ApiError> {
    body.request
        .validate()
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest, e))?;
    let owner = current_uid().map_err(invalid)?;
    let value = state
        .clawd
        .call(
            Command::NearbyControl,
            serde_json::to_value(&body).map_err(invalid)?,
        )
        .await
        .map_err(invalid)?;
    let response: NearbyResponse = serde_json::from_value(value).map_err(invalid)?;
    response.validate(owner, &body.request).map_err(invalid)?;
    Ok(Json(response))
}
