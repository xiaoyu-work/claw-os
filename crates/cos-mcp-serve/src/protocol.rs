//! JSON-RPC 2.0 envelope + MCP method types.
//!
//! Wire format reference: <https://spec.modelcontextprotocol.io/>.
//! We model the subset of methods the kernel actually uses today
//! (initialize, tools/list, tools/call, ping). Adding new methods
//! is purely additive — new variants on [`McpRequest`] /
//! [`McpResponse`] dispatch.
//!
//! **Note on duplication:** these types are also defined in
//! `core/src/agent/tools/mcp/protocol.rs`. The two definitions must
//! stay byte-compatible on the wire — the kernel acts as the client
//! when calling into Apps that use this crate. Kept in sync by hand
//! until a dedicated `cos-mcp-types` crate is extracted.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC version literal — every envelope must carry exactly this.
pub const JSONRPC_VERSION: &str = "2.0";

/// Protocol version reported in the `initialize` handshake. Bumped
/// when MCP releases a new dated version we have validated.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Either a numeric, string, or null request id. JSON-RPC 2.0 allows
/// all three; notifications omit the field entirely (handled at the
/// envelope level via `Option<RequestId>`). Null is reserved by the
/// spec for responses whose request id could not be determined — e.g.
/// the request didn't parse as JSON or arrived without an id — so we
/// MUST be able to emit it on error frames without collapsing it onto
/// the request namespace (Num(0) silently aliases legitimate id=0).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Num(i64),
    Str(String),
    Null,
}

impl From<i64> for RequestId {
    fn from(n: i64) -> Self {
        RequestId::Num(n)
    }
}

impl From<&str> for RequestId {
    fn from(s: &str) -> Self {
        RequestId::Str(s.to_string())
    }
}

impl From<String> for RequestId {
    fn from(s: String) -> Self {
        RequestId::Str(s)
    }
}

/// Outbound JSON-RPC request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

/// Notification envelope — no `id` field; receiver must not respond.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// Inbound response envelope — exactly one of `result` / `error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn ok(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: RequestId, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

// --- Standard JSON-RPC error codes ---------------------------------

pub use crate::generated::{
    ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_INVALID_REQUEST, ERR_METHOD_NOT_FOUND, ERR_PARSE,
};

// --- MCP-specific payloads -----------------------------------------

/// `initialize` request params (client → server).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: Implementation,
}

/// `initialize` result (server → client).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: Implementation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// Identity descriptor used by both peers in `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    pub version: String,
}

/// What the client supports / wants to receive. Intentionally
/// permissive — most fields are bool flags or empty objects in the
/// spec.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RootsCapability {
    #[serde(
        default,
        rename = "listChanged",
        skip_serializing_if = "Option::is_none"
    )]
    pub list_changed: Option<bool>,
}

/// What the server can serve.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(
        default,
        rename = "listChanged",
        skip_serializing_if = "Option::is_none"
    )]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourcesCapability {
    #[serde(
        default,
        rename = "listChanged",
        skip_serializing_if = "Option::is_none"
    )]
    pub list_changed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptsCapability {
    #[serde(
        default,
        rename = "listChanged",
        skip_serializing_if = "Option::is_none"
    )]
    pub list_changed: Option<bool>,
}

// --- tools/list ---------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<ToolDescriptor>,
    #[serde(
        default,
        rename = "nextCursor",
        skip_serializing_if = "Option::is_none"
    )]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema (object) describing this tool's input shape.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

// --- tools/call ---------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolResult {
    pub content: Vec<ContentItem>,
    #[serde(default, rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Content payload — MCP allows `text` / `image` / `resource`. We
/// model just `text` for now; richer kinds can be added without
/// breaking callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    Text {
        text: String,
    },
    Image {
        data: String,
        // MCP spec uses camelCase `mimeType`. The struct-level
        // `rename_all = "snake_case"` only renames the variant
        // discriminator, not the inner fields, so we have to apply
        // the rename explicitly. Without this the field was emitted
        // as `mime_type` and Claude Desktop / Cursor silently
        // dropped images attached to tool results.
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/protocol.rs"
    ));
}
