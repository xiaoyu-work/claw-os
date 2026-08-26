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
    /// Pre-computed union of `auto_approve ∪ auto_deny ∪ dangerous`.
    /// Used by [`ApprovalGate::is_classified`] so the runtime can do a
    /// single O(1) lookup per `dispatch_tool` instead of three separate
    /// `BTreeSet::contains` calls. Built once at construction; shared
    /// across clones via `Arc`.
    classified: Arc<std::collections::HashSet<String>>,
}

impl Default for ApprovalGate {
    /// Empty gate — every call short-circuits to `Approved`. Safe
    /// to use in contexts where no policy has been configured (e.g.
    /// the default [`crate::agent::tools::registry::ToolRegistry`]).
    fn default() -> Self {
        Self::new(ApprovalConfig::default())
    }
}

impl ApprovalGate {
    pub fn new(config: ApprovalConfig) -> Self {
        let mut classified =
            std::collections::HashSet::with_capacity(
                config.auto_approve.len() + config.auto_deny.len() + config.dangerous.len(),
            );
        for n in &config.auto_approve {
            classified.insert(n.clone());
        }
        for n in &config.auto_deny {
            classified.insert(n.clone());
        }
        for n in &config.dangerous {
            classified.insert(n.clone());
        }
        Self {
            config: Arc::new(config),
            approver: None,
            classified: Arc::new(classified),
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

    /// Single-lookup classification check: `true` iff `tool_name` is
    /// configured under *any* of `auto_approve`, `auto_deny`, or
    /// `dangerous`. The runtime uses this to skip the full
    /// [`evaluate`](Self::evaluate) call when the tool has no policy
    /// (the common case), with a single `HashSet` lookup instead of
    /// three separate `BTreeSet::contains` calls.
    pub fn is_classified(&self, tool_name: &str) -> bool {
        self.classified.contains(tool_name)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/approval.rs"
    ));
}
