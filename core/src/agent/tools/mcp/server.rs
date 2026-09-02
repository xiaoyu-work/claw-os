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

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;
use serde_json::{json, Value};
use tokio::task::{AbortHandle, JoinSet};

use super::protocol::{
    CallToolParams, CallToolResult, ContentItem, Implementation, InitializeParams,
    InitializeResult, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    ListToolsResult, RequestId, ServerCapabilities, ToolDescriptor, ToolsCapability, ERR_INTERNAL,
    ERR_INVALID_PARAMS, ERR_INVALID_REQUEST, ERR_METHOD_NOT_FOUND, ERR_PARSE, JSONRPC_VERSION,
    PROTOCOL_VERSION,
};
use super::transport::{Transport, TransportError};
use crate::agent::tools::exposure::ToolExposureContext;
use crate::agent::tools::registry::{ResolvedToolCall, ResolvedToolKind, ToolRegistry};
use crate::agent::tools::ToolResult;

/// Core-specific JSON-RPC server error used after a valid, authorized
/// `tools/call` cannot enter the bounded execution queue.
pub const ERR_SERVER_OVERLOADED: i64 = -32000;
pub const SERVER_OVERLOADED_KIND: &str = "server_overloaded";
pub const SERVER_OVERLOADED_HINT: &str = "retry the request with exponential backoff";

pub const DEFAULT_MAX_ACTIVE_TOOL_CALLS: usize = 4;
pub const DEFAULT_MAX_QUEUED_TOOL_CALLS: usize = 8;
pub const MAX_ACTIVE_TOOL_CALLS: usize = 16;
pub const MAX_QUEUED_TOOL_CALLS: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ServerLimitsError {
    #[error("active tool-call limit must be between 1 and {MAX_ACTIVE_TOOL_CALLS}")]
    Active,
    #[error("queued tool-call limit must not exceed {MAX_QUEUED_TOOL_CALLS}")]
    Queued,
}

/// Trusted composition-time limits for inbound tool execution.
///
/// Request fields never influence these values. The maxima keep even
/// operator-selected limits within a predictable retention envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpServerLimits {
    max_active_tool_calls: usize,
    max_queued_tool_calls: usize,
}

impl McpServerLimits {
    pub fn new(
        max_active_tool_calls: usize,
        max_queued_tool_calls: usize,
    ) -> Result<Self, ServerLimitsError> {
        if !(1..=MAX_ACTIVE_TOOL_CALLS).contains(&max_active_tool_calls) {
            return Err(ServerLimitsError::Active);
        }
        if max_queued_tool_calls > MAX_QUEUED_TOOL_CALLS {
            return Err(ServerLimitsError::Queued);
        }
        Ok(Self {
            max_active_tool_calls,
            max_queued_tool_calls,
        })
    }
}

impl Default for McpServerLimits {
    fn default() -> Self {
        Self {
            max_active_tool_calls: DEFAULT_MAX_ACTIVE_TOOL_CALLS,
            max_queued_tool_calls: DEFAULT_MAX_QUEUED_TOOL_CALLS,
        }
    }
}

pub struct McpServer {
    name: String,
    version: String,
    registry: Arc<ToolRegistry>,
    exposure: ToolExposureContext,
    limits: McpServerLimits,
}

struct PreparedToolCall {
    id: RequestId,
    resolved: ResolvedToolCall,
}

