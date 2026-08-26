//! Agent runtime hooks — pluggable callbacks that fire around
//! every turn and every tool dispatch.
//!
//! ## What hooks let you do
//!
//! Anything that should happen *consistently* before or after a
//! model call or tool call:
//!
//!   * Audit / structured logging (`pre_turn` records who, when,
//!     which provider; `post_turn` records latency, token usage,
//!     interrupt status).
//!   * Redaction-on-emit (`post_tool` scrubs paths in stderr before
//!     the result is shown to the model).
//!   * Prompt-cache marker injection (`pre_turn` decides cache
//!     boundaries; today this is inlined in the loop, but the hook
//!     is the migration target).
//!   * Per-tool consent gating that's distinct from the existing
//!     [`approval`](super::approval) flow — e.g. "if `cos_shell`
//!     touches `/etc/`, require explicit approval even if
//!     auto-approved by name".
//!   * External telemetry (Langfuse / OpenTelemetry export at
//!     turn boundaries).
//!
//! ## Design
//!
//! Hooks are *advisory* by default: they observe and may log, but
//! cannot mutate the in-flight request. The two exceptions are:
//!
//!   * `pre_tool` returns a [`ToolDecision`], which can `Allow`,
//!     `Deny(reason)`, or `Override(new_input)` a tool call. This
//!     is the ONE point of mutation; everything else is observe-only.
//!   * `post_turn` / `post_tool` return [`HookOutcome::Stop`] to
//!     request that the loop unwind cleanly between turns
//!     (interpreted as an interrupt at the next turn boundary).
//!
//! The default `Hook` impl is no-op for every method, so a hook
//! that only cares about `post_turn` need not implement the others.
//!
//! ## Concurrency
//!
//! [`HookRegistry`] holds `Arc<dyn Hook>` and is `Clone + Send +
//! Sync`. Hook impls themselves must be `Send + Sync`. The registry
//! is process-wide, accessible via [`global_registry`], or a custom
//! one can be passed to the runtime if/when the loop accepts a
//! per-call registry. (Currently the runtime calls only into the
//! global one — the per-call escape hatch lives here for future
//! agent-in-agent scenarios.)
//!
//! ## What this module is NOT
//!
//! This file is the *trait surface and registry*. It deliberately
//! avoids any opinion about *which* hooks should be on by default
//! (audit / redact / etc. live in their own modules and register
//! themselves at startup if enabled). The runtime calls
//! [`HookRegistry::dispatch_pre_turn`] etc. at the right places;
//! everything else is a hook implementer's call.

use std::sync::{Arc, OnceLock, RwLock};

use crate::agent::llm::types::ToolCall;

// =====================================================================
// Hook context — what the runtime hands the hook on every call
// =====================================================================

/// Snapshot of the agent state a hook sees on every callback.
/// The runtime fills this in. Hooks must treat it as read-only.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub session_id: String,
    /// 0-indexed turn within this session's run.
    pub turn_index: u32,
    pub provider: String,
    pub model: String,
    /// Wall-clock instant the turn started, in milliseconds since
    /// the Unix epoch. Hooks compute durations as `now() - started_at_ms`.
    pub started_at_ms: u64,
    /// True when the loop is acting as a delegated child agent.
    pub is_delegated: bool,
}

impl HookContext {
    /// Builder-style starter; runtime callers fill in the rest.
    pub fn new(
        session_id: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            turn_index: 0,
            provider: provider.into(),
            model: model.into(),
            started_at_ms: now_ms(),
            is_delegated: false,
        }
    }

    pub fn with_turn_index(mut self, n: u32) -> Self {
        self.turn_index = n;
        self
    }

    pub fn with_started_at_ms(mut self, t: u64) -> Self {
        self.started_at_ms = t;
        self
    }

    pub fn with_delegated(mut self, b: bool) -> Self {
        self.is_delegated = b;
        self
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// =====================================================================
// Hook outcomes
// =====================================================================

/// What a non-mutating hook is allowed to ask the runtime to do
/// after observing an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// All clear; continue.
    Continue,
    /// Request that the loop unwind cleanly at the next turn
    /// boundary. Carries a reason that the runtime threads through
    /// to whatever produces the loop's exit status.
    Stop(String),
}

