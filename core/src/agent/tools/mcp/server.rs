//! Inbound MCP server — accept JSON-RPC over a [`Transport`] and
//! dispatch to a [`crate::agent::tools::registry::ToolRegistry`].
//!
//! The kernel uses this to expose its agent tool catalogue to
//! external MCP clients (Claude Desktop / Cursor / Cody / etc.).
//! The handler set is intentionally minimal:
//!
//! * `initialize` — protocol handshake, advertise tool capability.
//! * `tools/list` — reflect the registry into MCP's tool descriptor
//!   shape.
//! * `tools/call` — exec the named tool and wrap the result.
//! * `ping` — round-trip liveness check.
//!
//! Unknown methods return `ERR_METHOD_NOT_FOUND` so the spec-defined
//! optional methods (resources/*, prompts/*) degrade cleanly.

use std::sync::Arc;

use serde_json::{json, Value};

use super::protocol::{
    CallToolParams, CallToolResult, ContentItem, Implementation, InitializeParams,
    InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse, ListToolsResult,
    ServerCapabilities, ToolDescriptor, ToolsCapability, ERR_INTERNAL, ERR_INVALID_PARAMS,
    ERR_METHOD_NOT_FOUND, PROTOCOL_VERSION,
};
use super::transport::{Transport, TransportError};
use crate::agent::tools::registry::ToolRegistry;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
}

pub struct McpServer {
    name: String,
    version: String,
    registry: Arc<ToolRegistry>,
}

impl McpServer {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            registry,
        }
    }

    /// Run the read/dispatch/write loop until the transport closes
    /// or errors.
    ///
    /// Each request is dispatched on its own `tokio::spawn`'d task so
    /// a slow tool (e.g. a 30-second LLM call inside a `tools/call`)
    /// does not head-of-line block subsequent requests. Responses are
    /// serialized through the single transport `send` channel; the
    /// `Transport` impls hold an internal mutex around the writer.
    ///
    /// MCP does not require strict request/response ordering (each
    /// response carries the request id), so interleaving is safe and
    /// well within spec.
    pub async fn serve(self, transport: impl Transport) -> Result<(), ServerError> {
        let t = Arc::new(transport);
        let me = Arc::new(self);
        let mut handlers: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        loop {
            let frame = match t.recv().await? {
                Some(f) => f,
                None => break,
            };
            // Reap finished handlers opportunistically so the vec
            // doesn't grow unbounded for long-lived servers.
            handlers.retain(|h| !h.is_finished());

            let parsed: Result<JsonRpcRequest, _> = serde_json::from_str(&frame);
            let server = me.clone();
            let t = t.clone();
            handlers.push(tokio::spawn(async move {
                let resp = match parsed {
                    Ok(req) => server.handle(req).await,
                    Err(err) => JsonRpcResponse::err(
                        super::protocol::RequestId::Num(0),
                        JsonRpcError::new(
                            super::protocol::ERR_PARSE,
                            format!("parse error: {err}"),
                        ),
                    ),
                };
                let body = serde_json::to_string(&resp).unwrap_or_else(|_| {
                    serde_json::to_string(&JsonRpcResponse::err(
                        super::protocol::RequestId::Num(0),
                        JsonRpcError::new(ERR_INTERNAL, "encode failed"),
                    ))
                    .unwrap_or_default()
                });
                let _ = t.send(body).await;
            }));
        }
        // Best-effort drain of in-flight handlers before returning so
        // queued responses get a chance to flush before the transport
        // closes.
        for h in handlers {
            let _ = h.await;
        }
        Ok(())
    }

    async fn handle(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        match req.method.as_str() {
            "initialize" => self.handle_initialize(req),
            "ping" => JsonRpcResponse::ok(id, json!({})),
            "tools/list" => self.handle_tools_list(id),
            "tools/call" => self.handle_tools_call(req).await,
            other => JsonRpcResponse::err(
                id,
                JsonRpcError::new(ERR_METHOD_NOT_FOUND, format!("method not found: {other}")),
            ),
        }
    }

    fn handle_initialize(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let _params: InitializeParams = match req.params {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::err(
                        id,
                        JsonRpcError::new(ERR_INVALID_PARAMS, e.to_string()),
                    );
                }
            },
            None => {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, "missing params"),
                );
            }
        };
        let result = InitializeResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(false),
                }),
                ..Default::default()
            },
            server_info: Implementation {
                name: self.name.clone(),
                version: self.version.clone(),
            },
            instructions: None,
        };
        match serde_json::to_value(&result) {
            Ok(v) => JsonRpcResponse::ok(id, v),
            Err(e) => JsonRpcResponse::err(id, JsonRpcError::new(ERR_INTERNAL, e.to_string())),
        }
    }

    fn handle_tools_list(&self, id: super::protocol::RequestId) -> JsonRpcResponse {
        let mut tools: Vec<ToolDescriptor> = Vec::new();
        for name in self.registry.names() {
            if let Some(t) = self.registry.get(name) {
                tools.push(ToolDescriptor {
                    name: t.name().to_string(),
                    description: Some(t.description().to_string()),
                    input_schema: t.input_schema(),
                });
            }
        }
        let result = ListToolsResult {
            tools,
            next_cursor: None,
        };
        match serde_json::to_value(&result) {
            Ok(v) => JsonRpcResponse::ok(id, v),
            Err(e) => JsonRpcResponse::err(id, JsonRpcError::new(ERR_INTERNAL, e.to_string())),
        }
    }

    async fn handle_tools_call(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let params: CallToolParams = match req.params {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::err(
                        id,
                        JsonRpcError::new(ERR_INVALID_PARAMS, e.to_string()),
                    );
                }
            },
            None => {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, "missing params"),
                );
            }
        };
        let tool = match self.registry.get(&params.name) {
            Some(t) => t,
            None => {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(
                        ERR_METHOD_NOT_FOUND,
                        format!("tool not registered: {}", params.name),
                    ),
                );
            }
        };
        let arguments = params.arguments.unwrap_or(Value::Null);
        let result = tool.exec(arguments).await;
        let body = CallToolResult {
            content: vec![ContentItem::Text {
                text: result.content,
            }],
            is_error: if result.is_error { Some(true) } else { None },
        };
        match serde_json::to_value(&body) {
            Ok(v) => JsonRpcResponse::ok(id, v),
            Err(e) => JsonRpcResponse::err(id, JsonRpcError::new(ERR_INTERNAL, e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/mcp/server.rs"
    ));
}
