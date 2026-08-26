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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/server.rs"
    ));
}