impl HookOutcome {
    pub fn is_stop(&self) -> bool {
        matches!(self, Self::Stop(_))
    }
}

/// Outcome of a `pre_tool` hook — allowed to mutate the call's
/// input or veto it outright.
#[derive(Debug, Clone)]
pub enum ToolDecision {
    /// Run the tool as the model requested.
    Allow,
    /// Refuse the tool call. The runtime will surface `reason` to
    /// the model as the `tool_result` body so the model can
    /// gracefully recover.
    Deny(String),
    /// Run the tool, but with a different input than the model
    /// asked for. The runtime substitutes the JSON value into the
    /// dispatch path.
    Override(serde_json::Value),
}

impl ToolDecision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }
    pub fn is_deny(&self) -> bool {
        matches!(self, Self::Deny(_))
    }
}

/// Compact summary of a finished turn — what `post_turn` sees.
#[derive(Debug, Clone, Default)]
pub struct TurnSummary {
    pub success: bool,
    pub latency_ms: u64,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Prompt-cache hit count (Anthropic / OpenAI cached input).
    /// Carried forward from `Usage::cache_read_tokens` so observers
    /// can monitor cache effectiveness without re-deriving.
    pub cache_read_tokens: u32,
    /// Prompt-cache write count — bytes the provider just stored
    /// for future reuse.
    pub cache_write_tokens: u32,
    pub stop_reason: String,
    pub tool_calls_made: u32,
    pub error: Option<String>,
}

/// Compact summary of a finished tool call — what `post_tool` sees.
#[derive(Debug, Clone)]
pub struct ToolResultSummary {
    pub tool_name: String,
    pub success: bool,
    pub latency_ms: u64,
    pub bytes_returned: usize,
    pub error: Option<String>,
}

// =====================================================================
// The trait
// =====================================================================

/// A hook that can observe (and in some cases steer) agent
/// execution. Every method has a no-op default; implementers
/// override only what they care about.
///
/// All methods are sync. Hooks should be cheap; if you need to do
/// something blocking or async, dispatch it onto a tokio task and
/// return immediately.
pub trait Hook: Send + Sync {
    /// Stable identifier for this hook (used by registry list
    /// commands and audit log). Convention: kebab-case.
    fn name(&self) -> &str;

    /// Fired once at the top of every turn, before the model is
    /// called. May NOT mutate the request. May return
    /// `HookOutcome::Stop` to ask the loop to abort cleanly.
    fn pre_turn(&self, _ctx: &HookContext) -> HookOutcome {
        HookOutcome::Continue
    }

    /// Fired once after every turn's model call returns (including
    /// after errors — `summary.success` indicates which).
    fn post_turn(&self, _ctx: &HookContext, _summary: &TurnSummary) -> HookOutcome {
        HookOutcome::Continue
    }

    /// Fired before each tool dispatch. The default returns
    /// `ToolDecision::Allow`; overrides can deny or substitute the
    /// input.
    fn pre_tool(&self, _ctx: &HookContext, _tool_call: &ToolCall) -> ToolDecision {
        ToolDecision::Allow
    }

    /// Fired after each tool dispatch finishes (success or error).
    fn post_tool(
        &self,
        _ctx: &HookContext,
        _tool_call: &ToolCall,
        _result: &ToolResultSummary,
    ) -> HookOutcome {
        HookOutcome::Continue
    }
}

// =====================================================================
// Registry
// =====================================================================

