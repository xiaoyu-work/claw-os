//! Catalog-tool helper — apps that want to fulfil a model-proposed
//! tool call (returned in [`crate::ai::AiResponse::tool_calls`]) shell
//! out through this module to `cos ai tool <name> --app <id> --args <json>`.
//! Mirrors the Python [`claw_os_sdk.tools`] module.
//!
//! The kernel runs the catalog implementation under the app's own
//! capabilities, audits the call, and returns a structured result.

use std::ffi::OsString;

use serde::{Deserialize, Serialize};

use crate::{cos_call_json, BridgeError};

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool: {0}")]
    InvalidArg(String),

    #[error("tool denied ({name}): {message}")]
    Denied {
        name: String,
        message: String,
        code: Option<String>,
        payload: serde_json::Value,
    },

    #[error("tool unavailable: {0}")]
    Unavailable(String),

    #[error(transparent)]
    Bridge(#[from] BridgeError),
}

/// Parsed reply from `cos ai tool`. See `wire/v1/tool.schema.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    #[serde(default)]
    pub tool: String,
    /// Tool's domain-specific output — shape depends on the tool.
    #[serde(default)]
    pub result: serde_json::Value,
    #[serde(default)]
    pub audit_id: Option<String>,
}

/// Execute a catalog tool with the given JSON args. The kernel
/// resolves `name` against the catalog, runs the implementation under
/// the app's caps grant, and returns the result.
pub fn call(name: &str, args: &serde_json::Value) -> Result<ToolResult, ToolError> {
    if name.trim().is_empty() {
        return Err(ToolError::InvalidArg("call: name must be non-empty".into()));
    }
    let app = std::env::var("COS_APP_ID").map_err(|_| {
        ToolError::InvalidArg(
            "call: COS_APP_ID is required when invoking a catalog tool".into(),
        )
    })?;
    let args_json = serde_json::to_string(args).map_err(|e| {
        ToolError::InvalidArg(format!("call: args is not serialisable JSON ({e})"))
    })?;
    let argv: Vec<OsString> = vec![
        "ai".into(), "tool".into(), name.into(),
        "--app".into(), app.into(),
        "--args".into(), args_json.into(),
    ];

    let value = cos_call_json("tool", name, argv)?;
    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
        return Err(ToolError::Denied {
            name: name.to_string(),
            message: err.to_string(),
            code: value
                .get("code")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            payload: value,
        });
    }
    serde_json::from_value(value.clone()).map_err(|e| {
        ToolError::Unavailable(format!("tool result decode failed ({e}): {value}"))
    })
}

/// List the catalog tool names this app is allowed to propose / call.
/// Wraps `cos ai tools list --app <id>`.
pub fn catalog() -> Result<Vec<String>, ToolError> {
    let app = std::env::var("COS_APP_ID").map_err(|_| {
        ToolError::InvalidArg(
            "catalog: COS_APP_ID is required when listing catalog tools".into(),
        )
    })?;
    let argv: Vec<OsString> = vec![
        "ai".into(), "tools".into(), "list".into(),
        "--app".into(), app.into(),
    ];
    let value = cos_call_json("tool", "list", argv)?;
    // Two possible shapes: array of strings, or { tools: [...] }.
    let arr = value
        .as_array()
        .cloned()
        .or_else(|| value.get("tools").and_then(|v| v.as_array()).cloned())
        .ok_or_else(|| {
            ToolError::Unavailable(format!("unexpected catalog shape: {value}"))
        })?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        match entry {
            serde_json::Value::String(s) => out.push(s),
            serde_json::Value::Object(map) => {
                if let Some(s) = map.get("name").and_then(|v| v.as_str()) {
                    out.push(s.to_string());
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Convenience: turn a list of tool names into the `tools=…` argument
/// shape expected by [`crate::ai::ChatOpts::tools`].
pub fn for_chat<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    names.into_iter().map(Into::into).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_rejects_blank_name() {
        let err = call("", &serde_json::json!({})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArg(_)));
    }

    #[test]
    fn for_chat_passes_through() {
        let names = for_chat(["fs.read_text", "kv.get"]);
        assert_eq!(names, vec!["fs.read_text", "kv.get"]);
    }
}
