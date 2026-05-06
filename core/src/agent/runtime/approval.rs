//! Approval gating for dangerous tool invocations.
//!
//! The agent loop consults [`ApprovalGate`] before executing a
//! tool call that the policy classifies as dangerous. The gate
//! either auto-approves (whitelisted), auto-denies (blacklisted),
//! or defers to a [`Approver`] strategy — interactive prompt,
//! external service, or in headless mode a `Deferred` outcome the
//! runtime surfaces back to the caller.
//!
//! ## Why a separate module
//!
//! Approval is orthogonal to [`super::guardrails::Guardrails`]
//! (which decides which tools the LLM can *see*). Guardrails are
//! a coarse pre-filter; approval is a per-call gate. A tool may
//! be advertised to the model but its actual invocations may
//! require explicit human consent — for example, `cos_proc`
//! invocations with `command: kill` or `cos_credential` writes.
//!
//! ## Default policy
//!
//! No tools are dangerous by default. Callers configure the
//! `dangerous` set explicitly (matched against tool name + an
//! optional input-shape predicate). Failure-closed defaults are
//! a deliberate choice deferred to the policy layer (which lives
//! one rung above this module — see runtime integration).
//!
//! Library-only this commit. Wiring the gate into the per-turn
//! loop (intercept tool dispatch, route Deferred outcomes back
//! through the response stream) is a runtime-layer change.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One pending approval request.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    /// Why this call was flagged dangerous (rule id or reason).
    pub reason: String,
}

/// Outcome of evaluating an approval request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ApprovalOutcome {
    /// Tool may proceed.
    Approved {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Tool refused.
    Denied {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// No synchronous decision available — caller must surface
    /// the request via its UI / API and re-invoke once the user
    /// has responded.
    Deferred {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
}

/// Pluggable async strategy for resolving approval requests.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalOutcome;
}

/// Approver that always returns the same outcome. Useful for tests
/// and for headless deployments that auto-deny everything.
pub struct StaticApprover {
    pub outcome: ApprovalOutcome,
}

#[async_trait]
impl Approver for StaticApprover {
    async fn approve(&self, _: &ApprovalRequest) -> ApprovalOutcome {
        self.outcome.clone()
    }
}

/// Approver that defers everything (the safe headless default).
pub struct DeferringApprover;

#[async_trait]
impl Approver for DeferringApprover {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalOutcome {
        ApprovalOutcome::Deferred {
            prompt: Some(format!(
                "approval required for `{}`: {}",
                request.tool_name, request.reason
            )),
        }
    }
}

/// Configuration for [`ApprovalGate`]. All sets default empty.
#[derive(Debug, Clone, Default)]
pub struct ApprovalConfig {
    /// Tools that are *always* allowed to run without prompting.
    /// Takes precedence over `dangerous`.
    pub auto_approve: BTreeSet<String>,
    /// Tools that are *always* blocked.
    pub auto_deny: BTreeSet<String>,
    /// Tools that require explicit approval. Names not in this
    /// set bypass the approver entirely.
    pub dangerous: BTreeSet<String>,
}

impl ApprovalConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn auto_approve(mut self, name: impl Into<String>) -> Self {
        self.auto_approve.insert(name.into());
        self
    }

    pub fn auto_deny(mut self, name: impl Into<String>) -> Self {
        self.auto_deny.insert(name.into());
        self
    }

    pub fn dangerous(mut self, name: impl Into<String>) -> Self {
        self.dangerous.insert(name.into());
        self
    }
}

/// Owns the policy + the active approver. Cheap to clone (Arc on
/// the approver, Arc on the config).
#[derive(Clone)]
pub struct ApprovalGate {
    config: Arc<ApprovalConfig>,
    approver: Option<Arc<dyn Approver>>,
}

impl ApprovalGate {
    pub fn new(config: ApprovalConfig) -> Self {
        Self {
            config: Arc::new(config),
            approver: None,
        }
    }