/// Process-wide (or scoped) collection of hooks.
///
/// Registration is idempotent by `name()` — re-registering a hook
/// of the same name replaces the prior one. Dispatch order matches
/// registration order.
#[derive(Clone, Default)]
pub struct HookRegistry {
    inner: Arc<RwLock<Vec<Arc<dyn Hook>>>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of currently-registered hooks.
    pub fn len(&self) -> usize {
        self.inner.read().map(|v| v.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn names(&self) -> Vec<String> {
        match self.inner.read() {
            Ok(v) => v.iter().map(|h| h.name().to_string()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Add or replace a hook by name. Returns true if a prior hook
    /// with the same name was replaced.
    pub fn register(&self, hook: Arc<dyn Hook>) -> bool {
        let name = hook.name().to_string();
        let mut guard = match self.inner.write() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if let Some(idx) = guard.iter().position(|h| h.name() == name) {
            guard[idx] = hook;
            true
        } else {
            guard.push(hook);
            false
        }
    }

    /// Remove a hook by name. Returns true if a hook was removed.
    pub fn unregister(&self, name: &str) -> bool {
        let mut guard = match self.inner.write() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let before = guard.len();
        guard.retain(|h| h.name() != name);
        guard.len() != before
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.write() {
            g.clear();
        }
    }

    /// Snapshot of the current hook list. Used to dispatch without
    /// holding the read lock across hook callbacks (so a hook is
    /// free to mutate the registry — though that's not recommended).
    fn snapshot(&self) -> Vec<Arc<dyn Hook>> {
        self.inner.read().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn dispatch_pre_turn(&self, ctx: &HookContext) -> HookOutcome {
        for h in self.snapshot() {
            let out = h.pre_turn(ctx);
            if out.is_stop() {
                return out;
            }
        }
        HookOutcome::Continue
    }

    pub fn dispatch_post_turn(&self, ctx: &HookContext, summary: &TurnSummary) -> HookOutcome {
        for h in self.snapshot() {
            let out = h.post_turn(ctx, summary);
            if out.is_stop() {
                return out;
            }
        }
        HookOutcome::Continue
    }

    /// Dispatch `pre_tool` across all hooks. The first non-`Allow`
    /// decision wins (deny stops the chain; override is also
    /// terminal — later hooks don't get to override the override).
    pub fn dispatch_pre_tool(&self, ctx: &HookContext, tool_call: &ToolCall) -> ToolDecision {
        for h in self.snapshot() {
            match h.pre_tool(ctx, tool_call) {
                ToolDecision::Allow => continue,
                non_allow => return non_allow,
            }
        }
        ToolDecision::Allow
    }

    pub fn dispatch_post_tool(
        &self,
        ctx: &HookContext,
        tool_call: &ToolCall,
        result: &ToolResultSummary,
    ) -> HookOutcome {
        for h in self.snapshot() {
            let out = h.post_tool(ctx, tool_call, result);
            if out.is_stop() {
                return out;
            }
        }
        HookOutcome::Continue
    }
}

// =====================================================================
// Global registry
// =====================================================================

static GLOBAL: OnceLock<HookRegistry> = OnceLock::new();

/// Process-wide hook registry. Lazily initialized on first access.
/// Tests that need an isolated registry should create a local
/// [`HookRegistry`] instead of touching this one.
pub fn global_registry() -> HookRegistry {
    GLOBAL.get_or_init(HookRegistry::new).clone()
}

// =====================================================================
// Reference impl — log every hook event to tracing
// =====================================================================

/// Reference hook that emits `tracing::info!` events for each
/// callback. Useful for development and as a template for new
/// hooks. Not registered by default.
#[derive(Debug, Default)]
pub struct LoggingHook;

impl Hook for LoggingHook {
    fn name(&self) -> &str {
        "logging"
    }

    fn pre_turn(&self, ctx: &HookContext) -> HookOutcome {
        tracing::debug!(
            target: "agent.hooks",
            session_id = %ctx.session_id,
            turn = ctx.turn_index,
            provider = %ctx.provider,
            model = %ctx.model,
            "pre_turn"
        );
        HookOutcome::Continue
    }

    fn post_turn(&self, ctx: &HookContext, summary: &TurnSummary) -> HookOutcome {
        tracing::info!(
            target: "agent.hooks",
            session_id = %ctx.session_id,
            turn = ctx.turn_index,
            success = summary.success,
            latency_ms = summary.latency_ms,
            input_tokens = summary.input_tokens,
            output_tokens = summary.output_tokens,
            cache_read_tokens = summary.cache_read_tokens,
            cache_write_tokens = summary.cache_write_tokens,
            tool_calls = summary.tool_calls_made,
            "post_turn"
        );
        HookOutcome::Continue
    }

    fn pre_tool(&self, ctx: &HookContext, tool_call: &ToolCall) -> ToolDecision {
        tracing::debug!(
            target: "agent.hooks",
            session_id = %ctx.session_id,
            tool = %tool_call.name,
            id = %tool_call.id,
            "pre_tool"
        );
        ToolDecision::Allow
    }

    fn post_tool(
        &self,
        ctx: &HookContext,
        tool_call: &ToolCall,
        result: &ToolResultSummary,
    ) -> HookOutcome {
        tracing::info!(
            target: "agent.hooks",
            session_id = %ctx.session_id,
            tool = %tool_call.name,
            id = %tool_call.id,
            success = result.success,
            latency_ms = result.latency_ms,
            bytes = result.bytes_returned,
            "post_tool"
        );
        HookOutcome::Continue
    }
}

// =====================================================================
// Reference impl — append every hook event as JSONL to a file
// =====================================================================

/// Hook that appends a structured JSONL audit event for every
/// `pre_turn` / `post_turn` / `pre_tool` / `post_tool` callback.
///
/// Default destination is [`crate::paths::agent_audit_log_path()`]
/// (`<log_dir>/agent.jsonl`). Use [`AuditHook::at`] to point at a
/// custom path — useful for tests and for routing per-session
/// audit streams.
///
/// Schema per event:
///
/// ```json
/// {
///   "timestamp": "2026-...Z",   // auto-injected if absent
///   "kind":      "pre_turn" | "post_turn" | "pre_tool" | "post_tool",
///   "session_id": "...",
///   "turn":      N,
///   "provider":  "...",         // pre/post_turn only
///   "model":     "...",         // pre/post_turn only
///   "tool_call_id": "...",      // pre/post_tool only
///   "tool_name":    "...",      // pre/post_tool only
///   "success":      bool,       // post_* only
///   "latency_ms":   N,          // post_* only
///   "chain_version": 1,
///   "chain_id":      "...",
///   "sequence":      N,
///   "prev_hash":     "...",
///   "this_hash":     "...",
///   ...
/// }
/// ```
///
/// Audit writes are best-effort and never abort the agent loop. Failures are
/// traced. Each event is chained under an exclusive file lock so `cos agent
/// audit verify` can detect accidental or naive edits, gaps, malformed tails,
/// and archive changes. The chain is unkeyed; it is not a signature against an
/// attacker who can rewrite the complete log and head.
#[derive(Debug, Clone)]
pub struct AuditHook {
    audit_path: std::path::PathBuf,
}

impl Default for AuditHook {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditHook {
    /// Create an `AuditHook` writing to the canonical
    /// `<log_dir>/agent.jsonl` location.
    pub fn new() -> Self {
        Self {
            audit_path: crate::paths::agent_audit_log_path(),
        }
    }

    /// Create an `AuditHook` writing to a caller-supplied path.
    pub fn at(audit_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            audit_path: audit_path.into(),
        }
    }

    /// The path this hook writes to. Useful for tests.
    pub fn audit_path(&self) -> &std::path::Path {
        &self.audit_path
    }
}

impl Hook for AuditHook {
    fn name(&self) -> &str {
        "audit"
    }

    fn pre_turn(&self, ctx: &HookContext) -> HookOutcome {
        crate::audit::log_chained_event(
            &self.audit_path,
            serde_json::json!({
                "kind": "pre_turn",
                "session_id": ctx.session_id,
                "turn": ctx.turn_index,
                "provider": ctx.provider,
                "model": ctx.model,
                "is_delegated": ctx.is_delegated,
                "started_at_ms": ctx.started_at_ms,
            }),
        );
        HookOutcome::Continue
    }

    fn post_turn(&self, ctx: &HookContext, summary: &TurnSummary) -> HookOutcome {
        crate::audit::log_chained_event(
            &self.audit_path,
            serde_json::json!({
                "kind": "post_turn",
                "session_id": ctx.session_id,
                "turn": ctx.turn_index,
                "provider": ctx.provider,
                "model": ctx.model,
                "is_delegated": ctx.is_delegated,
                "success": summary.success,
                "stop_reason": summary.stop_reason,
                "latency_ms": summary.latency_ms,
                "input_tokens": summary.input_tokens,
                "output_tokens": summary.output_tokens,
                "cache_read_tokens": summary.cache_read_tokens,
                "cache_write_tokens": summary.cache_write_tokens,
                "tool_calls_made": summary.tool_calls_made,
                "error": summary.error,
            }),
        );
        HookOutcome::Continue
    }

    fn pre_tool(&self, ctx: &HookContext, tool_call: &ToolCall) -> ToolDecision {
        crate::audit::log_chained_event(
            &self.audit_path,
            serde_json::json!({
                "kind": "pre_tool",
                "session_id": ctx.session_id,
                "turn": ctx.turn_index,
                "tool_call_id": tool_call.id,
                "tool_name": tool_call.name,
            }),
        );
        ToolDecision::Allow
    }

    fn post_tool(
        &self,
        ctx: &HookContext,
        tool_call: &ToolCall,
        result: &ToolResultSummary,
    ) -> HookOutcome {
        crate::audit::log_chained_event(
            &self.audit_path,
            serde_json::json!({
                "kind": "post_tool",
                "session_id": ctx.session_id,
                "turn": ctx.turn_index,
                "tool_call_id": tool_call.id,
                "tool_name": tool_call.name,
                "success": result.success,
                "latency_ms": result.latency_ms,
                "bytes_returned": result.bytes_returned,
                "error": result.error,
            }),
        );
        HookOutcome::Continue
    }
}

// =====================================================================
// CheckpointHook (reference impl)
// =====================================================================

/// Strategy for how a [`CheckpointHook`] actually creates a checkpoint.
///
/// Production code uses [`ProductionCheckpointCreator`], which calls
/// [`crate::checkpoint::run`]`("create", ...)`. Tests inject a fake
/// creator so the hook's dispatch logic can be exercised without
/// touching real overlay state.
pub trait CheckpointCreator: Send + Sync + std::fmt::Debug {
    /// Create a checkpoint with the given description.
    /// Returns the new checkpoint id on success.
    fn create(&self, description: &str) -> Result<String, String>;
}

/// Production [`CheckpointCreator`] backed by [`crate::checkpoint::run`].
///
/// On Linux with overlayfs, this performs a real filesystem-level
/// snapshot (cheap CoW). On other platforms, the underlying
/// `checkpoint::run` may fail (e.g. overlayfs not mounted) — failure
/// is propagated to the hook, which logs it to the audit trail
/// without aborting the agent.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProductionCheckpointCreator;

impl CheckpointCreator for ProductionCheckpointCreator {
    fn create(&self, description: &str) -> Result<String, String> {
        let v = crate::checkpoint::run("create", &[description.to_string()])?;
        v.get("id")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "checkpoint::run returned no id".to_string())
    }
}

/// Default conservative dangerous-tool list.
///
/// These are the tool names whose effects are most likely to mutate
/// overlay-protected state (filesystem under the workspace overlay,
/// or system-level resources we want a rollback marker for).
///
/// The list intentionally errs on the side of *snapshot more, not
/// less* — a checkpoint is cheap on overlayfs, expensive only in
/// directory churn. Operators who want a tighter set can construct
/// `CheckpointHook` with their own list via [`CheckpointHook::with_dangerous`].
pub fn default_dangerous_tools() -> std::collections::HashSet<String> {
    [
        "cos_sandbox",
        "cos_proc",
        "cos_credential",
        "cos_oauth_login",
        "cos_cron",
        "cos_netfilter",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Hook that creates a [`crate::checkpoint`] before any tool call
/// whose name is in its dangerous-tools set.
///
/// The hook's behaviour:
///
///   * `pre_tool` checks `tool_call.name` against the dangerous set.
///     If it's not in the set, returns [`ToolDecision::Allow`]
///     immediately (no checkpoint, no audit entry).
///   * If it *is* in the set, calls `creator.create(...)` with a
///     description like
///     `"agent_pre_tool: <tool_name> session=<sid> turn=<n>"`.
///   * Whether the checkpoint succeeded or failed, writes a JSONL
///     event (`kind: "pre_tool_checkpoint"`) to the audit log
///     containing tool_name, the description, and either
///     `checkpoint_id` (on success) or `error` (on failure).
///   * Always returns [`ToolDecision::Allow`] — the hook does *not*
///     gate execution. A failed checkpoint is best-effort safety,
///     not a hard precondition. (Operators who want hard gating
///     should use [`super::approval`] instead.)
///
/// `pre_turn`, `post_turn`, and `post_tool` are no-ops — the hook
/// only fires around dangerous tool calls, not around every turn.
#[derive(Debug)]
pub struct CheckpointHook {
    audit_path: std::path::PathBuf,
    dangerous: std::collections::HashSet<String>,
    creator: std::sync::Arc<dyn CheckpointCreator>,
}

impl CheckpointHook {
    /// Production constructor: real `crate::checkpoint::run` creator,
    /// canonical `<log_dir>/agent.jsonl` audit path, default
    /// dangerous-tools list.
    pub fn new() -> Self {
        Self {
            audit_path: crate::paths::agent_audit_log_path(),
            dangerous: default_dangerous_tools(),
            creator: std::sync::Arc::new(ProductionCheckpointCreator),
        }
    }

    /// Same as [`Self::new`] but with a custom dangerous-tools set.
    pub fn with_dangerous(dangerous: std::collections::HashSet<String>) -> Self {
        Self {
            audit_path: crate::paths::agent_audit_log_path(),
            dangerous,
            creator: std::sync::Arc::new(ProductionCheckpointCreator),
        }
    }

    /// Fully-injected constructor for tests:
    /// caller-supplied creator, audit path, and dangerous set.
    pub fn with_overrides(
        creator: std::sync::Arc<dyn CheckpointCreator>,
        audit_path: impl Into<std::path::PathBuf>,
        dangerous: std::collections::HashSet<String>,
    ) -> Self {
        Self {
            audit_path: audit_path.into(),
            dangerous,
            creator,
        }
    }

    /// Returns true iff this hook would create a checkpoint for a
    /// tool call with the given name.
    pub fn is_dangerous(&self, tool_name: &str) -> bool {
        self.dangerous.contains(tool_name)
    }

    /// The audit path this hook writes to. Useful for tests.
    pub fn audit_path(&self) -> &std::path::Path {
        &self.audit_path
    }
}

impl Default for CheckpointHook {
    fn default() -> Self {
        Self::new()
    }
}

impl Hook for CheckpointHook {
    fn name(&self) -> &str {
        "checkpoint"
    }

    fn pre_tool(&self, ctx: &HookContext, tool_call: &ToolCall) -> ToolDecision {
        if !self.dangerous.contains(&tool_call.name) {
            return ToolDecision::Allow;
        }
        let description = format!(
            "agent_pre_tool: {} session={} turn={}",
            tool_call.name, ctx.session_id, ctx.turn_index
        );
        match self.creator.create(&description) {
            Ok(checkpoint_id) => {
                crate::audit::log_chained_event(
                    &self.audit_path,
                    serde_json::json!({
                        "kind": "pre_tool_checkpoint",
                        "session_id": ctx.session_id,
                        "turn": ctx.turn_index,
                        "tool_call_id": tool_call.id,
                        "tool_name": tool_call.name,
                        "description": description,
                        "checkpoint_id": checkpoint_id,
                        "status": "ok",
                    }),
                );
            }
            Err(error) => {
                crate::audit::log_chained_event(
                    &self.audit_path,
                    serde_json::json!({
                        "kind": "pre_tool_checkpoint",
                        "session_id": ctx.session_id,
                        "turn": ctx.turn_index,
                        "tool_call_id": tool_call.id,
                        "tool_name": tool_call.name,
                        "description": description,
                        "error": error,
                        "status": "error",
                    }),
                );
            }
        }
        ToolDecision::Allow
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/hooks.rs"
    ));
}