impl McpServer {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        registry: Arc<ToolRegistry>,
    ) -> Self {
        Self::new_with_context(
            name,
            version,
            registry,
            ToolExposureContext::isolated(crate::agent::tools::guardrails::Guardrails::permissive()),
        )
    }

    pub fn new_with_context(
        name: impl Into<String>,
        version: impl Into<String>,
        registry: Arc<ToolRegistry>,
        exposure: ToolExposureContext,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            registry,
            exposure,
            limits: McpServerLimits::default(),
        }
    }

    /// Override execution bounds from trusted server composition.
    pub fn with_limits(mut self, limits: McpServerLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Run the read/dispatch/write loop until the transport closes
    /// or errors.
    ///
    /// Valid, authorized `tools/call` requests enter a bounded active
    /// set and bounded FIFO queue. Protocol-control methods stay on
    /// the read loop, so tool load cannot starve them. Excess calls
    /// receive [`ERR_SERVER_OVERLOADED`], while notifications are
    /// never queued and remain response-free.
    ///
    /// MCP does not require strict request/response ordering (each
    /// response carries the request id), so completion-order responses
    /// are safe and within spec.
    pub async fn serve(self, transport: impl Transport) -> Result<(), ServerError> {
        let t = Arc::new(transport);
        let me = Arc::new(self);
        let mut handlers = JoinSet::new();
        let mut active_requests: Vec<(RequestId, AbortHandle)> = Vec::new();
        let mut queued = VecDeque::new();

        let result = async {
            loop {
                while handlers.len() < me.limits.max_active_tool_calls {
                    let Some(call) = queued.pop_front() else {
                        break;
                    };
                    spawn_tool_call(&mut handlers, &mut active_requests, Arc::clone(&me), call);
                }

                let frame = tokio::select! {
                    biased;
                    completed = handlers.join_next(), if !handlers.is_empty() => {
                        active_requests.retain(|(_, handle)| !handle.is_finished());
                        match completed {
                            Some(Ok(response)) => t.send(encode_response(&response)).await?,
                            Some(Err(error)) if error.is_cancelled() => {}
                            Some(Err(error)) => {
                                tracing::warn!("MCP tool-call handler failed: {error}");
                            }
                            None => {}
                        }
                        continue;
                    }
                    received = t.recv() => {
                        match received {
                            Ok(Some(frame)) => frame,
                            Ok(None) => break Ok(()),
                            Err(error) => break Err(error.into()),
                        }
                    }
                };

                let raw: Value = match serde_json::from_str(&frame) {
                    Ok(value) => value,
                    Err(error) => {
                        let response = JsonRpcResponse::err(
                            RequestId::Null,
                            JsonRpcError::new(ERR_PARSE, format!("parse error: {error}")),
                        );
                        t.send(encode_response(&response)).await?;
                        continue;
                    }
                };
                if !raw.is_object() {
                    let response = JsonRpcResponse::err(
                        RequestId::Null,
                        JsonRpcError::new(ERR_INVALID_REQUEST, "request must be a JSON object"),
                    );
                    t.send(encode_response(&response)).await?;
                    continue;
                }
                if raw.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
                    let response = JsonRpcResponse::err(
                        extract_id(&raw),
                        JsonRpcError::new(
                            ERR_INVALID_REQUEST,
                            "missing or invalid jsonrpc 2.0 envelope",
                        ),
                    );
                    t.send(encode_response(&response)).await?;
                    continue;
                }
                if raw.get("id").is_some()
                    && !matches!(
                        raw.get("id"),
                        Some(Value::Null | Value::String(_) | Value::Number(_))
                    )
                {
                    let response = JsonRpcResponse::err(
                        RequestId::Null,
                        JsonRpcError::new(
                            ERR_INVALID_REQUEST,
                            "request id must be a string, number, or null",
                        ),
                    );
                    t.send(encode_response(&response)).await?;
                    continue;
                }
                if raw
                    .get("params")
                    .is_some_and(|params| !params.is_object() && !params.is_array())
                    && raw.get("id").is_none()
                {
                    let response = JsonRpcResponse::err(
                        extract_id(&raw),
                        JsonRpcError::new(
                            ERR_INVALID_REQUEST,
                            "request params must be an object or array",
                        ),
                    );
                    t.send(encode_response(&response)).await?;
                    continue;
                }
                if raw.get("id").is_some()
                    && raw.get("params").is_some_and(Value::is_null)
                    && matches!(
                        raw.get("method").and_then(Value::as_str),
                        Some("ping" | "tools/list")
                    )
                {
                    let response = JsonRpcResponse::err(
                        extract_id(&raw),
                        JsonRpcError::new(ERR_INVALID_PARAMS, "params must not be null"),
                    );
                    t.send(encode_response(&response)).await?;
                    continue;
                }
                if raw.get("id").is_none() {
                    match serde_json::from_value::<JsonRpcNotification>(raw) {
                        Ok(notification) => {
                            handle_notification(notification, &mut queued, &active_requests);
                            continue;
                        }
                        Err(error) => {
                            let response = JsonRpcResponse::err(
                                RequestId::Null,
                                JsonRpcError::new(
                                    ERR_INVALID_REQUEST,
                                    format!("invalid notification: {error}"),
                                ),
                            );
                            t.send(encode_response(&response)).await?;
                            continue;
                        }
                    }
                }
                let request: JsonRpcRequest = match serde_json::from_value(raw.clone()) {
                    Ok(request) => request,
                    Err(error) => {
                        let response = JsonRpcResponse::err(
                            extract_id(&raw),
                            JsonRpcError::new(
                                ERR_INVALID_REQUEST,
                                format!("invalid request: {error}"),
                            ),
                        );
                        t.send(encode_response(&response)).await?;
                        continue;
                    }
                };

                if request.method == "tools/call" {
                    let call = match me.prepare_tools_call(request) {
                        Ok(call) => call,
                        Err(response) => {
                            t.send(encode_response(&response)).await?;
                            continue;
                        }
                    };
                    if handlers.len() < me.limits.max_active_tool_calls {
                        spawn_tool_call(&mut handlers, &mut active_requests, Arc::clone(&me), call);
                    } else if queued.len() < me.limits.max_queued_tool_calls {
                        queued.push_back(call);
                    } else {
                        t.send(encode_response(&overload_response(call.id))).await?;
                    }
                    continue;
                }

                let response = me.handle_control(request);
                t.send(encode_response(&response)).await?;
            }
        }
        .await;

        queued.clear();
        shutdown_handlers(&mut handlers).await;
        result
    }

    fn handle_control(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        match req.method.as_str() {
            "initialize" => self.handle_initialize(req),
            "ping" => self.handle_ping(req),
            "tools/list" => self.handle_tools_list(req),
            other => JsonRpcResponse::err(
                id,
                JsonRpcError::new(ERR_METHOD_NOT_FOUND, format!("method not found: {other}")),
            ),
        }
    }

    fn handle_initialize(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        if let Some(params) = req.params.as_ref() {
            if let Err(error) = validate_initialize_params(params) {
                return JsonRpcResponse::err(id, JsonRpcError::new(ERR_INVALID_PARAMS, error));
            }
        }
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

    fn handle_ping(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id;
        match req.params {
            None | Some(Value::Object(_)) => JsonRpcResponse::ok(id, json!({})),
            Some(_) => JsonRpcResponse::err(
                id,
                JsonRpcError::new(ERR_INVALID_PARAMS, "ping params must be an object"),
            ),
        }
    }

    fn handle_tools_list(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id;
        if let Some(params) = req.params {
            let Some(params) = params.as_object() else {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, "tools/list params must be an object"),
                );
            };
            if params
                .get("cursor")
                .is_some_and(|cursor| !cursor.is_string())
            {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, "tools/list cursor must be a string"),
                );
            }
        }
        let tools = self
            .registry
            .as_llm_tools_for(&self.exposure)
            .into_iter()
            .map(|tool| ToolDescriptor {
                name: tool.name,
                description: Some(tool.description),
                input_schema: tool.input_schema,
            })
            .collect();
        let result = ListToolsResult {
            tools,
            next_cursor: None,
        };
        match serde_json::to_value(&result) {
            Ok(v) => JsonRpcResponse::ok(id, v),
            Err(e) => JsonRpcResponse::err(id, JsonRpcError::new(ERR_INTERNAL, e.to_string())),
        }
    }

    fn prepare_tools_call(
        &self,
        req: JsonRpcRequest,
    ) -> Result<PreparedToolCall, Box<JsonRpcResponse>> {
        let id = req.id.clone();
        let arguments_present = req
            .params
            .as_ref()
            .and_then(Value::as_object)
            .is_some_and(|params| params.contains_key("arguments"));
        let params: CallToolParams = match req.params {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => {
                    return Err(Box::new(JsonRpcResponse::err(
                        id,
                        JsonRpcError::new(ERR_INVALID_PARAMS, e.to_string()),
                    )));
                }
            },
            None => {
                return Err(Box::new(JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, "missing params"),
                )));
            }
        };
        let arguments = match params.arguments {
            Some(Value::Object(arguments)) => Value::Object(arguments),
            Some(_) => {
                return Err(Box::new(JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, "`arguments` must be an object"),
                )));
            }
            None if !arguments_present => json!({}),
            None => {
                return Err(Box::new(JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, "`arguments` must be an object"),
                )));
            }
        };
        let resolved = self.registry.resolve_model_call(
            &self.exposure,
            &crate::agent::llm::ToolCall {
                id: String::new(),
                name: params.name,
                input: arguments,
            },
        );
        if let ResolvedToolKind::Rejected(reason) = &resolved.kind {
            return Err(Box::new(JsonRpcResponse::err(
                id,
                JsonRpcError::new(ERR_INVALID_PARAMS, reason.clone()),
            )));
        }
        if matches!(resolved.kind, ResolvedToolKind::Registry)
            && self
                .registry
                .get_for(&self.exposure, &resolved.call.name)
                .is_none()
        {
            return Err(Box::new(JsonRpcResponse::err(
                id,
                JsonRpcError::new(
                    ERR_INVALID_PARAMS,
                    format!("tool not registered: {}", resolved.call.name),
                ),
            )));
        }
        Ok(PreparedToolCall { id, resolved })
    }

    async fn execute_tools_call(&self, call: PreparedToolCall) -> JsonRpcResponse {
        let result = match call.resolved.kind {
            ResolvedToolKind::Catalog => self.registry.execute_catalog(
                &self.exposure,
                &call.resolved.call.name,
                &call.resolved.call.input,
            ),
            ResolvedToolKind::Registry => {
                self.registry
                    .execute(
                        &self.exposure,
                        &call.resolved.call.name,
                        call.resolved.call.input,
                        "policy: external_mcp",
                    )
                    .await
            }
            ResolvedToolKind::Rejected(reason) => ToolResult::err(reason),
        };
        let body = CallToolResult {
            content: vec![ContentItem::Text {
                text: result.content,
            }],
            is_error: if result.is_error { Some(true) } else { None },
        };
        match serde_json::to_value(&body) {
            Ok(v) => JsonRpcResponse::ok(call.id, v),
            Err(e) => JsonRpcResponse::err(call.id, JsonRpcError::new(ERR_INTERNAL, e.to_string())),
        }
    }
}

