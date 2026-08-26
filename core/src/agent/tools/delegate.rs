//! `cos_delegate` — spawn a sub-agent with a scoped tool subset.
//!
//! Lets a parent agent fan a focused sub-task out to a child agent that has
//! a different (usually narrower) set of tools, and optionally a different
//! model/provider. The child runs a complete, fresh `ask_with` loop and
//! returns its final assistant text to the parent as a tool result.
//!
//! Why a scoped sub-agent matters:
//!
//!   * **Blast radius**: parent picks the subset of action tools the child can
//!     use (e.g. only `echo` + `cos_sysinfo`), so an over-eager child can't
//!     touch credentials or the sandbox unless explicitly allowed. Read-only
//!     `cos_skill` remains available unless parent guardrails deny it.
//!   * **Context isolation**: the child has a fresh trajectory, so its
//!     turns don't pollute the parent's prompt window. Useful for
//!     long-running research / extraction tasks the parent only needs the
//!     final summary of.
//!   * **Model choice**: the parent might be a 200B-class planner and the
//!     child a small fast worker. `provider` / `model` overrides let the
//!     parent route by cost.
//!
//! ## Recursion bounding
//!
//! Each call increments a [`DELEGATE_DEPTH`] tokio task-local. If a
//! delegated child also calls `cos_delegate`, depth = 2; once depth would
//! exceed `max_depth` (default 3, cap 5) the call is refused with a tool
//! error. This prevents runaway delegation trees.
//!
//! ## Memory
//!
//! The child runs via `ask_with` (no DB), so its turns do **not** land in
//! the parent's SQLite-FTS5 history. The parent's tool_use / tool_result
//! pair *does* get recorded under the parent's session, which is
//! sufficient to recover what was delegated and what came back.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use super::guardrails::Guardrails;
use super::registry::{default_registry, ToolRegistry};
use super::{Tool, ToolResult};
use crate::agent::llm::{self, Provider};
use crate::agent::runtime::approval::ApprovalGate;
use crate::agent::runtime::loop_::{self, AskResult};
use crate::config::AgentConfig;

/// Default ceiling for nested delegate calls. A single-level fan-out is the
/// common case; we allow a couple of nested layers but cap at 3 to keep
/// total cost predictable.
pub const DEFAULT_MAX_DEPTH: u32 = 3;

/// Hard ceiling regardless of caller-provided override. Keeps a buggy
/// agent from setting `max_depth` to `u32::MAX`.
pub const HARD_MAX_DEPTH: u32 = 5;

/// Hard ceiling for the child's `max_turns`. The default `AgentConfig`
/// uses 10 turns; we let callers go higher for research-heavy tasks but
/// cap at 50 to bound spend.
pub const HARD_MAX_TURNS: u32 = 50;

/// Default per-call timeout. A delegated task that hasn't produced a final
/// answer in 10 minutes is almost certainly stuck.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

tokio::task_local! {
    /// Depth of the current delegate call stack. 0 = top-level agent (no
    /// delegate active). Each `cos_delegate.exec` reads this, refuses if
    /// `>= max_depth`, then runs the child inside `scope(depth + 1, ...)`.
    static DELEGATE_DEPTH: u32;

    /// Snapshot of the *parent* registry's [`Guardrails`] set by the
    /// caller (typically `runtime::turn::dispatch_tool`) around every
    /// `Tool::exec`. The delegate tool reads it via [`current_parent_policy`]
    /// to propagate deny rules and approval policy into the child registry.
    /// Outside an active scope the value is `None`, meaning "no parent
    /// constraint" (default permissive).
    pub static PARENT_GUARDRAILS: Guardrails;

    /// Snapshot of the parent registry's [`ApprovalGate`]. See
    /// [`PARENT_GUARDRAILS`] for scope semantics.
    pub static PARENT_APPROVAL: ApprovalGate;
}

/// Snapshot of the parent's policy as observed inside a tool's `exec`.
/// Returns `(None, None)` when called outside a `dispatch_tool` scope
/// (e.g. unit tests that call `Tool::exec` directly).
pub fn current_parent_policy() -> (Option<Guardrails>, Option<ApprovalGate>) {
    let g = PARENT_GUARDRAILS.try_with(|g| g.clone()).ok();
    let a = PARENT_APPROVAL.try_with(|a| a.clone()).ok();
    (g, a)
}

