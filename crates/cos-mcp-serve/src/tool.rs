//! `Tool` trait + `ToolResult`. Mirrors the kernel's
//! `core/src/agent/tools/mod.rs` shape so an App-side handler can be
//! written once and (in the future) reused on either side without
//! glue. The two definitions intentionally agree byte-for-byte; the
//! kernel converts between them at the registry boundary.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One tool's result. The model sees `content` verbatim; `is_error`
/// tells it whether the call failed so it can react. Failures are
/// **not** transport errors — they go back over the wire as a
/// successful JSON-RPC response with `isError: true`, exactly like
/// the spec's `CallToolResult` requires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// What a tool implementation must offer.
///
/// Designed to be cheap to box: `&self` (so a single instance can be
/// shared via `Arc`), `&'static str` names (so `tools/list` can hand
/// out borrows without allocating), and an async `exec` so handlers
/// can do I/O without spawning blocking threads themselves.
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// Stable, snake/dot-case identifier the model uses to reference
    /// this tool. Apps that group several tools under one verb
    /// commonly use dots (`screenshot.capture`).
    fn name(&self) -> &'static str;

    /// One-line human description shown in `tools/list`. The kernel
    /// agent surfaces this when summarising what's available.
    fn description(&self) -> &'static str;

    /// JSON Schema describing the input shape. The LLM consumes this
    /// to decide how to call the tool; the schema should be tight
    /// enough that malformed calls round-trip as cheap rejections,
    /// not as expensive App work.
    fn input_schema(&self) -> Value;

    /// Execute the tool. Errors should be returned via
    /// [`ToolResult::err`], **not** by panicking; panics in this
    /// method will tear the whole MCP server process down, which the
    /// kernel agent observes as the App going away.
    async fn exec(&self, input: Value) -> ToolResult;
}
