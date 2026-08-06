//! Catalog-tool helper — apps that want to fulfil a model-proposed
//! tool call (returned in [`crate::ai::AiResponse::tool_calls`]) shell
//! out through this module to `cos ai tool <name> --app <id> --args <json>`.
//! Mirrors the Python [`claw_os_sdk.tools`] module.
//!
//! The kernel runs the catalog implementation under the app's own
//! capabilities, audits the call, and returns a structured result.

#![allow(clippy::result_large_err)]

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
    pub tool: String,
    pub app_id: String,
    pub status: String,
    /// Tool's domain-specific output — shape depends on the tool.
    pub result: serde_json::Value,
}

/// One row from the live `cos ai tools` catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    pub summary: String,
    pub verb: String,
    pub stability: String,
    pub args_schema: serde_json::Value,
    pub returns_schema: serde_json::Value,
}

fn cos_tool_json(
    name: &str,
    argv: Vec<OsString>,
) -> Result<serde_json::Value, ToolError> {
    match cos_call_json("tool", name, argv) {
        Ok(value) => Ok(value),
        Err(BridgeError::AppError {
            message,
            code,
            ..
        }) => {
            let mut payload = serde_json::json!({ "error": &message });
            if let Some(code) = code.clone() {
                payload["code"] = serde_json::Value::String(code);
            }
            Err(ToolError::Denied {
                name: name.to_string(),
                message,
                code,
                payload,
            })
        }
        Err(error) => Err(ToolError::Bridge(error)),
    }
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

    let value = cos_tool_json(name, argv)?;
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

/// Return the live global catalog from the argument-free `cos ai tools`.
pub fn catalog() -> Result<Vec<CatalogEntry>, ToolError> {
    let argv: Vec<OsString> = vec!["ai".into(), "tools".into()];
    let value = cos_tool_json("catalog", argv)?;
    let rows = value
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            ToolError::Unavailable(format!("catalog response omitted `tools`: {value}"))
        })?;
    rows.iter()
        .cloned()
        .map(|row| {
            serde_json::from_value(row)
                .map_err(|error| ToolError::Unavailable(format!("catalog decode failed: {error}")))
        })
        .collect()
}

/// Convenience projection for callers that only need stable tool names.
pub fn catalog_names() -> Result<Vec<String>, ToolError> {
    catalog().map(|entries| entries.into_iter().map(|entry| entry.name).collect())
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