fn spawn_tool_call(
    handlers: &mut JoinSet<JsonRpcResponse>,
    active_requests: &mut Vec<(RequestId, AbortHandle)>,
    server: Arc<McpServer>,
    call: PreparedToolCall,
) {
    let request_id = call.id.clone();
    let panic_id = request_id.clone();
    let handle = handlers.spawn(async move {
        match AssertUnwindSafe(server.execute_tools_call(call))
            .catch_unwind()
            .await
        {
            Ok(response) => response,
            Err(_) => JsonRpcResponse::err(
                panic_id,
                JsonRpcError::new(ERR_INTERNAL, "tool-call handler panicked"),
            ),
        }
    });
    active_requests.push((request_id, handle));
}

async fn shutdown_handlers(handlers: &mut JoinSet<JsonRpcResponse>) {
    handlers.abort_all();
    while handlers.join_next().await.is_some() {}
}

fn handle_notification(
    notification: JsonRpcNotification,
    queued: &mut VecDeque<PreparedToolCall>,
    active_requests: &[(RequestId, AbortHandle)],
) {
    if notification.method != "notifications/cancelled" {
        return;
    }
    let Some(request_id) = notification
        .params
        .and_then(|params| params.as_object().cloned())
        .and_then(|params| params.get("requestId").cloned())
        .and_then(|request_id| serde_json::from_value::<RequestId>(request_id).ok())
    else {
        return;
    };

    queued.retain(|call| call.id != request_id);
    for (id, handle) in active_requests {
        if *id == request_id {
            handle.abort();
        }
    }
}

