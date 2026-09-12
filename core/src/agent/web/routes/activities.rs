//! Authenticated HTTP presentation of the shared, owner-scoped Activity broker.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::Json;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::clawd::routes::Command;
use crate::clawd::wire::requests::{
    ActivityCreate, ActivityGet, ActivityList, ActivityObjectAttach, ActivityObjectState,
    ActivityObjectStateRecord, ActivityObjects, ActivityOperationPreview, ActivityReceipts,
    ActivityRun, ActivityTransition, ActivityUpdate, NoBody,
};
use crate::clawd::wire::requests::{
    ActivityExecutionLimitsEnabled, ActivityExecutionLimitsGet, ActivityExecutionLimitsSet,
};

use super::clawd::ApiError;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailQuery {
    limit: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectStateQuery {
    reference: Option<String>,
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

pub async fn objects(
    Path(id): Path<String>,
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    query.map_err(|error| bad_request(error.body_text()))?;
    request(
        Command::ActivityObjects,
        with_id::<ActivityObjects>(id, json!({}))?,
    )
    .await
}

pub async fn attach_object(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityObjectAttach,
        with_id::<ActivityObjectAttach>(id, json_body(body)?)?,
    )
    .await
}

pub async fn operation_preview(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityOperationPreview,
        with_id::<ActivityOperationPreview>(id, json_body(body)?)?,
    )
    .await
}

pub async fn receipts(
    Path(id): Path<String>,
    query: Result<Query<DetailQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|error| bad_request(error.body_text()))?;
    request(
        Command::ActivityReceipts,
        with_id::<ActivityReceipts>(id, json!({ "limit": query.limit }))?,
    )
    .await
}

pub async fn object_state(
    Path(id): Path<String>,
    query: Result<Query<ObjectStateQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|error| bad_request(error.body_text()))?;
    request(
        Command::ActivityObjectStateList,
        with_id::<ActivityObjectState>(
            id,
            json!({
                "reference":query.reference,"limit":query.limit,
            }),
        )?,
    )
    .await
}

pub async fn record_object_state(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityObjectStateRecord,
        with_id::<ActivityObjectStateRecord>(id, json_body(body)?)?,
    )
    .await
}

pub async fn execution_limits(
    Path(id): Path<String>,
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    query.map_err(|error| bad_request(error.body_text()))?;
    request(
        Command::ActivityExecutionLimitsGet,
        with_id::<ActivityExecutionLimitsGet>(id, json!({}))?,
    )
    .await
}

pub async fn set_execution_limits(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityExecutionLimitsSet,
        with_id::<ActivityExecutionLimitsSet>(id, json_body(body)?)?,
    )
    .await
}

pub async fn enable_execution_limits(
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::ActivityExecutionLimitsEnabled,
        with_id::<ActivityExecutionLimitsEnabled>(id, json_body(body)?)?,
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
