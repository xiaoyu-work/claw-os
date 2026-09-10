//! Authenticated HTTP presentation of the shared, owner-scoped Activity broker.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::Json;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::clawd::routes::Command;
use crate::clawd::wire::requests::{
    ActivityCreate, ActivityGet, ActivityList, ActivityRun, ActivityTransition, ActivityUpdate,
};

use super::clawd::ApiError;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailQuery {
    limit: Option<u64>,
}

pub async fn list(
    query: Result<Query<ActivityList>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|error| bad_request(error.body_text()))?;
    request(Command::ActivityList, query).await
}

pub async fn create(
    body: Result<Json<ActivityCreate>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(Command::ActivityCreate, json_body(body)?).await
}

pub async fn get(
    Path(id): Path<String>,
    query: Result<Query<DetailQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|error| bad_request(error.body_text()))?;
    request(
        Command::ActivityGet,
        with_id::<ActivityGet>(id, json!({ "limit": query.limit }))?,
    )
    .await
}

pub async fn update(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityUpdate,
        with_id::<ActivityUpdate>(id, json_body(body)?)?,
    )
    .await
}

pub async fn transition(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityTransition,
        with_id::<ActivityTransition>(id, json_body(body)?)?,
    )
    .await
}

pub async fn run(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityRun,
        with_id::<ActivityRun>(id, json_body(body)?)?,
    )
    .await
}

fn json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(body)| body)
        .map_err(|error| (error.status(), Json(json!({ "error": error.body_text() }))))
}

fn with_id<T: DeserializeOwned>(id: String, mut body: Value) -> Result<T, ApiError> {
    let fields = body
        .as_object_mut()
        .ok_or_else(|| bad_request("Activity body must be a JSON object"))?;
    if fields.contains_key("id") {
        return Err(bad_request("Activity id belongs in the URL, not the body"));
    }
    fields.insert("id".to_string(), json!(id));
    // The broker's bounded, closed DTOs remain the request contract. There is
    // no Web-owned lifecycle, persistence, or caller-supplied owner identity.
    serde_json::from_value(body).map_err(|error| bad_request(error.to_string()))
}

async fn request(command: Command, body: impl Serialize) -> Result<Json<Value>, ApiError> {
    let params = serde_json::to_value(body).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("encode Activity request: {error}") })),
        )
    })?;
    super::clawd::request(command, params)
        .await
        .map(Json)
        .map_err(super::clawd::RpcError::into_api_error)
}

fn bad_request(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into() })),
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/routes/activities.rs"
    ));
}