/// Read the current delegate depth. Returns 0 outside any delegate scope.
pub fn current_depth() -> u32 {
    DELEGATE_DEPTH.try_with(|d| *d).ok().unwrap_or(0)
}

/// `cos_delegate` tool — spawn a child agent with a scoped tool subset.
pub struct Delegate;

#[derive(Debug, Deserialize)]
struct DelegateInput {
    /// Task instructions handed to the child as its user message.
    task: String,

    /// Names of tools the child is permitted to call. Required (may be
    /// empty for pure-LLM tasks). `cos_delegate` is silently filtered out
    /// even if the parent listed it — the parent must reduce depth via
    /// `max_depth` rather than re-listing the tool.
    #[serde(default)]
    allowed_tools: Vec<String>,

    /// Optional provider override (e.g. `"openai"`, `"anthropic"`,
    /// `"gemini"`). Defaults to the parent agent's provider.
    #[serde(default)]
    provider: Option<String>,

    /// Optional model override. Defaults to the parent agent's model.
    #[serde(default)]
    model: Option<String>,

    /// Optional `max_turns` for the child. Capped at [`HARD_MAX_TURNS`].
    #[serde(default)]
    max_turns: Option<u32>,

    /// Optional `max_depth` ceiling. Capped at [`HARD_MAX_DEPTH`]. A
    /// caller-supplied `max_depth` lower than the current depth + 1
    /// causes the tool to refuse before launching the child.
    #[serde(default)]
    max_depth: Option<u32>,

    /// Optional per-call timeout in seconds.
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[async_trait]
impl Tool for Delegate {
    fn name(&self) -> &'static str {
        "cos_delegate"
    }

    fn description(&self) -> &'static str {
        "Spawn a child sub-agent with a scoped subset of tools and an \
         optional model override. Returns the child's final answer. Use \
         this for focused sub-tasks (research, extraction, summarisation) \
         that you only need the final result of."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Instructions for the child agent. Becomes the child's user message."
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Action-tool names the child may call. Empty leaves only read-only cos_skill when parent guardrails permit it. cos_delegate is always filtered out to prevent recursion."
                },
                "provider": {
                    "type": "string",
                    "description": "Optional provider override (e.g. 'openai', 'anthropic', 'gemini'). Defaults to parent's."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override. Defaults to parent's."
                },
                "max_turns": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": HARD_MAX_TURNS,
                    "description": "Maximum turns the child may take. Capped at 50."
                },
                "max_depth": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": HARD_MAX_DEPTH,
                    "description": "Maximum nesting depth for further delegation. Capped at 5."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Per-call timeout in seconds. Defaults to 600."
                }
            },
            "required": ["task", "allowed_tools"]
        })
    }

    async fn exec(&self, input: serde_json::Value) -> ToolResult {
        let parsed: DelegateInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("invalid delegate input: {e}")),
        };

        let parent_cfg = crate::config::get().agent.clone();
        run_delegate(parsed, &parent_cfg, registry_factory).await
    }
}

/// Allow tests to inject a custom registry instead of opening the real
/// default_registry (which does Disk I/O for the FTS5 DB).
type RegistryFactory = fn() -> ToolRegistry;

fn registry_factory() -> ToolRegistry {
    default_registry()
}

