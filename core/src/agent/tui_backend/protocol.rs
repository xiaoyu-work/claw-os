use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub(super) const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub(super) const MAX_PROMPT_BYTES: usize = 64 * 1024;
pub(super) const MAX_HISTORY_ROWS: u64 = 1000;
pub(super) const MAX_PAGE_SIZE: usize = 100;
pub(super) const MAX_LOADED_THREADS: usize = 64;
pub(super) const MAX_PENDING_APPROVALS: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub(super) enum RequestId {
    Integer(i64),
    String(String),
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Value,
}

impl RpcError {
    fn new(code: i64, kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: safe_text(&message.into()),
            data: json!({ "kind": kind }),
        }
    }

    pub fn parse() -> Self {
        Self::new(-32700, "parse_error", "Invalid JSON message")
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(-32600, "invalid_request", message)
    }

    pub fn params(message: impl Into<String>) -> Self {
        Self::new(-32602, "invalid_params", message)
    }

    pub fn unsupported(method: &str) -> Self {
        Self::new(
            -32601,
            "unsupported_method",
            format!("{method} is not implemented by the Claw backend; no action was taken"),
        )
    }

    pub fn option(field: &str) -> Self {
        Self::new(
            -32602,
            "unsupported_option",
            format!(
                "{field} is not supported by the Claw task backend; the request was not submitted"
            ),
        )
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(-32005, "backend_unavailable", message)
    }

    pub fn backend(message: impl Into<String>) -> Self {
        Self::new(-32603, "invalid_backend_response", message)
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(-32000, "execution_failed", message)
    }

    pub fn stale(message: impl Into<String>) -> Self {
        Self::new(-32003, "stale_id", message)
    }

    pub fn busy() -> Self {
        Self::new(
            -32004,
            "thread_busy",
            "The conversation already has an active task. Queue input in the TUI or interrupt it first; live steering is not supported",
        )
    }

    pub fn capacity() -> Self {
        Self::new(-32007, "capacity_exceeded", "Adapter capacity exceeded")
    }

    pub fn not_initialized() -> Self {
        Self::new(
            -32001,
            "not_initialized",
            "Send initialize and initialized first",
        )
    }
}

#[derive(Debug)]
pub(super) enum Incoming {
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: RequestId,
        result: Result<Value, Value>,
    },
}

/// Upstream intentionally omits `jsonrpc`. Also accept the standard 2.0 marker,
/// but never infer a request from an ambiguous request/response-shaped object.
pub(super) fn decode(text: &str) -> Result<Incoming, (Option<RequestId>, RpcError)> {
    let value: Value = serde_json::from_str(text).map_err(|_| (None, RpcError::parse()))?;
    let object = value.as_object().ok_or_else(|| {
        (
            None,
            RpcError::invalid_request("Expected one JSON-RPC object"),
        )
    })?;
    let id = object
        .get("id")
        .map(|value| serde_json::from_value::<RequestId>(value.clone()))
        .transpose()
        .map_err(|_| (None, RpcError::invalid_request("Invalid request id")))?;
    if matches!(&id, Some(RequestId::String(id)) if id.is_empty() || id.len() > 256) {
        return Err((None, RpcError::invalid_request("Invalid request id")));
    }
    let invalid = |message| (id.clone(), RpcError::invalid_request(message));
    if object.get("jsonrpc").is_some_and(|value| value != "2.0") {
        return Err(invalid("Unsupported JSON-RPC version"));
    }
    if let Some(method) = object.get("method") {
        let method = method
            .as_str()
            .filter(|method| {
                !method.is_empty()
                    && method.len() <= 160
                    && method.bytes().all(|byte| byte.is_ascii_graphic())
            })
            .ok_or_else(|| invalid("Invalid method name"))?;
        if object
            .keys()
            .any(|key| !["id", "method", "params", "jsonrpc", "trace"].contains(&key.as_str()))
        {
            return Err(invalid("Unexpected JSON-RPC request field"));
        }
        if let Some(trace) = object.get("trace").filter(|value| !value.is_null()) {
            let trace = trace
                .as_object()
                .ok_or_else(|| invalid("Invalid trace context"))?;
            if trace.len() > 4
                || trace.values().any(|value| {
                    !value
                        .as_str()
                        .is_some_and(|value| value.len() <= 512 && !value.contains('\0'))
                })
            {
                return Err(invalid("Invalid trace context"));
            }
        }
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        return Ok(match id {
            Some(id) => Incoming::Request {
                id,
                method: method.to_string(),
                params,
            },
            None => Incoming::Notification {
                method: method.to_string(),
                params,
            },
        });
    }
    if object
        .keys()
        .any(|key| !["id", "result", "error", "jsonrpc"].contains(&key.as_str()))
        || object.contains_key("result") == object.contains_key("error")
    {
        return Err(invalid(
            "Expected a request, notification, or correlated response",
        ));
    }
    let id = id.ok_or_else(|| (None, RpcError::invalid_request("Response has no id")))?;
    Ok(Incoming::Response {
        id,
        result: match object.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(object["result"].clone()),
        },
    })
}

