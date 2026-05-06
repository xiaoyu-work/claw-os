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
    pub fn new(session_id: impl Into<String>, provider: impl Into<String>, model: impl Into<String>) -> Self {
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
        self.inner
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
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
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn sample_tool_call() -> ToolCall {
        ToolCall {
            id: "call_1".to_string(),
            name: "echo".to_string(),
            input: serde_json::json!({"text": "hi"}),
        }
    }

    /// Counts every callback so tests can assert dispatch order +
    /// frequency.
    #[derive(Default)]
    struct CountingHook {
        name: String,
        pre_turn: AtomicUsize,
        post_turn: AtomicUsize,
        pre_tool: AtomicUsize,
        post_tool: AtomicUsize,
    }

    impl Hook for CountingHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn pre_turn(&self, _ctx: &HookContext) -> HookOutcome {
            self.pre_turn.fetch_add(1, Ordering::SeqCst);
            HookOutcome::Continue
        }
        fn post_turn(&self, _ctx: &HookContext, _summary: &TurnSummary) -> HookOutcome {
            self.post_turn.fetch_add(1, Ordering::SeqCst);
            HookOutcome::Continue
        }
        fn pre_tool(&self, _ctx: &HookContext, _t: &ToolCall) -> ToolDecision {
            self.pre_tool.fetch_add(1, Ordering::SeqCst);
            ToolDecision::Allow
        }
        fn post_tool(
            &self,
            _ctx: &HookContext,
            _t: &ToolCall,
            _r: &ToolResultSummary,
        ) -> HookOutcome {
            self.post_tool.fetch_add(1, Ordering::SeqCst);
            HookOutcome::Continue
        }
    }

    fn ctx() -> HookContext {
        HookContext::new("sess-1", "mock", "mock-model")
    }

    fn turn_summary_ok() -> TurnSummary {
        TurnSummary {
            success: true,
            latency_ms: 42,
            input_tokens: 10,
            output_tokens: 5,
            stop_reason: "Stop".into(),
            tool_calls_made: 0,
            error: None,
        }
    }

    fn tool_result_ok() -> ToolResultSummary {
        ToolResultSummary {
            tool_name: "echo".into(),
            success: true,
            latency_ms: 1,
            bytes_returned: 12,
            error: None,
        }
    }

    // ---- HookContext --------------------------------------------------

    #[test]
    fn hook_context_builder_sets_fields() {
        let c = HookContext::new("s", "p", "m")
            .with_turn_index(7)
            .with_started_at_ms(1_000)
            .with_delegated(true);
        assert_eq!(c.session_id, "s");
        assert_eq!(c.provider, "p");
        assert_eq!(c.model, "m");
        assert_eq!(c.turn_index, 7);
        assert_eq!(c.started_at_ms, 1_000);
        assert!(c.is_delegated);
    }

    #[test]
    fn hook_context_default_started_at_is_recent() {
        let before = now_ms();
        let c = HookContext::new("s", "p", "m");
        let after = now_ms();
        assert!(c.started_at_ms >= before);
        assert!(c.started_at_ms <= after);
    }

    // ---- Outcomes -----------------------------------------------------

    #[test]
    fn hook_outcome_is_stop_predicate() {
        assert!(!HookOutcome::Continue.is_stop());
        assert!(HookOutcome::Stop("interrupt".into()).is_stop());
    }

    #[test]
    fn tool_decision_predicates() {
        assert!(ToolDecision::Allow.is_allow());
        assert!(!ToolDecision::Allow.is_deny());
        assert!(ToolDecision::Deny("nope".into()).is_deny());
        assert!(!ToolDecision::Override(serde_json::json!({})).is_allow());
    }

    // ---- Default trait impls ------------------------------------------

    /// A hook that overrides only `name()` should still get default
    /// no-op behaviour from the rest.
    struct NameOnly;
    impl Hook for NameOnly {
        fn name(&self) -> &str {
            "name-only"
        }
    }

    #[test]
    fn default_hook_methods_are_noop_continue() {
        let h = NameOnly;
        assert_eq!(h.pre_turn(&ctx()), HookOutcome::Continue);
        assert_eq!(h.post_turn(&ctx(), &turn_summary_ok()), HookOutcome::Continue);
        assert!(h.pre_tool(&ctx(), &sample_tool_call()).is_allow());
        assert_eq!(
            h.post_tool(&ctx(), &sample_tool_call(), &tool_result_ok()),
            HookOutcome::Continue
        );
    }

    // ---- Registry -----------------------------------------------------

    #[test]
    fn registry_starts_empty() {
        let r = HookRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(r.names().is_empty());
    }

    #[test]
    fn registry_register_appends_and_returns_false_for_new() {
        let r = HookRegistry::new();
        let h = Arc::new(CountingHook {
            name: "a".into(),
            ..Default::default()
        });
        let replaced = r.register(h);
        assert!(!replaced);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn registry_register_replaces_by_name_and_returns_true() {
        let r = HookRegistry::new();
        let h1 = Arc::new(CountingHook {
            name: "a".into(),
            ..Default::default()
        });
        let h2 = Arc::new(CountingHook {
            name: "a".into(),
            ..Default::default()
        });
        assert!(!r.register(h1));
        assert!(r.register(h2));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn registry_unregister_returns_true_when_removed() {
        let r = HookRegistry::new();
        let h = Arc::new(CountingHook {
            name: "a".into(),
            ..Default::default()
        });
        r.register(h);
        assert!(r.unregister("a"));
        assert!(!r.unregister("a")); // already gone
        assert!(r.is_empty());
    }

    #[test]
    fn registry_clear_drops_all() {
        let r = HookRegistry::new();
        for n in &["a", "b", "c"] {
            r.register(Arc::new(CountingHook {
                name: (*n).into(),
                ..Default::default()
            }));
        }
        assert_eq!(r.len(), 3);
        r.clear();
        assert!(r.is_empty());
    }

    #[test]
    fn registry_names_preserves_order() {
        let r = HookRegistry::new();
        for n in &["c", "a", "b"] {
            r.register(Arc::new(CountingHook {
                name: (*n).into(),
                ..Default::default()
            }));
        }
        assert_eq!(r.names(), vec!["c", "a", "b"]);
    }

    // ---- Dispatch -----------------------------------------------------

    #[test]
    fn dispatch_pre_turn_hits_every_hook_when_all_continue() {
        let r = HookRegistry::new();
        let h1 = Arc::new(CountingHook {
            name: "a".into(),
            ..Default::default()
        });
        let h2 = Arc::new(CountingHook {
            name: "b".into(),
            ..Default::default()
        });
        r.register(h1.clone());
        r.register(h2.clone());

        let outcome = r.dispatch_pre_turn(&ctx());
        assert_eq!(outcome, HookOutcome::Continue);
        assert_eq!(h1.pre_turn.load(Ordering::SeqCst), 1);
        assert_eq!(h2.pre_turn.load(Ordering::SeqCst), 1);
    }

    /// First-stop-wins: when a hook returns Stop, later hooks
    /// should NOT be called.
    #[test]
    fn dispatch_pre_turn_stops_on_first_stop_and_skips_later_hooks() {
        struct Stopper;
        impl Hook for Stopper {
            fn name(&self) -> &str {
                "stopper"
            }
            fn pre_turn(&self, _ctx: &HookContext) -> HookOutcome {
                HookOutcome::Stop("nope".into())
            }
        }

        let r = HookRegistry::new();
        r.register(Arc::new(Stopper));
        let later = Arc::new(CountingHook {
            name: "later".into(),
            ..Default::default()
        });
        r.register(later.clone());

        let outcome = r.dispatch_pre_turn(&ctx());
        match outcome {
            HookOutcome::Stop(reason) => assert_eq!(reason, "nope"),
            other => panic!("expected Stop, got {other:?}"),
        }
        assert_eq!(later.pre_turn.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dispatch_post_turn_hits_every_hook() {
        let r = HookRegistry::new();
        let h = Arc::new(CountingHook {
            name: "a".into(),
            ..Default::default()
        });
        r.register(h.clone());
        let _ = r.dispatch_post_turn(&ctx(), &turn_summary_ok());
        assert_eq!(h.post_turn.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dispatch_pre_tool_first_non_allow_wins() {
        struct Denier;
        impl Hook for Denier {
            fn name(&self) -> &str {
                "denier"
            }
            fn pre_tool(&self, _ctx: &HookContext, _t: &ToolCall) -> ToolDecision {
                ToolDecision::Deny("nope".into())
            }
        }

        let r = HookRegistry::new();
        let allow_first = Arc::new(CountingHook {
            name: "allow".into(),
            ..Default::default()
        });
        r.register(allow_first.clone());
        r.register(Arc::new(Denier));
        let later = Arc::new(CountingHook {
            name: "later".into(),
            ..Default::default()
        });
        r.register(later.clone());

        let decision = r.dispatch_pre_tool(&ctx(), &sample_tool_call());
        assert!(decision.is_deny());

        // First hook ran (Allow); denier ran; later did NOT.
        assert_eq!(allow_first.pre_tool.load(Ordering::SeqCst), 1);
        assert_eq!(later.pre_tool.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dispatch_pre_tool_override_short_circuits_chain() {
        struct Overrider;
        impl Hook for Overrider {
            fn name(&self) -> &str {
                "overrider"
            }
            fn pre_tool(&self, _ctx: &HookContext, _t: &ToolCall) -> ToolDecision {
                ToolDecision::Override(serde_json::json!({"replaced": true}))
            }
        }

        let r = HookRegistry::new();
        r.register(Arc::new(Overrider));
        let later = Arc::new(CountingHook {
            name: "later".into(),
            ..Default::default()
        });
        r.register(later.clone());

        let decision = r.dispatch_pre_tool(&ctx(), &sample_tool_call());
        match decision {
            ToolDecision::Override(v) => {
                assert_eq!(v["replaced"], serde_json::Value::Bool(true));
            }
            other => panic!("expected Override, got {other:?}"),
        }
        assert_eq!(later.pre_tool.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dispatch_pre_tool_all_allow_returns_allow() {
        let r = HookRegistry::new();
        for n in &["a", "b", "c"] {
            r.register(Arc::new(CountingHook {
                name: (*n).into(),
                ..Default::default()
            }));
        }
        let decision = r.dispatch_pre_tool(&ctx(), &sample_tool_call());
        assert!(decision.is_allow());
    }

    #[test]
    fn dispatch_post_tool_stop_short_circuits_later_hooks() {
        struct Stopper;
        impl Hook for Stopper {
            fn name(&self) -> &str {
                "stopper"
            }
            fn post_tool(
                &self,
                _ctx: &HookContext,
                _t: &ToolCall,
                _r: &ToolResultSummary,
            ) -> HookOutcome {
                HookOutcome::Stop("done".into())
            }
        }

        let r = HookRegistry::new();
        r.register(Arc::new(Stopper));
        let later = Arc::new(CountingHook {
            name: "later".into(),
            ..Default::default()
        });
        r.register(later.clone());

        let outcome = r.dispatch_post_tool(&ctx(), &sample_tool_call(), &tool_result_ok());
        assert!(outcome.is_stop());
        assert_eq!(later.post_tool.load(Ordering::SeqCst), 0);
    }

    // ---- Global registry ----------------------------------------------

    #[test]
    fn global_registry_returns_same_instance() {
        let a = global_registry();
        let b = global_registry();
        // Same Arc<RwLock>; mutating through one is visible through the other.
        let h = Arc::new(CountingHook {
            name: "global-test-hook".into(),
            ..Default::default()
        });
        a.register(h);
        let names = b.names();
        assert!(names.contains(&"global-test-hook".to_string()));
        // Cleanup so we don't leak registrations into other tests.
        b.unregister("global-test-hook");
    }

    // ---- LoggingHook (smoke test — just confirms it doesn't panic) ----

    #[test]
    fn logging_hook_callbacks_smoke() {
        let h = LoggingHook;
        assert_eq!(h.name(), "logging");
        assert_eq!(h.pre_turn(&ctx()), HookOutcome::Continue);
        assert_eq!(h.post_turn(&ctx(), &turn_summary_ok()), HookOutcome::Continue);
        assert!(h.pre_tool(&ctx(), &sample_tool_call()).is_allow());
        assert_eq!(
            h.post_tool(&ctx(), &sample_tool_call(), &tool_result_ok()),
            HookOutcome::Continue
        );
    }
}