    pub fn with_approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = Some(approver);
        self
    }

    pub fn config(&self) -> &ApprovalConfig {
        &self.config
    }

    /// Decide whether `tool_name` may execute with `tool_input`.
    ///
    /// Decision tree, in order:
    ///   1. `auto_deny` → `Denied`.
    ///   2. `auto_approve` → `Approved`.
    ///   3. Not in `dangerous` → `Approved` (silent pass-through).
    ///   4. In `dangerous`, no approver configured →
    ///      `Deferred`.
    ///   5. In `dangerous`, approver configured → defer to approver.
    pub async fn evaluate(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
        reason: impl Into<String>,
    ) -> ApprovalOutcome {
        if self.config.auto_deny.contains(tool_name) {
            return ApprovalOutcome::Denied {
                reason: Some(format!("`{tool_name}` is in auto_deny list")),
            };
        }
        if self.config.auto_approve.contains(tool_name) {
            return ApprovalOutcome::Approved {
                note: Some("auto-approved by policy".to_string()),
            };
        }
        if !self.config.dangerous.contains(tool_name) {
            return ApprovalOutcome::Approved { note: None };
        }
        let request = ApprovalRequest {
            tool_name: tool_name.to_string(),
            tool_input: tool_input.clone(),
            reason: reason.into(),
        };
        match &self.approver {
            Some(a) => a.approve(&request).await,
            None => ApprovalOutcome::Deferred {
                prompt: Some(format!(
                    "no approver configured; `{tool_name}` requires approval"
                )),
            },
        }
    }

    /// Convenience: returns `true` if `tool_name` would short-circuit
    /// without invoking the approver. Useful for the runtime to
    /// avoid building a request struct when not needed.
    pub fn would_short_circuit(&self, tool_name: &str) -> bool {
        self.config.auto_deny.contains(tool_name)
            || self.config.auto_approve.contains(tool_name)
            || !self.config.dangerous.contains(tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn req(name: &str, input: serde_json::Value) -> ApprovalRequest {
        ApprovalRequest {
            tool_name: name.to_string(),
            tool_input: input,
            reason: "test".to_string(),
        }
    }

    #[tokio::test]
    async fn non_dangerous_tool_passes_through() {
        let gate = ApprovalGate::new(ApprovalConfig::new());
        let out = gate.evaluate("cos_echo", &json!({}), "n/a").await;
        assert!(matches!(out, ApprovalOutcome::Approved { .. }));
    }

    #[tokio::test]
    async fn auto_deny_takes_precedence_over_auto_approve() {
        let cfg = ApprovalConfig::new()
            .auto_approve("dangerous_tool")
            .auto_deny("dangerous_tool");
        let gate = ApprovalGate::new(cfg);
        let out = gate.evaluate("dangerous_tool", &json!({}), "test").await;
        assert!(matches!(out, ApprovalOutcome::Denied { .. }));
    }

    #[tokio::test]
    async fn auto_approve_short_circuits() {
        let cfg = ApprovalConfig::new()
            .auto_approve("safe_tool")
            .dangerous("safe_tool");
        let gate = ApprovalGate::new(cfg);
        let out = gate.evaluate("safe_tool", &json!({}), "test").await;
        match out {
            ApprovalOutcome::Approved { note } => {
                assert!(note.unwrap().contains("auto-approved"));
            }
            other => panic!("expected approved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dangerous_no_approver_defers() {
        let cfg = ApprovalConfig::new().dangerous("cos_proc");
        let gate = ApprovalGate::new(cfg);
        let out = gate.evaluate("cos_proc", &json!({}), "kill cmd").await;
        assert!(matches!(out, ApprovalOutcome::Deferred { .. }));
    }

    #[tokio::test]
    async fn dangerous_with_approver_uses_approver() {
        let cfg = ApprovalConfig::new().dangerous("cos_proc");
        let gate = ApprovalGate::new(cfg).with_approver(Arc::new(StaticApprover {
            outcome: ApprovalOutcome::Approved {
                note: Some("ok".to_string()),
            },
        }));
        let out = gate.evaluate("cos_proc", &json!({}), "test").await;
        match out {
            ApprovalOutcome::Approved { note } => assert_eq!(note.as_deref(), Some("ok")),
            other => panic!("expected approved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deferring_approver_emits_prompt() {
        let cfg = ApprovalConfig::new().dangerous("cos_proc");
        let gate = ApprovalGate::new(cfg).with_approver(Arc::new(DeferringApprover));
        let out = gate.evaluate("cos_proc", &json!({}), "kill cmd").await;
        match out {
            ApprovalOutcome::Deferred { prompt } => {
                let p = prompt.unwrap();
                assert!(p.contains("cos_proc"));
                assert!(p.contains("kill cmd"));
            }
            other => panic!("expected deferred, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approver_sees_full_request() {
        struct Capture(AtomicUsize, std::sync::Mutex<Option<ApprovalRequest>>);
        #[async_trait]
        impl Approver for Capture {
            async fn approve(&self, request: &ApprovalRequest) -> ApprovalOutcome {
                self.0.fetch_add(1, Ordering::SeqCst);
                *self.1.lock().unwrap() = Some(request.clone());
                ApprovalOutcome::Denied {
                    reason: Some("captured".to_string()),
                }
            }
        }
        let capture = Arc::new(Capture(AtomicUsize::new(0), std::sync::Mutex::new(None)));
        let cfg = ApprovalConfig::new().dangerous("cos_credential");
        let gate = ApprovalGate::new(cfg).with_approver(capture.clone());
        let _ = gate
            .evaluate("cos_credential", &json!({"action": "set"}), "writes")
            .await;
        assert_eq!(capture.0.load(Ordering::SeqCst), 1);
        let captured = capture.1.lock().unwrap().clone().unwrap();
        assert_eq!(captured.tool_name, "cos_credential");
        assert_eq!(captured.tool_input["action"], "set");
        assert_eq!(captured.reason, "writes");
    }

    #[tokio::test]
    async fn approver_not_invoked_for_non_dangerous() {
        struct ShouldNotBeCalled;
        #[async_trait]
        impl Approver for ShouldNotBeCalled {
            async fn approve(&self, _: &ApprovalRequest) -> ApprovalOutcome {
                panic!("approver should not be called for non-dangerous tool");
            }
        }
        let gate = ApprovalGate::new(ApprovalConfig::new())
            .with_approver(Arc::new(ShouldNotBeCalled));
        let out = gate.evaluate("safe", &json!({}), "n/a").await;
        assert!(matches!(out, ApprovalOutcome::Approved { .. }));
    }

    #[test]
    fn would_short_circuit_when_not_dangerous() {
        let gate = ApprovalGate::new(ApprovalConfig::new());
        assert!(gate.would_short_circuit("anything"));
    }

    #[test]
    fn would_short_circuit_for_auto_deny() {
        let gate = ApprovalGate::new(ApprovalConfig::new().auto_deny("x"));
        assert!(gate.would_short_circuit("x"));
    }

    #[test]
    fn would_short_circuit_for_auto_approve() {
        let gate = ApprovalGate::new(ApprovalConfig::new().auto_approve("x"));
        assert!(gate.would_short_circuit("x"));
    }

    #[test]
    fn would_not_short_circuit_for_dangerous() {
        let gate = ApprovalGate::new(ApprovalConfig::new().dangerous("x"));
        assert!(!gate.would_short_circuit("x"));
    }

    #[test]
    fn config_builder_chains() {
        let cfg = ApprovalConfig::new()
            .auto_approve("a")
            .auto_deny("b")
            .dangerous("c");
        assert!(cfg.auto_approve.contains("a"));
        assert!(cfg.auto_deny.contains("b"));
        assert!(cfg.dangerous.contains("c"));
    }

    #[test]
    fn outcome_serialisation_uses_decision_tag() {
        let approved = ApprovalOutcome::Approved {
            note: Some("ok".to_string()),
        };
        let v = serde_json::to_value(&approved).unwrap();
        assert_eq!(v["decision"], "approved");
        assert_eq!(v["note"], "ok");
    }

    #[test]
    fn outcome_round_trip_for_all_variants() {
        for o in [
            ApprovalOutcome::Approved { note: None },
            ApprovalOutcome::Approved {
                note: Some("ok".to_string()),
            },
            ApprovalOutcome::Denied { reason: None },
            ApprovalOutcome::Denied {
                reason: Some("nope".to_string()),
            },
            ApprovalOutcome::Deferred { prompt: None },
            ApprovalOutcome::Deferred {
                prompt: Some("ask?".to_string()),
            },
        ] {
            let s = serde_json::to_string(&o).unwrap();
            let parsed: ApprovalOutcome = serde_json::from_str(&s).unwrap();
            assert_eq!(parsed, o);
        }
    }

    #[tokio::test]
    async fn gate_clone_shares_state() {
        let cfg = ApprovalConfig::new().dangerous("cos_proc");
        let gate = ApprovalGate::new(cfg).with_approver(Arc::new(DeferringApprover));
        let cloned = gate.clone();
        let out_orig = gate.evaluate("cos_proc", &json!({}), "test").await;
        let out_clone = cloned.evaluate("cos_proc", &json!({}), "test").await;
        assert_eq!(out_orig, out_clone);
    }
}
