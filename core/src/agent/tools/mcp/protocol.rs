//! JSON-RPC 2.0 envelope + MCP method types.
//!
//! Wire format reference: <https://spec.modelcontextprotocol.io/>.
//! We model the subset of methods the kernel actually uses today
//! (initialize, tools/list, tools/call, ping). Adding new methods
//! is purely additive — new variants on [`McpRequest`] /
//! [`McpResponse`] dispatch.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC version literal — every envelope must carry exactly this.
pub const JSONRPC_VERSION: &str = "2.0";

/// Protocol version reported in the `initialize` handshake. Bumped
/// when MCP releases a new dated version we have validated.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Either a numeric or string request id. JSON-RPC 2.0 allows both;
/// notifications omit the field entirely (handled at the envelope
/// level via `Option<RequestId>`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Num(i64),
    Str(String),
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

pub const ERR_PARSE: i64 = -32700;
pub const ERR_INVALID_REQUEST: i64 = -32600;
pub const ERR_METHOD_NOT_FOUND: i64 = -32601;
pub const ERR_INVALID_PARAMS: i64 = -32602;
pub const ERR_INTERNAL: i64 = -32603;

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
/// model `text` and `image` for now; richer kinds can be added
/// without breaking callers.
///
/// Field naming: MCP wire format is camelCase for object fields
/// (see e.g. `inputSchema`, `protocolVersion`). The variant-level
/// `rename_all = "snake_case"` here only renames the discriminator
/// values (`text` / `image`); the `Image.mime_type` field gets an
/// explicit per-field `rename` to `mimeType` to stay spec-compliant.
/// A spec server emits `{"type":"image","mimeType":"image/png",
/// "data":"..."}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_with_jsonrpc_field() {
        let r = JsonRpcRequest::new(1, "ping", None);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["method"], "ping");
        assert!(v.get("params").is_none(), "params None must be skipped");
    }

    #[test]
    fn notification_has_no_id_field() {
        let n = JsonRpcNotification::new("notifications/cancelled", None);
        let v = serde_json::to_value(&n).unwrap();
        assert!(v.get("id").is_none());
        assert_eq!(v["method"], "notifications/cancelled");
    }

    #[test]
    fn response_ok_omits_error() {
        let r = JsonRpcResponse::ok(RequestId::Num(7), serde_json::json!({"ok": true}));
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("error").is_none());
        assert_eq!(v["result"]["ok"], true);
    }

    #[test]
    fn response_err_omits_result() {
        let r = JsonRpcResponse::err(
            RequestId::Str("a".into()),
            JsonRpcError::new(ERR_METHOD_NOT_FOUND, "no such method"),
        );
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("result").is_none());
        assert_eq!(v["error"]["code"], ERR_METHOD_NOT_FOUND);
        assert_eq!(v["error"]["message"], "no such method");
    }

    #[test]
    fn request_id_round_trips_both_kinds() {
        for input in [
            "{\"jsonrpc\":\"2.0\",\"id\":42,\"method\":\"x\"}",
            "{\"jsonrpc\":\"2.0\",\"id\":\"abc\",\"method\":\"x\"}",
        ] {
            let r: JsonRpcRequest = serde_json::from_str(input).unwrap();
            let again = serde_json::to_string(&r).unwrap();
            // Parse again to confirm round-trip is stable.
            let _: JsonRpcRequest = serde_json::from_str(&again).unwrap();
        }
    }

    #[test]
    fn initialize_result_has_required_fields() {
        let body = serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": "cos", "version": "0.1.0"}
        });
        let init: InitializeResult = serde_json::from_value(body).unwrap();
        assert_eq!(init.protocol_version, "2025-06-18");
        assert_eq!(init.server_info.name, "cos");
        assert!(init.capabilities.tools.is_some());
    }

    #[test]
    fn call_tool_result_is_error_round_trip() {
        let r = CallToolResult {
            content: vec![ContentItem::Text {
                text: "boom".into(),
            }],
            is_error: Some(true),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"isError\":true"));
        let back: CallToolResult = serde_json::from_str(&s).unwrap();
        assert_eq!(back.is_error, Some(true));
    }

    #[test]
    fn tool_descriptor_serializes_input_schema_camel_case() {
        let t = ToolDescriptor {
            name: "echo".into(),
            description: Some("echo".into()),
            input_schema: serde_json::json!({"type":"object"}),
        };
        let s = serde_json::to_string(&t).unwrap();
        assert!(s.contains("\"inputSchema\""));
    }

    /// MCP spec mandates `mimeType` (camelCase) for image content items.
    /// Regression test for the `mime_type` → `mimeType` rename — a
    /// spec-compliant server returning image content must round-trip
    /// through us without a Decode error.
    #[test]
    fn image_content_item_uses_camel_case_mime_type_on_the_wire() {
        let item = ContentItem::Image {
            data: "QUJD".into(),
            mime_type: "image/png".into(),
        };
        let s = serde_json::to_string(&item).unwrap();
        assert!(
            s.contains("\"mimeType\""),
            "wire payload must use camelCase, got {s}"
        );
        assert!(
            !s.contains("\"mime_type\""),
            "snake_case mime_type leaked into wire payload: {s}"
        );

        // And it must accept spec-compliant payloads on the way back in.
        let parsed: ContentItem =
            serde_json::from_str(r#"{"type":"image","mimeType":"image/jpeg","data":"AAA"}"#)
                .expect("must parse spec-compliant mimeType");
        match parsed {
            ContentItem::Image { mime_type, data } => {
                assert_eq!(mime_type, "image/jpeg");
                assert_eq!(data, "AAA");
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }
}
