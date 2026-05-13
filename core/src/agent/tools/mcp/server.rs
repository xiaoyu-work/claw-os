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
    /// or errors. Each request is handled in-line; for long-running
    /// tools consider spawning a task per request — left as a
    /// follow-up.
    pub async fn serve(self, transport: impl Transport) -> Result<(), ServerError> {
        let t = Arc::new(transport);
        loop {
            let frame = match t.recv().await? {
                Some(f) => f,
                None => return Ok(()),
            };
            let parsed: Result<JsonRpcRequest, _> = serde_json::from_str(&frame);
            match parsed {
                Ok(req) => {
                    let resp = self.handle(req).await;
                    let body = serde_json::to_string(&resp).unwrap_or_else(|_| {
                        serde_json::to_string(&JsonRpcResponse::err(
                            super::protocol::RequestId::Num(0),
                            JsonRpcError::new(ERR_INTERNAL, "encode failed"),
                        ))
                        .unwrap()
                    });
                    t.send(body).await?;
                }
                Err(err) => {
                    // We can't know the request id; fall back to id 0.
                    let resp = JsonRpcResponse::err(
                        super::protocol::RequestId::Num(0),
                        JsonRpcError::new(
                            super::protocol::ERR_PARSE,
                            format!("parse error: {err}"),
                        ),
                    );
                    t.send(serde_json::to_string(&resp).unwrap_or_default())
                        .await?;
                }
            }
        }
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
    use super::super::client::{ClientError, McpClient};
    use super::super::protocol::{ClientCapabilities, ContentItem, Implementation};
    use super::super::transport::in_memory_pair;
    use super::*;
    use crate::agent::tools::builtin::Echo;

    fn registry_with_echo() -> Arc<ToolRegistry> {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(Echo));
        Arc::new(r)
    }

    #[tokio::test]
    async fn initialize_returns_tools_capability() {
        let (client_t, server_t) = in_memory_pair();
        let server = McpServer::new("cos-test", "0.0.1", registry_with_echo());
        let server_handle = tokio::spawn(server.serve(server_t));

        let client = McpClient::new(client_t);
        client.start().await;
        let init = client
            .initialize(
                Implementation {
                    name: "test-client".into(),
                    version: "0.0.1".into(),
                },
                ClientCapabilities::default(),
            )
            .await
            .unwrap();
        assert_eq!(init.protocol_version, PROTOCOL_VERSION);
        assert_eq!(init.server_info.name, "cos-test");
        assert!(init.capabilities.tools.is_some());
        drop(client);
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn tools_list_reflects_registry() {
        let (client_t, server_t) = in_memory_pair();
        let server = McpServer::new("cos", "0", registry_with_echo());
        let server_handle = tokio::spawn(server.serve(server_t));

        let client = McpClient::new(client_t);
        client.start().await;
        let listing = client.list_tools().await.unwrap();
        assert!(listing.tools.iter().any(|t| t.name == "echo"));
        drop(client);
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn tools_call_executes_registered_tool() {
        let (client_t, server_t) = in_memory_pair();
        let server = McpServer::new("cos", "0", registry_with_echo());
        let server_handle = tokio::spawn(server.serve(server_t));

        let client = McpClient::new(client_t);
        client.start().await;
        let result = client
            .call_tool("echo", Some(serde_json::json!({"text": "hi"})))
            .await
            .unwrap();
        assert!(result.is_error.unwrap_or(false) == false);
        match result.content.first() {
            Some(ContentItem::Text { text }) => assert!(text.contains("hi")),
            _ => panic!("expected text content"),
        }
        drop(client);
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn unknown_method_yields_method_not_found() {
        let (client_t, server_t) = in_memory_pair();
        let server = McpServer::new("cos", "0", registry_with_echo());
        let server_handle = tokio::spawn(server.serve(server_t));

        let client = McpClient::new(client_t);
        client.start().await;
        let err = client.request("not/a/method", None).await.unwrap_err();
        match err {
            ClientError::Server { code, .. } => {
                assert_eq!(code, ERR_METHOD_NOT_FOUND);
            }
            other => panic!("expected Server error, got {other:?}"),
        }
        drop(client);
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn unknown_tool_yields_method_not_found() {
        let (client_t, server_t) = in_memory_pair();
        let server = McpServer::new("cos", "0", registry_with_echo());
        let server_handle = tokio::spawn(server.serve(server_t));

        let client = McpClient::new(client_t);
        client.start().await;
        let err = client.call_tool("missing", None).await.unwrap_err();
        match err {
            ClientError::Server { code, .. } => {
                assert_eq!(code, ERR_METHOD_NOT_FOUND);
            }
            other => panic!("expected Server error, got {other:?}"),
        }
        drop(client);
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn parse_error_does_not_kill_server() {
        let (client_t, server_t) = in_memory_pair();
        let server = McpServer::new("cos", "0", registry_with_echo());
        let server_handle = tokio::spawn(server.serve(server_t));

        // Send a junk frame.
        client_t.send("not json at all".into()).await.unwrap();
        // Read the parse-error response.
        let resp = client_t.recv().await.unwrap().unwrap();
        let parsed: JsonRpcResponse = serde_json::from_str(&resp).unwrap();
        assert!(parsed.error.is_some());

        // Now drive a real request — server must still be alive.
        let client = McpClient::new(client_t);
        client.start().await;
        let _ = client.request("ping", None).await.unwrap();
        drop(client);
        let _ = server_handle.await;
    }
}
