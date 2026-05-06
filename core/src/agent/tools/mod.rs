//! Tool subsystem: trait, registry, and built-in tools.
//!
//! Tools are how the agent acts on the system. Phase 1 ships only safe,
//! side-effect-free built-in tools (`echo`, `now`) so the runtime can be
//! exercised without committing to a sandbox/credential integration. Phase 2
//! adds the cos-primitive proxies (fs/exec/proc/net/web/etc.).

pub mod builtin;
pub mod clarify;
pub mod cos_proxy;
pub mod delegate;
pub mod guardrails;
pub mod media;
pub mod registry;
pub mod todo;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Result content shown back to the model. Plain text recommended; may
    /// contain JSON / formatted blocks if it helps the model reason.
    pub content: String,
    /// True if this tool call failed. The model sees this and can react.
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
#[async_trait]
pub trait Tool: Send + Sync {
    /// Stable, snake_case identifier exposed to the model.
    fn name(&self) -> &'static str;

    /// One-line human description shown in the tool list.
    fn description(&self) -> &'static str;

    /// JSON Schema describing the input shape. The schema is consumed by the
    /// LLM to decide how to call this tool.
    fn input_schema(&self) -> serde_json::Value;

    /// Execute the tool. Errors should be returned via `ToolResult::err`,
    /// not via Result, so the model can see them and react.
    async fn exec(&self, input: serde_json::Value) -> ToolResult;
}
