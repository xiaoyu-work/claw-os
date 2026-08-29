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
    InitializeResult, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    ListToolsResult, RequestId, ServerCapabilities, ToolDescriptor, ToolsCapability,
    ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_INVALID_REQUEST, ERR_METHOD_NOT_FOUND,
    ERR_PARSE, JSONRPC_VERSION, PROTOCOL_VERSION,
};
use super::transport::{Transport, TransportError};
use crate::agent::tools::exposure::ToolExposureContext;
use crate::agent::tools::registry::{ResolvedToolKind, ToolRegistry};

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
}

pub struct McpServer {
    name: String,
    version: String,
    registry: Arc<ToolRegistry>,
    exposure: ToolExposureContext,
}

impl McpServer {
    #[cfg(test)]
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        registry: Arc<ToolRegistry>,
    ) -> Self {
        Self::new_with_context(
            name,
            version,
            registry,
            ToolExposureContext::isolated(
                crate::agent::tools::guardrails::Guardrails::permissive(),
            ),
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
                    Ok(_) => continue,
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
            let server = me.clone();
            let t = t.clone();
            handlers.push(tokio::spawn(async move {
                let response = server.handle(request).await;
                let _ = t.send(encode_response(&response)).await;
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
            "ping" => self.handle_ping(req),
            "tools/list" => self.handle_tools_list(req),
            "tools/call" => self.handle_tools_call(req).await,
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
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, error),
                );
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
        let tools: Vec<ToolDescriptor> = self
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

    async fn handle_tools_call(&self, req: JsonRpcRequest) -> JsonRpcResponse {
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
        let arguments = match params.arguments {
            Some(Value::Object(arguments)) => Value::Object(arguments),
            Some(_) => {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, "`arguments` must be an object"),
                );
            }
            None if !arguments_present => json!({}),
            None => {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, "`arguments` must be an object"),
                );
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
        let result = match &resolved.kind {
            ResolvedToolKind::Rejected(reason) => {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(ERR_INVALID_PARAMS, reason.clone()),
                )
            }
            ResolvedToolKind::Catalog => self.registry.execute_catalog(
                &self.exposure,
                &resolved.call.name,
                &resolved.call.input,
            ),
            ResolvedToolKind::Registry => {
                if self
                    .registry
                    .get_for(&self.exposure, &resolved.call.name)
                    .is_none()
                {
                    return JsonRpcResponse::err(
                        id,
                        JsonRpcError::new(
                            ERR_INVALID_PARAMS,
                            format!("tool not registered: {}", resolved.call.name),
                        ),
                    );
                }
                self.registry
                    .execute(
                        &self.exposure,
                        &resolved.call.name,
                        resolved.call.input.clone(),
                        "policy: external_mcp",
                    )
                    .await
            }
        };
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