fn overload_response(id: RequestId) -> JsonRpcResponse {
    JsonRpcResponse::err(
        id,
        JsonRpcError::new(ERR_SERVER_OVERLOADED, "server overloaded").with_data(json!({
            "kind": SERVER_OVERLOADED_KIND,
            "retryable": true,
            "hint": SERVER_OVERLOADED_HINT,
        })),
    )
}

fn validate_initialize_params(params: &Value) -> Result<(), String> {
    let capabilities = params
        .get("capabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| "initialize capabilities must be an object".to_string())?;
    for name in ["experimental", "sampling", "elicitation"] {
        if capabilities
            .get(name)
            .is_some_and(|capability| !capability.is_object())
        {
            return Err(format!("capabilities.{name} must be an object"));
        }
    }
    if let Some(roots) = capabilities.get("roots") {
        let roots = roots
            .as_object()
            .ok_or_else(|| "capabilities.roots must be an object".to_string())?;
        if roots
            .get("listChanged")
            .is_some_and(|list_changed| !list_changed.is_boolean())
        {
            return Err("capabilities.roots.listChanged must be a boolean".to_string());
        }
    }
    Ok(())
}

fn encode_response(response: &JsonRpcResponse) -> String {
    serde_json::to_string(response).unwrap_or_else(|_| {
        serde_json::to_string(&JsonRpcResponse::err(
            RequestId::Null,
            JsonRpcError::new(ERR_INTERNAL, "encode failed"),
        ))
        .unwrap_or_default()
    })
}

fn extract_id(raw: &Value) -> RequestId {
    match raw.get("id") {
        Some(Value::Number(number)) => RequestId::Num(number.clone()),
        Some(Value::String(value)) => RequestId::Str(value.clone()),
        _ => RequestId::Null,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/mcp/server.rs"
    ));
}
