use crate::{api_error::ApiError, state::AppState};
use axum::{Json, extract::State};
use clawd_client::Command;
use cos_agent_protocol::{
    MemoryEntry, MemoryForgetRequest, MemoryInspectRequest, MemoryInventory, MemoryReadRequest,
    MemoryUpdateRequest,
};

async fn request<T: serde::Serialize, R: serde::de::DeserializeOwned>(
    state: AppState,
    command: Command,
    body: T,
) -> Result<Json<R>, ApiError> {
    let params =
        serde_json::to_value(body).map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    let value = state
        .clawd
        .call(command, params)
        .await
        .map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    serde_json::from_value(value)
        .map(Json)
        .map_err(|error| ApiError::bad_gateway(format!("Invalid owner memory response: {error}")))
}
pub async fn inspect(
    State(state): State<AppState>,
    Json(body): Json<MemoryInspectRequest>,
) -> Result<Json<MemoryInventory>, ApiError> {
    request(state, Command::MemoryInspect, body).await
}
pub async fn read(
    State(state): State<AppState>,
    Json(body): Json<MemoryReadRequest>,
) -> Result<Json<MemoryEntry>, ApiError> {
    request(state, Command::MemoryRead, body).await
}
pub async fn update(
    State(state): State<AppState>,
    Json(body): Json<MemoryUpdateRequest>,
) -> Result<Json<MemoryEntry>, ApiError> {
    request(state, Command::MemoryUpdate, body).await
}
pub async fn forget(
    State(state): State<AppState>,
    Json(body): Json<MemoryForgetRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    request(state, Command::MemoryForget, body).await
}
