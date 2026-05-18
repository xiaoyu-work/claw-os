//! Built-in tools: side-effect-free utilities included with every agent.
//!
//! Phase 1 keeps this list minimal — these tools exist mostly so the runtime
//! can be exercised end-to-end without exposing the system. Real cos-primitive
//! proxies (fs/exec/proc/net/web/sandbox/checkpoint/...) land in Phase 2.

use async_trait::async_trait;
use serde_json::json;

use super::{Tool, ToolResult};

/// `echo` — return the input text unchanged. Useful for tool-loop sanity tests.
pub struct Echo;

#[async_trait]
impl Tool for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Return the provided text unchanged. Useful for testing the tool loop."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to echo back."
                }
            },
            "required": ["text"]
        })
    }

    async fn exec(&self, input: serde_json::Value) -> ToolResult {
        match input.get("text").and_then(|v| v.as_str()) {
            Some(text) => ToolResult::ok(text.to_string()),
            None => ToolResult::err("missing required field: text"),
        }
    }

    fn parallel_safe(&self) -> bool {
        // Pure function: returns its argument. Trivially safe to
        // run concurrently with anything.
        true
    }
}

/// `now` — return the current UTC time as RFC 3339.
pub struct Now;

#[async_trait]
impl Tool for Now {
    fn name(&self) -> &'static str {
        "now"
    }

    fn description(&self) -> &'static str {
        "Return the current UTC time in RFC 3339 format."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn exec(&self, _input: serde_json::Value) -> ToolResult {
        let now = chrono::Utc::now().to_rfc3339();
        ToolResult::ok(now)
    }

    fn parallel_safe(&self) -> bool {
        // Read-only clock query; no side effects.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echo_returns_text() {
        let r = Echo.exec(json!({"text": "hello"})).await;
        assert!(!r.is_error);
        assert_eq!(r.content, "hello");
    }

    #[tokio::test]
    async fn echo_missing_field() {
        let r = Echo.exec(json!({})).await;
        assert!(r.is_error);
        assert!(r.content.contains("text"));
    }

    #[tokio::test]
    async fn now_returns_rfc3339() {
        let r = Now.exec(json!({})).await;
        assert!(!r.is_error);
        // Year 20XX or 21XX, RFC 3339-ish: we just assert it parses.
        let parsed = chrono::DateTime::parse_from_rfc3339(&r.content);
        assert!(parsed.is_ok(), "expected RFC3339, got: {}", r.content);
    }
}
