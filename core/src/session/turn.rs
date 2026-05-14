//! Turn record — what an agent writes to `turns.jsonl`.
//!
//! Schema goal: any agent runtime should be able to attach to a
//! session and resume the conversation by reading this file. The
//! format is **deliberately OpenAI-compatible at the wire level** so
//! the agent runtimes already running on claw-os can append turns with
//! near-zero translation cost.
//!
//! ## File layout
//!
//! `turns.jsonl` is one [`Turn`] per line, append-only, never
//! rewritten. Readers tolerate a trailing partial line (the only way
//! the file can become corrupt — a crash mid-write — leaves at worst
//! one half-written tail entry, which iterators skip).
//!
//! ## What goes here vs `state.json`
//!
//! - `turns.jsonl` — durable, ordered, shared across runtimes. The
//!   "conversation" that any agent can read to understand what
//!   happened.
//! - `state.json`  — opaque per-runtime scratch (compiled prompt
//!   prefixes, vector cache pointers, planner-internal queues). Other
//!   runtimes are free to ignore it.
//!
//! Rule of thumb: if another agent picking up the session needs to
//! see it to continue, it belongs in `turns.jsonl`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::meta::now_rfc3339;

/// Producer of a turn. Mirrors the OpenAI / Anthropic chat schema so
/// agent runtimes can convert with one `match` arm.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TurnRole {
    /// The end user. Free-form text.
    User,
    /// The agent's own LLM output. May carry tool_calls.
    Assistant,
    /// A system / developer-supplied prompt.
    System,
    /// Result of a tool the assistant invoked. `tool_call_id` should
    /// match an assistant turn's emitted tool call.
    Tool,
}

/// One conversation event. Fields beyond `role` / `content` are all
/// optional so unknown runtimes can produce minimal turns without
/// loss.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    /// Monotonic 0-based index within the session. The store assigns
    /// this — callers leave it at 0 in the constructor and let
    /// [`crate::session::append_turn`] set it.
    #[serde(default)]
    pub seq: u64,

    /// RFC 3339 UTC. Auto-stamped by the store if left empty.
    #[serde(default)]
    pub at: String,

    pub role: TurnRole,

    /// Free-form. For `assistant` turns with tool calls this is often
    /// empty or the user-facing summary; the actual call goes in
    /// `tool_calls`. For `tool` turns this is the tool's stdout / json
    /// payload as a string.
    #[serde(default)]
    pub content: String,

    /// Optional label for the runtime that produced this turn
    /// (`"cos-agent"`, `"langchain-py"`, …). Lets a GUI badge cross-
    /// runtime sessions and lets curators filter by source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,

    /// Tool invocations emitted by this turn. We carry them as opaque
    /// JSON so we don't have to keep the schema in lockstep with every
    /// provider's evolving function-call format.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<Value>,

    /// For `role = tool`: the id of the assistant turn's tool call
    /// this completes. Empty for other roles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    /// Token usage if the runtime tracked it. Carried for budget
    /// enforcement and audit display. Opaque shape so any provider's
    /// `usage` block fits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

impl Turn {
    /// Convenience constructor for plain user / assistant text.
    /// Caller does not set `seq` or `at` — the store fills both.
    pub fn text(role: TurnRole, content: impl Into<String>) -> Self {
        Self {
            seq: 0,
            at: String::new(),
            role,
            content: content.into(),
            runtime: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            usage: None,
        }
    }

    /// Stamp `at` with the current time if it's empty. Called by the
    /// store before serializing.
    pub(super) fn stamp_default_time(&mut self) {
        if self.at.is_empty() {
            self.at = now_rfc3339();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_constructor_defaults() {
        let t = Turn::text(TurnRole::User, "hello");
        assert_eq!(t.role, TurnRole::User);
        assert_eq!(t.content, "hello");
        assert_eq!(t.seq, 0);
        assert!(t.at.is_empty());
        assert!(t.tool_calls.is_empty());
    }

    #[test]
    fn role_serializes_kebab() {
        assert_eq!(serde_json::to_string(&TurnRole::User).unwrap(), "\"user\"");
        assert_eq!(serde_json::to_string(&TurnRole::Assistant).unwrap(), "\"assistant\"");
        assert_eq!(serde_json::to_string(&TurnRole::Tool).unwrap(), "\"tool\"");
    }

    #[test]
    fn stamp_default_time_only_when_empty() {
        let mut t = Turn::text(TurnRole::User, "a");
        t.stamp_default_time();
        assert!(!t.at.is_empty());
        let kept = t.at.clone();
        t.stamp_default_time();
        assert_eq!(t.at, kept, "second stamp must be a no-op");
    }

    #[test]
    fn round_trip_with_tool_calls() {
        let t = Turn {
            seq: 7,
            at: "2026-01-01T00:00:00Z".into(),
            role: TurnRole::Assistant,
            content: "let me check".into(),
            runtime: Some("cos-agent".into()),
            tool_calls: vec![serde_json::json!({
                "id": "call_1",
                "name": "fs.read",
                "arguments": { "path": "/etc/hosts" }
            })],
            tool_call_id: None,
            usage: Some(serde_json::json!({
                "input_tokens": 12,
                "output_tokens": 4
            })),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: Turn = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn unknown_runtime_can_round_trip_minimal_turn() {
        // Simulate a minimal turn a non-claw runtime might produce.
        let raw = r#"{"role":"user","content":"hi"}"#;
        let t: Turn = serde_json::from_str(raw).unwrap();
        assert_eq!(t.role, TurnRole::User);
        assert_eq!(t.content, "hi");
        // Optional fields default cleanly.
        assert_eq!(t.seq, 0);
        assert!(t.runtime.is_none());
        assert!(t.tool_calls.is_empty());
    }
}
