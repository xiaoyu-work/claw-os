//! Authenticated human nearby-sharing adapters; ownership is never client input.

use super::clawd::ApiError;
use crate::{
    agent::web::auth::AuthenticatedToken,
    clawd::{
        routes::Command,
        wire::requests::{NearbyControl, NoBody},
    },
};
use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Extension, Query,
    },
    http::StatusCode,
    Json,
};
use clawd_client::nearby::{NearbyResponse, NearbyView};
use serde_json::json;

pub async fn status(
    Extension(auth): Extension<AuthenticatedToken>,
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<NearbyView>, ApiError> {
    query.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":e.body_text()})),
        )
    })?;
    let value = super::clawd::request(Command::NearbyStatus, json!({}))
        .await
        .map_err(super::clawd::RpcError::into_api_error)?;
    let view: NearbyView = serde_json::from_value(value).map_err(bad_gateway)?;
    view.validate(auth.uid).map_err(bad_gateway)?;
    Ok(Json(view))
}

pub async fn control(
    Extension(auth): Extension<AuthenticatedToken>,
    body: Result<Json<NearbyControl>, JsonRejection>,
) -> Result<Json<NearbyResponse>, ApiError> {
    let Json(body) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":e.body_text()})),
        )
    })?;
    body.request
        .validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({"error":e}))))?;
    let value = super::clawd::request(Command::NearbyControl, json!({"request":body.request}))
        .await
        .map_err(super::clawd::RpcError::into_api_error)?;
    let response: NearbyResponse = serde_json::from_value(value).map_err(bad_gateway)?;
    response
        .validate(auth.uid, &body.request)
        .map_err(bad_gateway)?;
    Ok(Json(response))
}

fn bad_gateway(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"error":format!("Invalid nearby response: {error}")})),
    )
}