/// Core delegate logic. Split out from `Tool::exec` so tests can drive it
/// without going through `serde_json::Value`.
async fn run_delegate(
    input: DelegateInput,
    parent_cfg: &AgentConfig,
    factory: RegistryFactory,
) -> ToolResult {
    let cur = current_depth();
    let requested_max_depth = input
        .max_depth
        .unwrap_or(DEFAULT_MAX_DEPTH)
        .min(HARD_MAX_DEPTH);

    if cur + 1 > requested_max_depth {
        return ToolResult::err(format!(
            "delegate depth limit reached: current={cur} max={requested_max_depth}; \
             refusing further delegation"
        ));
    }

    let explicit_provider = input.provider.is_some();
    let provider_name = input
        .provider
        .clone()
        .unwrap_or_else(|| parent_cfg.provider.clone());
    let model = input
        .model
        .clone()
        .unwrap_or_else(|| parent_cfg.model.clone());

    let child_cfg = AgentConfig {
        provider: provider_name.clone(),
        model: model.clone(),
        max_turns: input
            .max_turns
            .unwrap_or(parent_cfg.max_turns)
            .clamp(1, HARD_MAX_TURNS),
        max_tokens: parent_cfg.max_tokens,
        temperature: parent_cfg.temperature,
        // Child uses a clean system prompt — no MEMORY.md / USER.md
        // injection, since the child has a fresh, isolated trajectory and
        // those are properties of the parent's session.
        system_prompt_path: None,
        ..parent_cfg.clone()
    };

    let provider: Arc<dyn Provider> = if explicit_provider {
        match llm::registry::build(&provider_name, &model, &child_cfg) {
            Ok(provider) => crate::ai::gate::wrap_for_system(provider),
            Err(error) => {
                return ToolResult::err(format!(
                    "failed to build provider '{provider_name}' for delegate: {error}"
                ));
            }
        }
    } else {
        match crate::ai::gate::build_system_provider(&child_cfg) {
            Ok(provider) => provider,
            Err(error) => {
                return ToolResult::err(format!(
                    "failed to build provider chain for delegate: {error}"
                ));
            }
        }
    };

    let (parent_guardrails, parent_approval) = current_parent_policy();
    let tools = build_child_registry(
        parent_guardrails.as_ref(),
        parent_approval.as_ref(),
        factory(),
        &input.allowed_tools,
    );

    let timeout = Duration::from_secs(input.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
    let task = input.task.clone();

    let child_future = DELEGATE_DEPTH.scope(cur + 1, async move {
        loop_::ask_with(provider, &child_cfg, &task, &tools).await
    });

    let outcome = tokio::time::timeout(timeout, child_future).await;
    match outcome {
        Ok(Ok(result)) => ToolResult::ok(format_result(&result)),
        Ok(Err(e)) => ToolResult::err(format!("delegate child failed: {e}")),
        Err(_) => ToolResult::err(format!("delegate timed out after {}s", timeout.as_secs())),
    }
}

/// Build a registry that contains only the tools the parent allow-listed,
/// plus read-only progressive Skill disclosure when the parent permits it.
/// `cos_delegate` is always filtered out (depth-counter handles recursion;
/// we additionally never pass the tool to children to keep the surface
/// minimal). Unknown names are silently dropped — the child sees only the
/// tools that actually exist.
///
/// The child registry **inherits the parent's `Guardrails` and
/// `ApprovalGate`**. Without this, every delegate ran with the
/// default permissive guardrails + a default no-op approver — meaning
/// any tool the parent had a deny rule for, or any tool that required
/// human approval, became unguarded as soon as it crossed the
/// delegate boundary. The agent's blast-radius advertised model
/// (parent picks tools, parent's deny/approval policy follows them)
/// only works if the child's registry inherits these.
///
/// We also re-apply the `allowed_tools` filter on top of the parent's
/// guardrails by inserting `allowed` into the child's `allow` set —
/// the net effect is the intersection of "parent allowed" ∩ "this
/// delegate call's allowed_tools".
fn build_child_registry(
    parent_guardrails: Option<&Guardrails>,
    parent_approval: Option<&ApprovalGate>,
    source: ToolRegistry,
    allowed: &[String],
) -> ToolRegistry {
    let mut child = ToolRegistry::new();
    // Inherit parent's policy primitives. These are Clone (Guardrails
    // is a pair of sets; ApprovalGate wraps everything in Arcs).
    if let Some(g) = parent_guardrails {
        child.set_guardrails(g.clone());
    }
    if let Some(a) = parent_approval {
        child.set_approval(a.clone());
    }

    if parent_guardrails
        .map(|guardrails| guardrails.permits("cos_skill"))
        .unwrap_or(true)
    {
        if let Some(tool) = source.get_unfiltered("cos_skill") {
            child.register(tool);
        }
    }

    for name in allowed {
        if matches!(
            name.as_str(),
            "cos_delegate" | "cos_skill" | "cos_oauth_login"
        ) {
            continue;
        }
        // Honour the parent's deny list — even if the caller asked
        // for a tool, if the parent denied it the child shouldn't
        // see it either.
        if let Some(g) = parent_guardrails {
            if !g.permits(name) {
                continue;
            }
        }
        if let Some(tool) = source.get_unfiltered(name) {
            child.register(tool);
        }
    }
    child
}

fn format_result(result: &AskResult) -> String {
    format!(
        "delegate finished (provider={}, model={}, turns={}, evidence={}, degraded={})\n---\n{}",
        result.provider,
        result.model,
        result.turns,
        result.evidence.status.as_str(),
        result
            .fallback
            .as_ref()
            .is_some_and(|fallback| fallback.degraded),
        result.answer
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/delegate.rs"
    ));
}
