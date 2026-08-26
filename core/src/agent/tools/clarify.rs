//! `cos_clarify` — agent clarification request tool.
//!
//! The model invokes this when it doesn't have enough information to
//! proceed safely or correctly: ambiguous user intent, missing
//! parameters, multiple plausible interpretations, etc. The tool
//! records the question (and proposed options, if any) and returns a
//! structured marker the runtime can detect.
//!
//! ## Why this exists
//!
//! Without an explicit clarify channel, models tend to either (a)
//! pick an arbitrary interpretation and run with it (silent failure)
//! or (b) embed the question inside an `assistant` message that the
//! caller may treat as a plain narration. A dedicated tool gives:
//!
//!   * A typed surface for downstream UIs / interactive shells to
//!     render a prompt and capture an answer.
//!   * A natural pause point for the runtime — when the most recent
//!     tool_result is a `ClarifyOutcome::Pending`, the loop can
//!     suspend instead of continuing to call the model.
//!   * An audit-friendly record of *why* the agent asked and what
//!     it asked.
//!
//! ## Operational shape
//!
//! Headless / non-interactive callers (default in this commit) get a
//! `Pending` outcome — the question is recorded, the tool returns a
//! marker, and the runtime is expected to surface it via the normal
//! tool_result channel and stop. Interactive callers can supply a
//! [`ClarifyResponder`] that resolves the question synchronously
//! (e.g. read line, prompt the user, return the typed answer).
//!
//! Library-only this commit. The runtime's handling of
//! `ClarifyOutcome::Pending` is intentionally not wired here —
//! that's the runtime integration's call.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::tools::{Tool, ToolResult};

/// Caller-supplied clarification request payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClarifyRequest {
    /// The question to ask the user. Must be non-empty.
    pub question: String,
    /// Optional canned options. Free-form answers are still
    /// permitted unless the responder enforces selection.
    #[serde(default)]
    pub options: Vec<String>,
    /// Why the model can't continue without an answer. Helps the
    /// human evaluate the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ClarifyRequest {
    fn validate(&self) -> Result<(), &'static str> {
        if self.question.trim().is_empty() {
            return Err("question is required and must be non-empty");
        }
        Ok(())
    }
}

/// Result of dispatching a clarification request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClarifyOutcome {
    /// User supplied a real answer.
    Answered { answer: String },
    /// Question was recorded but no synchronous answer is available
    /// (headless mode, or responder deferred). The runtime should
    /// surface this as a stop point.
    Pending { question: String },
    /// User declined / cancelled.
    Cancelled { reason: Option<String> },
}

/// Pluggable strategy for resolving a clarification request.
///
/// `None` (the default) makes every clarify call return
/// [`ClarifyOutcome::Pending`].
#[async_trait]
pub trait ClarifyResponder: Send + Sync {
    async fn ask(&self, request: &ClarifyRequest) -> ClarifyOutcome;
}

/// `cos_clarify` LLM tool.
pub struct Clarify {
    responder: Option<Arc<dyn ClarifyResponder>>,
}

impl Default for Clarify {
    fn default() -> Self {
        Self::new()
    }
}

impl Clarify {
    pub fn new() -> Self {
        Self { responder: None }
    }

    pub fn with_responder(responder: Arc<dyn ClarifyResponder>) -> Self {
        Self {
            responder: Some(responder),
        }
    }

    /// Direct programmatic invocation (useful in tests or when the
    /// runtime wants to call clarify without going through JSON).
    pub async fn ask(&self, request: ClarifyRequest) -> ClarifyOutcome {
        if let Err(msg) = request.validate() {
            return ClarifyOutcome::Cancelled {
                reason: Some(msg.to_string()),
            };
        }
        match &self.responder {
            Some(r) => r.ask(&request).await,
            None => ClarifyOutcome::Pending {
                question: request.question,
            },
        }
    }
}

#[async_trait]
impl Tool for Clarify {
    fn name(&self) -> &str {
        "cos_clarify"
    }

    fn description(&self) -> &str {
        "Ask the user a clarifying question when intent is ambiguous or required information is missing. \
         Returns the user's answer, or a `pending` marker in headless mode."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "Concise, single-question prompt to show the user."
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional canned options. Free-form answers are still allowed."
                },
                "reason": {
                    "type": "string",
                    "description": "Brief explanation of why the question is needed."
                }
            },
            "required": ["question"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: serde_json::Value) -> ToolResult {
        let request: ClarifyRequest = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("invalid clarify input: {e}")),
        };
        if let Err(msg) = request.validate() {
            return ToolResult::err(msg.to_string());
        }
        let outcome = self.ask(request).await;
        match serde_json::to_string(&outcome) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("failed to serialise outcome: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/clarify.rs"
    ));
}