pub(super) fn response(id: &RequestId, result: Value) -> Value {
    json!({ "id": id, "result": result })
}

pub(super) fn error_response(id: Option<&RequestId>, error: RpcError) -> Value {
    json!({ "id": id, "error": error })
}

pub(super) fn notification(method: &str, params: Value) -> Value {
    json!({ "method": method, "params": params })
}

pub(super) fn safe_text(text: &str) -> String {
    crate::agent::safety::redact::Redactor::default_set().redact(text)
}

pub(super) fn token(value: &str, field: &str) -> Result<(), RpcError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
    {
        return Err(RpcError::params(format!("Invalid {field}")));
    }
    Ok(())
}

pub(super) fn canonical_session_id(value: &str) -> Option<crate::session::SessionId> {
    // The existing parser uses byte offsets after checking length/prefix.
    if !value.is_ascii() {
        return None;
    }
    value.parse().ok()
}

pub(super) struct Params(Map<String, Value>);

impl Params {
    pub fn new(value: Value, allowed: &[&str]) -> Result<Self, RpcError> {
        let object = match value {
            Value::Null => Map::new(),
            Value::Object(object) => object,
            _ => return Err(RpcError::params("params must be an object")),
        };
        if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
            return Err(RpcError::params(format!("Unknown parameter: {key}")));
        }
        Ok(Self(object))
    }

    pub fn value(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|value| !value.is_null())
    }

    pub fn required_string(&self, key: &str) -> Result<&str, RpcError> {
        self.string(key)?
            .filter(|value| !value.is_empty())
            .ok_or_else(|| RpcError::params(format!("{key} is required")))
    }

    pub fn string(&self, key: &str) -> Result<Option<&str>, RpcError> {
        self.value(key)
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| RpcError::params(format!("{key} must be a string")))
            })
            .transpose()
    }

    pub fn boolean(&self, key: &str, default: bool) -> Result<bool, RpcError> {
        self.value(key)
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| RpcError::params(format!("{key} must be a boolean")))
            })
            .transpose()
            .map(|value| value.unwrap_or(default))
    }

    pub fn array(&self, key: &str) -> Result<Option<&Vec<Value>>, RpcError> {
        self.value(key)
            .map(|value| {
                value
                    .as_array()
                    .ok_or_else(|| RpcError::params(format!("{key} must be an array")))
            })
            .transpose()
    }

    pub fn limit(&self, default: usize) -> Result<usize, RpcError> {
        match self.value("limit") {
            None => Ok(default),
            Some(value) => {
                let limit = value
                    .as_u64()
                    .filter(|limit| (1..=MAX_PAGE_SIZE as u64).contains(limit))
                    .ok_or_else(|| RpcError::params("limit must be in 1..=100"))?;
                Ok(limit as usize)
            }
        }
    }

    pub fn reject_present(&self, fields: &[&str]) -> Result<(), RpcError> {
        if let Some(field) = fields.iter().find(|field| self.value(field).is_some()) {
            return Err(RpcError::option(field));
        }
        Ok(())
    }

    pub fn reject_nonempty_array(&self, key: &str) -> Result<(), RpcError> {
        if self.array(key)?.is_some_and(|values| !values.is_empty()) {
            return Err(RpcError::option(key));
        }
        Ok(())
    }

    pub fn unique_strings(&self, key: &str, max: usize) -> Result<HashSet<String>, RpcError> {
        let mut result = HashSet::new();
        for item in self.array(key)?.into_iter().flatten() {
            let item = item
                .as_str()
                .filter(|item| item.len() <= 160)
                .ok_or_else(|| RpcError::params(format!("Invalid {key} entry")))?;
            result.insert(item.to_string());
            if result.len() > max {
                return Err(RpcError::params(format!("Too many {key} entries")));
            }
        }
        Ok(result)
    }
}

pub(super) fn backend_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RpcError::backend(format!("Claw response is missing {key}")))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/protocol.rs"
    ));
}
