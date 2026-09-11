//! Stateless adapters to the same owner-scoped Activity service used by `cos`.
//! Unlike chat SSE, a work submission has no cancel-on-disconnect guard.

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection, rejection::QueryRejection},
    http::StatusCode,
};
use clawd_client::{Command, Error as BrokerError, ErrorCode as BrokerErrorCode};
use cos_agent_protocol::{
    ActivityCreateRequest, ActivityDetailResponse, ActivityListQuery, ActivityListResponse,
    ActivityObjectAttachRequest, ActivityObjectsResponse, ActivityOperationPreview,
    ActivityOperationPreviewRequest, ActivityRunRequest, ActivityTransitionRequest,
    ActivityUpdateRequest, ActivityView, ActivityWorkResponse, ErrorCode,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{api_error::ApiError, state::AppState, translation::activities as translation};

pub async fn list(
    State(state): State<AppState>,
    query: Result<Query<ActivityListQuery>, QueryRejection>,
) -> Result<Json<ActivityListResponse>, ApiError> {
    let Query(query) = query.map_err(|_| invalid("invalid Activity list query"))?;
    if query.limit.is_some_and(|limit| !(1..=100).contains(&limit)) {
        return Err(invalid("Activity limit must be between 1 and 100"));
    }
    let value = state
        .clawd
        .call(Command::ActivityList, encode(query)?)
        .await
        .map_err(upstream_error)?;
    translation::list(value)
        .map(Json)
        .map_err(ApiError::bad_gateway)
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActivityDetailResponse>, ApiError> {
    let value = state
        .clawd
        .call(Command::ActivityGet, with_id(&id, json!({"limit": 100}))?)
        .await
        .map_err(upstream_error)?;
    let mut detail = translation::detail(value).map_err(ApiError::bad_gateway)?;
    if detail.activity.id != id {
        return Err(ApiError::bad_gateway(
            "Activity response id did not match the request",
        ));
    }
    let pending = match state
        .clawd
        .call(Command::PermissionPending, json!({"limit": 100}))
        .await
    {
        Ok(value) => translation::approvals(value, &detail),
        Err(error) => Err(error.to_string()),
    };
    match pending {
        Ok(approvals) => detail.pending_approvals = approvals,
        Err(error) => detail.approvals_error = Some(error),
    }
    Ok(Json(detail))
}

pub async fn create(
    State(state): State<AppState>,
    request: Result<Json<ActivityCreateRequest>, JsonRejection>,
) -> Result<Json<ActivityView>, ApiError> {
    let request = body(request)?;
    let value = state
        .clawd
        .call(Command::ActivityCreate, encode(request)?)
        .await
        .map_err(upstream_error)?;
    translation::activity(value)
        .map(Json)
        .map_err(ApiError::bad_gateway)
}

pub async fn objects(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActivityObjectsResponse>, ApiError> {
    let value = state
        .clawd
        .call(Command::ActivityObjects, with_id(&id, json!({}))?)
        .await
        .map_err(upstream_error)?;
    let response = translation::objects(value).map_err(ApiError::bad_gateway)?;
    if response.activity_id != id {
        return Err(ApiError::bad_gateway(
            "Activity object response id did not match the request",
        ));
    }
    Ok(Json(response))
}

pub async fn attach_object(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<ActivityObjectAttachRequest>, JsonRejection>,
) -> Result<Json<ActivityView>, ApiError> {
    mutate(&state, &id, Command::ActivityObjectAttach, body(request)?).await
}

pub async fn operation_preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<ActivityOperationPreviewRequest>, JsonRejection>,
) -> Result<Json<ActivityOperationPreview>, ApiError> {
    let request = body(request)?;
    let value = state
        .clawd
        .call(Command::ActivityOperationPreview, with_id(&id, &request)?)
        .await
        .map_err(upstream_error)?;
    let preview = translation::operation_preview(value).map_err(ApiError::bad_gateway)?;
    if preview.app_id != request.app_id || preview.operation != request.operation {
        return Err(ApiError::bad_gateway(
            "Activity preview operation did not match the request",
        ));
    }
    Ok(Json(preview))
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<ActivityUpdateRequest>, JsonRejection>,
) -> Result<Json<ActivityView>, ApiError> {
    mutate(&state, &id, Command::ActivityUpdate, body(request)?).await
}

pub async fn transition(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<ActivityTransitionRequest>, JsonRejection>,
) -> Result<Json<ActivityView>, ApiError> {
    mutate(&state, &id, Command::ActivityTransition, body(request)?).await
}

pub async fn run(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Result<Json<ActivityRunRequest>, JsonRejection>,
) -> Result<Json<ActivityWorkResponse>, ApiError> {
    let value = state
        .clawd
        .call(Command::ActivityRun, with_id(&id, body(request)?)?)
        .await
        .map_err(upstream_error)?;
    let work = translation::work(value).map_err(ApiError::bad_gateway)?;
    if work
        .activity_id
        .as_deref()
        .is_some_and(|activity_id| activity_id != id)
    {
        return Err(ApiError::bad_gateway(
            "Activity work response id did not match the request",
        ));
    }
    Ok(Json(work))
}

pub async fn retry_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActivityWorkResponse>, ApiError> {
    let value = state
        .clawd
        .call(Command::TaskRetry, with_id(&id, json!({}))?)
        .await
        .map_err(upstream_error)?;
    translation::work(value)
        .map(Json)
        .map_err(ApiError::bad_gateway)
}

async fn mutate(
    state: &AppState,
    id: &str,
    command: Command,
    request: impl Serialize,
) -> Result<Json<ActivityView>, ApiError> {
    let value = state
        .clawd
        .call(command, with_id(id, request)?)
        .await
        .map_err(upstream_error)?;
    let activity = translation::activity(value).map_err(ApiError::bad_gateway)?;
    if activity.id != id {
        return Err(ApiError::bad_gateway(
            "Activity response id did not match the request",
        ));
    }
    Ok(Json(activity))
}

fn body<T>(request: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    request
        .map(|Json(value)| value)
        .map_err(|_| invalid("invalid Activity request"))
}

fn encode(request: impl Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(request).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "could not encode Activity request",
        )
    })
}

fn with_id(id: &str, request: impl Serialize) -> Result<Value, ApiError> {
    if id.trim().is_empty() {
        return Err(invalid("id is required"));
    }
    let Value::Object(mut params) = encode(request)? else {
        return Err(invalid("Activity request must be an object"));
    };
    params.insert("id".into(), Value::String(id.to_string()));
    Ok(Value::Object(params))
}

fn invalid(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest, message)
}

fn upstream_error(error: BrokerError) -> ApiError {
    let (status, code) = match &error {
        BrokerError::Remote(remote) => match remote.code {
            BrokerErrorCode::InvalidJson | BrokerErrorCode::InvalidRequest => {
                (StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest)
            }
            BrokerErrorCode::NotAuthorized => (StatusCode::FORBIDDEN, ErrorCode::Unauthorized),
            BrokerErrorCode::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::ServiceUnavailable,
            ),
            BrokerErrorCode::UnknownCommand => {
                (StatusCode::NOT_IMPLEMENTED, ErrorCode::NotImplemented)
            }
            _ => (StatusCode::BAD_GATEWAY, ErrorCode::UpstreamError),
        },
        BrokerError::Client(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::ServiceUnavailable,
        ),
    };
    ApiError::new(status, code, error.to_string())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/activities.rs"
    ));
}
