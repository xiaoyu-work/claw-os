use super::clawd;
use crate::agent::memory::inspection::{ForgetRequest, InspectRequest, ReadRequest, UpdateRequest};
use crate::clawd::routes::Command;
use axum::{http::StatusCode, Json};
use serde_json::Value;

type Failure = (StatusCode, Json<Value>);

async fn request<T: serde::Serialize>(command: Command, body: T) -> Result<Json<Value>, Failure> {
    let params = serde_json::to_value(body).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":error.to_string()})),
        )
    })?;
    clawd::request(command, params)
        .await
        .map(Json)
        .map_err(clawd::RpcError::into_api_error)
}
pub async fn inspect(Json(body): Json<InspectRequest>) -> Result<Json<Value>, Failure> {
    request(Command::MemoryInspect, body).await
}
pub async fn read(Json(body): Json<ReadRequest>) -> Result<Json<Value>, Failure> {
    request(Command::MemoryRead, body).await
}
pub async fn update(Json(body): Json<UpdateRequest>) -> Result<Json<Value>, Failure> {
    request(Command::MemoryUpdate, body).await
}
pub async fn forget(Json(body): Json<ForgetRequest>) -> Result<Json<Value>, Failure> {
    request(Command::MemoryForget, body).await
}
