use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::clawd::protocol::Request;
use crate::clawd::routes::Command;

pub type ApiError = (StatusCode, Json<Value>);

pub async fn request(command: Command, params: Value) -> Result<Value, RpcError> {
    let request = Request::build(command, params);
    let response = crate::clawd::client::request(crate::paths::clawd_socket_path(), request)
        .await
        .map_err(|error| RpcError {
            status: StatusCode::BAD_GATEWAY,
            message: error.to_string(),
        })?;
    if response.ok {
        response.result.ok_or_else(|| RpcError {
            status: StatusCode::BAD_GATEWAY,
            message: "clawd returned no result".to_string(),
        })
    } else {
        Err(RpcError {
            status: StatusCode::BAD_REQUEST,
            message: response
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "clawd request failed".to_string()),
        })
    }
}

#[derive(Debug)]
pub struct RpcError {
    status: StatusCode,
    message: String,
}

impl RpcError {
    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn into_api_error(self) -> ApiError {
        (self.status, Json(json!({ "error": self.message })))
    }
}
