//! Inbound MCP server — accept JSON-RPC over a [`Transport`] and
//! dispatch to registered [`Tool`] handlers.
//!
//! Differs from the kernel's `core/src/agent/tools/mcp/server.rs` in
//! one important way: it owns its own simple `Vec<Arc<dyn Tool>>`
//! instead of borrowing a kernel `ToolRegistry`. Apps that embed
//! this crate need *only* this crate — they don't pull in any of the
//! kernel's runtime state (caps, audit, registry filters, …). All of
//! that gating happens on the kernel side **before** the call is
//! forwarded over MCP, so the App is in a "trust the kernel" position
//! by the time `exec` runs.
//!
//! ## Method surface
//!
//! * `initialize` — protocol handshake; advertises the `tools`
//!   capability.
//! * `tools/list` — reflect the registered tools into MCP
//!   `ToolDescriptor`s.
//! * `tools/call` — exec the named tool, wrap the result in
//!   `CallToolResult`.
//! * `ping` — round-trip liveness.
//! * `notifications/initialized` — accepted and ignored (spec's
//!   "client is ready" signal).
//!
//! Anything else returns `METHOD_NOT_FOUND` so the optional method
//! families (`resources/*`, `prompts/*`) degrade cleanly.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::protocol::{
    CallToolParams, CallToolResult, ContentItem, Implementation, InitializeParams,
    InitializeResult, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    ListToolsResult, RequestId, ServerCapabilities, ToolDescriptor, ToolsCapability, ERR_INTERNAL,
    ERR_INVALID_PARAMS, ERR_INVALID_REQUEST, ERR_METHOD_NOT_FOUND, ERR_PARSE, JSONRPC_VERSION,
};
use crate::tool::Tool;
use crate::transport::{StdioTransport, Transport, TransportError};

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),

    /// Two tools were registered under the same name. The builder
    /// returns this instead of silently dropping the prior handler —
    /// MCP clients pin tool names so the resulting "same name, different
    /// behaviour" race is impossible to debug from the client side.
    #[error("duplicate tool registration: {name}")]
    DuplicateTool { name: &'static str },
}

