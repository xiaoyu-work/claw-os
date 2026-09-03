//! Tool subsystem: trait, registry, and built-in tools.
//!
//! Tools are how the agent acts on the system. Phase 1 ships only safe,
//! side-effect-free built-in tools (`echo`, `now`) so the runtime can be
//! exercised without committing to a sandbox/credential integration. Phase 2
//! adds the cos-primitive proxies (fs/exec/proc/net/web/etc.).

pub mod app_gateway;
pub mod builtin;
pub mod clarify;
pub mod cos_apps;
pub mod cos_apps_session;
pub mod cos_help;
pub mod cos_proxy;
pub mod delegate;
pub mod exposure;
pub mod guardrails;
pub mod mcp;
pub mod media;
pub mod progressive;
pub mod registry;
pub mod skills;
pub mod todo;

use crate::agent::runtime::approval::ApprovalBoundary;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub(crate) const SYSTEM_AGENT_MEMORY_SCOPE: &str = "agent";

pub(crate) enum MemoryScope<'a> {
    SystemAgent,
    Session(&'a str),
    App(&'a str),
}

pub(crate) fn validate_memory_scope(value: &str, label: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(format!(
            "{label} must be a non-empty single-line identifier"
        ));
    }
    Ok(())
}

pub(crate) fn require_memory(
    verb: crate::caps::Verb,
    target: MemoryScope<'_>,
) -> Result<(), crate::caps::Denial> {
    let target = match target {
        MemoryScope::SystemAgent => SYSTEM_AGENT_MEMORY_SCOPE,
        MemoryScope::Session(session) | MemoryScope::App(session) => session,
    };
    let system_scope = crate::caps::Scope::self_ref(SYSTEM_AGENT_MEMORY_SCOPE);
    let requested_scope = crate::caps::Scope::self_ref(target);
    let scope = exposure::current()
        .filter(|context| {
            context
                .capabilities()
                .covers(&crate::caps::Cap::new(verb, system_scope.clone()))
        })
        .map_or(requested_scope, |_| system_scope);
    crate::caps::require(verb, scope)
}

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
    fn name(&self) -> &str;

    /// One-line human description shown in the tool list.
    fn description(&self) -> &str;

    /// JSON Schema describing the input shape. The schema is consumed by the
    /// LLM to decide how to call this tool.
    fn input_schema(&self) -> serde_json::Value;

    /// Immutable coarse requirements for advertising this tool. Exact
    /// argument-derived authorization still runs inside [`Tool::exec`].
    fn exposure(&self) -> exposure::ToolExposure {
        exposure::ToolExposure::always()
    }

    /// Discovery metadata for budget-driven schema disclosure. Core tools
    /// remain direct by default; extension-backed tools opt in explicitly.
    fn disclosure(&self) -> progressive::ToolDisclosure {
        progressive::ToolDisclosure::default()
    }

    /// Dynamic liveness for attached extension tools. The registry checks this
    /// both while projecting schemas and immediately before dispatch.
    fn is_available(&self) -> bool {
        true
    }

    /// Execute the tool. Errors should be returned via `ToolResult::err`,
    /// not via Result, so the model can see them and react.
    async fn exec(&self, input: serde_json::Value) -> ToolResult;

    /// May this tool be dispatched concurrently with siblings in the
    /// same turn? Default is `false` — every existing tool serializes,
    /// preserving the historical guarantee that side-effecting tools
    /// (shell exec, fs writes, network mutations) run one at a time
    /// in declaration order.
    ///
    /// Read-only tools that only inspect process / filesystem / system
    /// state (e.g. `cos_sysinfo`, `cos_app_web`, `cos_app_data` reads)
    /// should override this to `true` so multi-tool turns return in
    /// `max(durations)` rather than `sum(durations)`. The agent
    /// frequently fires 4–6 inspection calls in parallel — running
    /// them serially behind a slow filesystem walk dominated the
    /// "what's the biggest file" UX.
    fn parallel_safe(&self) -> bool {
        false
    }

    /// Identify the authoritative consent boundary for this tool.
    ///
    /// Capability-aware tools must still honour `auto_deny_tools`, but
    /// the legacy `dangerous_tools` prompt is skipped so a coarse tool
    /// name cannot pre-approve or block unrelated commands exposed by
    /// the same proxy.
    fn approval_boundary(&self) -> ApprovalBoundary {
        ApprovalBoundary::ToolName
    }
}
