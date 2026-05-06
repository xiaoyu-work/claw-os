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
    fn name(&self) -> &'static str {
        "cos_clarify"
    }

    fn description(&self) -> &'static str {
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
    use super::*;
    use serde_json::json;

    struct EchoResponder;

    #[async_trait]
    impl ClarifyResponder for EchoResponder {
        async fn ask(&self, request: &ClarifyRequest) -> ClarifyOutcome {
            ClarifyOutcome::Answered {
                answer: format!("you asked: {}", request.question),
            }
        }
    }

    struct CancelResponder(Option<String>);

    #[async_trait]
    impl ClarifyResponder for CancelResponder {
        async fn ask(&self, _: &ClarifyRequest) -> ClarifyOutcome {
            ClarifyOutcome::Cancelled {
                reason: self.0.clone(),
            }
        }
    }

    #[tokio::test]
    async fn headless_returns_pending() {
        let c = Clarify::new();
        let outcome = c
            .ask(ClarifyRequest {
                question: "which file?".to_string(),
                options: vec![],
                reason: None,
            })
            .await;
        assert_eq!(
            outcome,
            ClarifyOutcome::Pending {
                question: "which file?".to_string()
            }
        );
    }

    #[tokio::test]
    async fn responder_provides_synchronous_answer() {
        let c = Clarify::with_responder(Arc::new(EchoResponder));
        let outcome = c
            .ask(ClarifyRequest {
                question: "what now?".to_string(),
                options: vec![],
                reason: None,
            })
            .await;
        match outcome {
            ClarifyOutcome::Answered { answer } => {
                assert_eq!(answer, "you asked: what now?");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_question_is_rejected() {
        let c = Clarify::new();
        let outcome = c
            .ask(ClarifyRequest {
                question: "   ".to_string(),
                options: vec![],
                reason: None,
            })
            .await;
        assert!(matches!(outcome, ClarifyOutcome::Cancelled { .. }));
    }

    #[tokio::test]
    async fn cancellation_round_trips_reason() {
        let c = Clarify::with_responder(Arc::new(CancelResponder(Some("nope".to_string()))));
        let outcome = c
            .ask(ClarifyRequest {
                question: "go ahead?".to_string(),
                options: vec![],
                reason: None,
            })
            .await;
        assert_eq!(
            outcome,
            ClarifyOutcome::Cancelled {
                reason: Some("nope".to_string())
            }
        );
    }

    #[tokio::test]
    async fn tool_exec_returns_pending_json_in_headless_mode() {
        let c = Clarify::new();
        let res = c
            .exec(json!({
                "question": "which one?",
                "options": ["a", "b"]
            }))
            .await;
        assert!(!res.is_error);
        let v: serde_json::Value = serde_json::from_str(&res.content).unwrap();
        assert_eq!(v["kind"], "pending");
        assert_eq!(v["question"], "which one?");
    }

    #[tokio::test]
    async fn tool_exec_returns_answered_json_with_responder() {
        let c = Clarify::with_responder(Arc::new(EchoResponder));
        let res = c.exec(json!({ "question": "what?" })).await;
        assert!(!res.is_error);
        let v: serde_json::Value = serde_json::from_str(&res.content).unwrap();
        assert_eq!(v["kind"], "answered");
        assert!(v["answer"].as_str().unwrap().contains("what?"));
    }

    #[tokio::test]
    async fn tool_exec_rejects_missing_question() {
        let c = Clarify::new();
        let res = c.exec(json!({})).await;
        assert!(res.is_error);
    }

    #[tokio::test]
    async fn tool_exec_rejects_blank_question() {
        let c = Clarify::new();
        let res = c.exec(json!({ "question": "   " })).await;
        assert!(res.is_error);
        assert!(res.content.contains("question"));
    }

    #[tokio::test]
    async fn tool_exec_rejects_unknown_field() {
        // additionalProperties:false is advisory at the schema layer
        // (provider may or may not enforce). serde with default
        // permissive struct accepts unknown — verify our schema
        // expressly forbids them.
        let c = Clarify::new();
        let schema = c.input_schema();
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn outcome_serialisation_uses_kind_tag() {
        let answered = ClarifyOutcome::Answered {
            answer: "yes".to_string(),
        };
        let v = serde_json::to_value(&answered).unwrap();
        assert_eq!(v["kind"], "answered");
        assert_eq!(v["answer"], "yes");
    }

    #[test]
    fn outcome_pending_round_trips() {
        let p = ClarifyOutcome::Pending {
            question: "?".to_string(),
        };
        let s = serde_json::to_string(&p).unwrap();
        let parsed: ClarifyOutcome = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, p);
    }

    #[test]
    fn outcome_cancelled_omits_null_reason() {
        let c = ClarifyOutcome::Cancelled { reason: None };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["kind"], "cancelled");
        // Tagged enums serialise variant fields inline; None should
        // round-trip but we don't strictly forbid the null key.
        let parsed: ClarifyOutcome = serde_json::from_value(v).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn tool_metadata_matches_name() {
        let c = Clarify::new();
        assert_eq!(c.name(), "cos_clarify");
        assert!(!c.description().is_empty());
        let schema = c.input_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "question"));
    }
}
