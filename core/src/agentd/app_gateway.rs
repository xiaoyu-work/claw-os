//! Worker-side client seam for the daemon-owned MCP App Gateway.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::protocol::AppGatewayRequest;
use crate::agent::tools::mcp::protocol::CallToolResult;

#[async_trait]
pub(crate) trait AppGateway: Send + Sync {
    async fn call(&self, request: AppGatewayRequest) -> Result<CallToolResult, String>;
}

fn slot() -> &'static OnceLock<Arc<dyn AppGateway>> {
    static GATEWAY: OnceLock<Arc<dyn AppGateway>> = OnceLock::new();
    &GATEWAY
}

pub(crate) fn install(gateway: Arc<dyn AppGateway>) -> Result<(), String> {
    slot()
        .set(gateway)
        .map_err(|_| "agentd App Gateway is already installed".to_string())
}

pub(crate) fn available() -> bool {
    slot().get().is_some()
}

pub(crate) async fn call(
    app_id: String,
    tool: String,
    arguments: Value,
    timeout: Duration,
) -> Result<CallToolResult, String> {
    let gateway = slot()
        .get()
        .cloned()
        .ok_or_else(|| "agentd App Gateway is unavailable".to_string())?;
    let timeout_ms = timeout
        .as_millis()
        .min(u128::from(super::protocol::MAX_APP_GATEWAY_TIMEOUT_MS)) as u64;
    let request = AppGatewayRequest {
        app_id,
        tool,
        arguments,
        timeout_ms,
    };
    request.validate()?;
    gateway.call(request).await
}
