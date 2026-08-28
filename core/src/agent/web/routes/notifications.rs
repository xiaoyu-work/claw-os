//! Authenticated Web notification API and SSE projection.

use std::convert::Infallible;

use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::{header, Response, StatusCode};
use axum::Json;
use bytes::Bytes;
use futures_util::stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::web::sse;
use crate::clawd::routes::Command;

type ApiError = (StatusCode, Json<Value>);
type SseFrame = Result<Bytes, Infallible>;

#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    include_dismissed: bool,
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct StreamBody {
    #[serde(default)]
    cursor: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PreferencesBody {
    web_enabled: bool,
    desktop_enabled: bool,
    ntfy_enabled: bool,
    web_min_severity: String,
    desktop_min_severity: String,
    ntfy_min_severity: String,
    #[serde(default)]
    muted_kinds: Vec<String>,
    #[serde(default)]
    dnd_start_minute_utc: Option<u16>,
    #[serde(default)]
    dnd_end_minute_utc: Option<u16>,
    critical_bypasses_dnd: bool,
    retention_days: u16,
    ntfy_server: String,
    #[serde(default)]
    ntfy_topic: Option<String>,
}

pub async fn list(Query(query): Query<ListQuery>) -> Result<Json<Value>, ApiError> {
    request(
        Command::NotificationList,
        json!({
            "include_dismissed": query.include_dismissed,
            "limit": query.limit.unwrap_or(100),
        }),
    )
    .await
    .map(Json)
}

pub async fn stream(body: Option<Json<StreamBody>>) -> Response<Body> {
    let cursor = body.map(|Json(body)| body.cursor).unwrap_or(0);
    let events = stream::unfold(
        StreamState {
            cursor,
            finished: false,
        },
        |mut state| async move {
            if state.finished {
                return None;
            }
            let result = request(
                Command::NotificationSubscribe,
                json!({
                    "cursor": state.cursor,
                    "limit": 100,
                    "timeout_ms": 15_000,
                }),
            )
            .await;
            let frame = match result {
                Ok(result) => {
                    if let Some(cursor) = result.get("cursor").and_then(Value::as_u64) {
                        state.cursor = cursor;
                    }
                    let changes = result
                        .get("changes")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if changes.is_empty() {
                        sse::encode_comment("ping")
                    } else {
                        changes
                            .iter()
                            .map(|change| sse::encode_event("notification", change))
                            .collect::<String>()
                    }
                }
                Err((_, Json(error))) => {
                    state.finished = true;
                    sse::encode_event("error", &error)
                }
            };
            Some((Ok::<_, Infallible>(Bytes::from(frame)), state))
        },
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(events))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

struct StreamState {
    cursor: u64,
    finished: bool,
}

pub async fn mark_read(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    request(Command::NotificationRead, json!({ "id": id }))
        .await
        .map(Json)
}

pub async fn acknowledge(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    request(Command::NotificationAcknowledge, json!({ "id": id }))
        .await
        .map(Json)
}

pub async fn dismiss(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    request(Command::NotificationDismiss, json!({ "id": id }))
        .await
        .map(Json)
}

pub async fn delivered(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    request(
        Command::NotificationDeliveryComplete,
        json!({
            "id": id,
            "channel": "web",
            "status": "delivered",
        }),
    )
    .await
    .map(Json)
}

pub async fn claim_web_deliveries() -> Result<Json<Value>, ApiError> {
    request(
        Command::NotificationDeliveryClaim,
        json!({
            "channel": "web",
            "limit": 50,
            "lease_ms": 30_000,
        }),
    )
    .await
    .map(Json)
}

pub async fn get_preferences() -> Result<Json<Value>, ApiError> {
    request(Command::NotificationPreferencesGet, json!({}))
        .await
        .map(Json)
}

pub async fn set_preferences(
    Json(preferences): Json<PreferencesBody>,
) -> Result<Json<Value>, ApiError> {
    request(
        Command::NotificationPreferencesSet,
        serde_json::to_value(preferences)
            .map_err(|error| bad_request(format!("invalid preferences: {error}")))?,
    )
    .await
    .map(Json)
}

async fn request(command: Command, params: Value) -> Result<Value, ApiError> {
    super::clawd::request(command, params)
        .await
        .map_err(super::clawd::RpcError::into_api_error)
}

fn bad_request(message: String) -> ApiError {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}