/// Builder + runtime for an inbound MCP server.
pub struct Server {
    name: String,
    version: String,
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl Server {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tools: HashMap::new(),
        }
    }

    /// Register one tool. Panics if a tool with the same name has
    /// already been registered — silently overwriting would make
    /// versioning races impossible to detect from either the client
    /// or the audit log. Callers that need a non-panicking variant
    /// (e.g. building the registry from user configuration) should
    /// use [`Server::try_tool`] instead.
    pub fn tool(self, tool: Arc<dyn Tool>) -> Self {
        let name = tool.name();
        self.try_tool(tool).unwrap_or_else(|_| {
            panic!("duplicate MCP tool registration at startup: {name}")
        })
    }

    /// Like [`Server::tool`] but surfaces duplicate registrations as
    /// `Err(ServerError::DuplicateTool)` instead of panicking.
    pub fn try_tool(mut self, tool: Arc<dyn Tool>) -> Result<Self, ServerError> {
        let name = tool.name();
        if self.tools.contains_key(name) {
            return Err(ServerError::DuplicateTool { name });
        }
        self.tools.insert(name, tool);
        Ok(self)
    }

    /// Register every tool from an iterator. Panics on the first
    /// duplicate (see [`Server::tool`] for rationale). Callers that
    /// want graceful error handling should fold over [`try_tool`]
    /// directly.
    pub fn tools<I>(self, iter: I) -> Self
    where
        I: IntoIterator<Item = Arc<dyn Tool>>,
    {
        let mut s = self;
        for t in iter {
            s = s.tool(t);
        }
        s
    }

    /// Drive the server over `tokio::io::stdin()` / `stdout()`.
    /// Blocks until stdin closes (or a transport error fires), which
    /// is exactly the lifecycle the kernel agent expects: spawn,
    /// handshake, run, exit when the parent kills the child.
    pub async fn serve_stdio(self) -> Result<(), ServerError> {
        self.serve(StdioTransport::stdio()).await
    }

    /// Drive the server over an arbitrary [`Transport`]. Tests use
    /// `transport::in_memory_pair` to wire a client + server pair
    /// in-process; production calls [`Server::serve_stdio`].
    pub async fn serve(self, transport: impl Transport) -> Result<(), ServerError> {
        let server = Arc::new(self);
        let t = Arc::new(transport);
        loop {
            let frame = match t.recv().await? {
                Some(f) => f,
                None => return Ok(()),
            };

            // Quick structural sniff. JSON-RPC notifications carry no
            // `id`; requests carry one. Detecting the difference up
            // front lets us:
            //   - silently drop spec-conformant notifications
            //   - reject "notification with id" frames (forbidden by
            //     spec) without engaging the request dispatcher
            //   - emit RequestId::Null when we can't even read the id
            //     out of a malformed frame
            let raw_value: Result<Value, _> = serde_json::from_str(&frame);
            let raw = match raw_value {
                Ok(v) => v,
                Err(err) => {
                    let resp = JsonRpcResponse::err(
                        RequestId::Null,
                        JsonRpcError::new(ERR_PARSE, format!("parse error: {err}")),
                    );
                    t.send(serde_json::to_string(&resp).unwrap_or_default())
                        .await?;
                    continue;
                }
            };
            if !raw.is_object() {
                let resp = JsonRpcResponse::err(
                    RequestId::Null,
                    JsonRpcError::new(ERR_INVALID_REQUEST, "request must be a JSON object"),
                );
                t.send(serde_json::to_string(&resp).unwrap_or_default())
                    .await?;
                continue;
            }
            // jsonrpc version check — spec requires exactly "2.0".
            // Otherwise the server can be tricked into accepting a
            // v1-flavoured envelope whose semantics differ from
            // what callers expect.
            match raw.get("jsonrpc").and_then(|v| v.as_str()) {
                Some(v) if v == JSONRPC_VERSION => {}
                _ => {
                    let id = extract_id(&raw);
                    let resp = JsonRpcResponse::err(
                        id,
                        JsonRpcError::new(
                            ERR_INVALID_REQUEST,
                            "missing or invalid jsonrpc 2.0 envelope",
                        ),
                    );
                    t.send(serde_json::to_string(&resp).unwrap_or_default())
                        .await?;
                    continue;
                }
            }

            // Notifications: per spec, MUST NOT carry an `id` field.
            // A "notifications/*" frame WITH an id is malformed.
            let method_starts_with_notifications = raw
                .get("method")
                .and_then(|m| m.as_str())
                .map(|s| s.starts_with("notifications/"))
                .unwrap_or(false);
            let has_id = raw.get("id").is_some();
            if method_starts_with_notifications {
                if has_id {
                    let id = extract_id(&raw);
                    let resp = JsonRpcResponse::err(
                        id,
                        JsonRpcError::new(
                            ERR_INVALID_REQUEST,
                            "notifications/* must not carry an id",
                        ),
                    );
                    t.send(serde_json::to_string(&resp).unwrap_or_default())
                        .await?;
                }
                // Drop the notification silently.
                continue;
            }

            // Conversely, a frame with no id but a non-notification
            // method is still a notification per spec — silently
            // accept the `JsonRpcNotification` shape and ignore.
            if !has_id {
                let _ = serde_json::from_value::<JsonRpcNotification>(raw);
                continue;
            }

            let parsed: Result<JsonRpcRequest, _> = serde_json::from_str(&frame);
            match parsed {
                Ok(req) => {
                    let resp = server.handle(req).await;
                    let body = serde_json::to_string(&resp).unwrap_or_else(|_| {
                        // We can always serialize a parse-error
                        // response because it has no Value fields.
                        serde_json::to_string(&JsonRpcResponse::err(
                            RequestId::Null,
                            JsonRpcError::new(ERR_INTERNAL, "encode failed"),
                        ))
                        .unwrap()
                    });
                    t.send(body).await?;
                }
                Err(err) => {
                    let id = extract_id(&raw);
                    let resp = JsonRpcResponse::err(
                        id,
                        JsonRpcError::new(ERR_INVALID_REQUEST, format!("invalid request: {err}")),
                    );
                    t.send(serde_json::to_string(&resp).unwrap_or_default())
                        .await?;
                }
            }
        }
    }

    async fn handle(self: &Arc<Self>, req: JsonRpcRequest) -> JsonRpcResponse {
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
        // We accept the params for compliance but don't currently
        // honour client capabilities (we don't emit progress
        // notifications, etc.). The spec allows servers to ignore
        // capabilities they don't implement — and to tolerate an
        // omitted `params` member, since the only required fields
        // (protocol version, client info, capabilities) all default
        // sensibly.
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
            None => InitializeParams {
                protocol_version: crate::protocol::PROTOCOL_VERSION.to_string(),
                capabilities: Default::default(),
                client_info: Implementation {
                    name: "unknown".into(),
                    version: "unknown".into(),
                },
            },
        };
        let result = InitializeResult {
            protocol_version: crate::protocol::PROTOCOL_VERSION.to_string(),
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

    fn handle_tools_list(&self, id: RequestId) -> JsonRpcResponse {
        let mut tools: Vec<ToolDescriptor> = Vec::with_capacity(self.tools.len());
        // Sort by name so list order is deterministic — easier on
        // both human readers and JSON-Schema-aware UIs that snapshot
        // the catalogue.
        let mut names: Vec<&&'static str> = self.tools.keys().collect();
        names.sort();
        for n in names {
            let t = &self.tools[*n];
            tools.push(ToolDescriptor {
                name: t.name().to_string(),
                description: Some(t.description().to_string()),
                input_schema: t.input_schema(),
            });
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
        let tool = match self.tools.get(params.name.as_str()) {
            Some(t) => t.clone(),
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

/// Pull a [`RequestId`] out of an arbitrary JSON-RPC frame value.
/// Returns [`RequestId::Null`] when no id is present or the id is of an
/// unrecognised type — exactly what the JSON-RPC 2.0 spec mandates
/// for error responses to malformed requests.
fn extract_id(raw: &Value) -> RequestId {
    match raw.get("id") {
        Some(Value::Number(n)) => n
            .as_i64()
            .map(RequestId::Num)
            .unwrap_or(RequestId::Null),
        Some(Value::String(s)) => RequestId::Str(s.clone()),
        _ => RequestId::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ClientCapabilities, ContentItem, JsonRpcRequest, RequestId};
    use crate::tool::ToolResult;
    use crate::transport::in_memory_pair;
    use async_trait::async_trait;

    struct Echo;

    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn description(&self) -> &'static str {
            "echo back"
        }
        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false,
            })
        }
        async fn exec(&self, input: Value) -> ToolResult {
            let t = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
            ToolResult::ok(t.to_string())
        }
    }

    async fn drive_initialize(t: &impl Transport) -> InitializeResult {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: RequestId::Num(1),
            method: "initialize".to_string(),
            params: Some(
                serde_json::to_value(&InitializeParams {
                    protocol_version: crate::protocol::PROTOCOL_VERSION.to_string(),
                    capabilities: ClientCapabilities::default(),
                    client_info: Implementation {
                        name: "test".into(),
                        version: "0".into(),
                    },
                })
                .unwrap(),
            ),
        };
        t.send(serde_json::to_string(&req).unwrap()).await.unwrap();
        let frame = t.recv().await.unwrap().unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
        serde_json::from_value(resp.result.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn initialize_advertises_tools_capability() {
        let (client, server) = in_memory_pair();
        let s = Server::new("my-app", "0.1.0").tool(Arc::new(Echo));
        let handle = tokio::spawn(s.serve(server));
        let init = drive_initialize(&client).await;
        assert_eq!(init.server_info.name, "my-app");
        assert_eq!(init.server_info.version, "0.1.0");
        assert!(init.capabilities.tools.is_some());
        drop(client);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn tools_list_returns_registered_tools_sorted() {
        struct A;
        struct B;
        struct C;
        macro_rules! impl_simple_tool {
            ($t:ty, $n:literal) => {
                #[async_trait]
                impl Tool for $t {
                    fn name(&self) -> &'static str {
                        $n
                    }
                    fn description(&self) -> &'static str {
                        $n
                    }
                    fn input_schema(&self) -> Value {
                        json!({"type":"object"})
                    }
                    async fn exec(&self, _: Value) -> ToolResult {
                        ToolResult::ok($n)
                    }
                }
            };
        }
        impl_simple_tool!(A, "alpha");
        impl_simple_tool!(B, "bravo");
        impl_simple_tool!(C, "charlie");
        // Register in non-sorted order.
        let s = Server::new("t", "0")
            .tool(Arc::new(C))
            .tool(Arc::new(A))
            .tool(Arc::new(B));
        let (client, server) = in_memory_pair();
        let handle = tokio::spawn(s.serve(server));
        let _ = drive_initialize(&client).await;

        let req = JsonRpcRequest::new(2, "tools/list", None);
        client
            .send(serde_json::to_string(&req).unwrap())
            .await
            .unwrap();
        let frame = client.recv().await.unwrap().unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
        let listing: ListToolsResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        let names: Vec<String> = listing.tools.iter().map(|t| t.name.clone()).collect();
        assert_eq!(names, vec!["alpha", "bravo", "charlie"]);
        drop(client);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn tools_call_routes_to_handler() {
        let s = Server::new("t", "0").tool(Arc::new(Echo));
        let (client, server) = in_memory_pair();
        let handle = tokio::spawn(s.serve(server));
        let _ = drive_initialize(&client).await;

        let req = JsonRpcRequest::new(
            2,
            "tools/call",
            Some(json!({"name":"echo","arguments":{"text":"hi"}})),
        );
        client
            .send(serde_json::to_string(&req).unwrap())
            .await
            .unwrap();
        let frame = client.recv().await.unwrap().unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
        let result: CallToolResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(!result.is_error.unwrap_or(false));
        match result.content.first() {
            Some(ContentItem::Text { text }) => assert_eq!(text, "hi"),
            _ => panic!("expected text content"),
        }
        drop(client);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn unknown_tool_yields_method_not_found() {
        let s = Server::new("t", "0").tool(Arc::new(Echo));
        let (client, server) = in_memory_pair();
        let handle = tokio::spawn(s.serve(server));
        let _ = drive_initialize(&client).await;

        let req = JsonRpcRequest::new(
            2,
            "tools/call",
            Some(json!({"name":"missing","arguments":{}})),
        );
        client
            .send(serde_json::to_string(&req).unwrap())
            .await
            .unwrap();
        let frame = client.recv().await.unwrap().unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
        let err = resp.error.unwrap();
        assert_eq!(err.code, ERR_METHOD_NOT_FOUND);
        drop(client);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn notifications_initialized_is_silently_dropped() {
        let s = Server::new("t", "0").tool(Arc::new(Echo));
        let (client, server) = in_memory_pair();
        let handle = tokio::spawn(s.serve(server));

        // Send a notification then a ping; the server must not have
        // tried to respond to the notification, so the ping response
        // arrives first / unambiguously.
        let note = JsonRpcNotification {
            jsonrpc: "2.0".into(),
            method: "notifications/initialized".into(),
            params: None,
        };
        client
            .send(serde_json::to_string(&note).unwrap())
            .await
            .unwrap();
        let ping = JsonRpcRequest::new(42, "ping", None);
        client
            .send(serde_json::to_string(&ping).unwrap())
            .await
            .unwrap();

        let frame = client.recv().await.unwrap().unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
        assert_eq!(resp.id, RequestId::Num(42));
        drop(client);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn parse_error_keeps_server_alive() {
        let s = Server::new("t", "0").tool(Arc::new(Echo));
        let (client, server) = in_memory_pair();
        let handle = tokio::spawn(s.serve(server));

        client.send("not json".into()).await.unwrap();
        let frame = client.recv().await.unwrap().unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
        assert!(resp.error.is_some());

        // Still alive: ping round-trips.
        let req = JsonRpcRequest::new(2, "ping", None);
        client
            .send(serde_json::to_string(&req).unwrap())
            .await
            .unwrap();
        let frame = client.recv().await.unwrap().unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
        assert!(resp.error.is_none());
        drop(client);
        let _ = handle.await;
    }
}
