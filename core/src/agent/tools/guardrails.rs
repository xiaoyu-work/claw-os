//! Tool guardrails — allow/deny lists for restricting which tools the
//! model can see and call.
//!
//! Use case: shipping the same agent loop in different security
//! contexts. Examples:
//!
//!   * **Untrusted prompts** — disable destructive tools (`cos_proc`
//!     kill, `cos_sandbox` exec, network egress) so prompt injection
//!     can't pivot to system damage.
//!   * **Sub-agent delegation** — `cos_delegate` already restricts
//!     children via `allowed_tools`. This module is the standalone
//!     equivalent for the top-level loop.
//!   * **Demo mode** — limit to read-only inspection tools.
//!   * **Test isolation** — surface only `echo` / `now` to keep
//!     fixtures hermetic without manually building a registry.
//!
//! Two-list model:
//!
//!   * `allow: Option<HashSet<String>>` — when `Some`, only listed
//!     tools are permitted; everything else is denied. When `None`
//!     (the default), all tools are permitted **unless** explicitly
//!     denied.
//!   * `deny: HashSet<String>` — explicit denylist. Always wins over
//!     `allow` (deny-overrides-allow). An empty deny set means nothing
//!     is explicitly forbidden.
//!
//! ## Decision matrix
//!
//! ```text
//!   allow=None,           deny=empty   → Allow every tool.
//!   allow=None,           deny=[X]     → Allow all except X.
//!   allow=Some([A,B]),    deny=empty   → Allow only A, B.
//!   allow=Some([A,B]),    deny=[A]     → Allow only B.
//!   allow=Some([]),       deny=*       → Allow nothing.
//! ```
//!
//! Library-only this commit. Runtime integration (e.g. wiring a
//! `Guardrails` into the agent loop or `cos_delegate`) is a separate
//! step and a behaviour change requiring its own review.

use std::collections::HashSet;
use std::sync::Arc;

use super::registry::ToolRegistry;
use super::Tool;
use crate::agent::llm;

/// Decision returned by [`Guardrails::decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Tool is permitted.
    Allow,
    /// Tool is denied. The string is a human-readable reason suitable
    /// for surfacing to the model (so it can pick a different tool).
    Deny(String),
}

impl Decision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Decision::Allow)
    }

    pub fn is_deny(&self) -> bool {
        matches!(self, Decision::Deny(_))
    }
}

/// Allow/deny ruleset.
///
/// Default state is "allow everything" (`allow: None, deny: empty`).
/// Build with the chainable constructors or directly via fields.
#[derive(Debug, Clone, Default)]
pub struct Guardrails {
    /// `None` → no allowlist filtering. `Some(set)` → only members of
    /// `set` are permitted.
    pub allow: Option<HashSet<String>>,
    /// Always forbidden, regardless of `allow`.
    pub deny: HashSet<String>,
}

impl Guardrails {
    /// Permit-everything ruleset.
    pub fn permissive() -> Self {
        Self::default()
    }

    /// Deny everything ruleset (empty allowlist).
    pub fn deny_all() -> Self {
        Self {
            allow: Some(HashSet::new()),
            deny: HashSet::new(),
        }
    }

    /// Set the allowlist. Pass `None` to clear it (= permit all).
    pub fn with_allow<I, S>(mut self, allow: Option<I>) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow = allow.map(|it| it.into_iter().map(Into::into).collect());
        self
    }

    /// Set the denylist (replaces any existing entries).
    pub fn with_deny<I, S>(mut self, deny: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.deny = deny.into_iter().map(Into::into).collect();
        self
    }

    /// Add a single allowlist entry. Initialises the allowlist if it
    /// was `None`.
    pub fn allow_tool(mut self, name: impl Into<String>) -> Self {
        self.allow
            .get_or_insert_with(HashSet::new)
            .insert(name.into());
        self
    }

    /// Add a single denylist entry.
    pub fn deny_tool(mut self, name: impl Into<String>) -> Self {
        self.deny.insert(name.into());
        self
    }

    /// Decide whether `name` is permitted.
    ///
    /// Order:
    ///   1. Deny wins. If `name ∈ deny`, returns `Deny(...)`.
    ///   2. If `allow.is_some()` and `name ∉ allow`, returns `Deny(...)`.
    ///   3. Otherwise `Allow`.
    pub fn decide(&self, name: &str) -> Decision {
        if self.deny.contains(name) {
            return Decision::Deny(format!("tool '{name}' is on the denylist"));
        }
        if let Some(allow) = &self.allow {
            if !allow.contains(name) {
                return Decision::Deny(format!("tool '{name}' is not on the allowlist"));
            }
        }
        Decision::Allow
    }

    /// True iff `name` is permitted.
    pub fn permits(&self, name: &str) -> bool {
        self.decide(name).is_allow()
    }
}

/// Filter the LLM-facing tool list emitted by `registry` to only the
/// tools `guardrails` permits. Preserves the original sort order.
pub fn filter_llm_tools(registry: &ToolRegistry, guardrails: &Guardrails) -> Vec<llm::Tool> {
    registry
        .as_llm_tools()
        .into_iter()
        .filter(|t| guardrails.permits(&t.name))
        .collect()
}

/// Like [`ToolRegistry::get`] but returns `None` for denied tools.
/// `Some(Arc<dyn Tool>)` on allow + present, `None` on deny or absent.
pub fn get_filtered(
    registry: &ToolRegistry,
    guardrails: &Guardrails,
    name: &str,
) -> Option<Arc<dyn Tool>> {
    if !guardrails.permits(name) {
        return None;
    }
    registry.get(name)
}

/// Return the sorted names of every permitted tool currently registered.
/// Useful for prompts that enumerate available capabilities.
pub fn permitted_names(registry: &ToolRegistry, guardrails: &Guardrails) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = registry
        .names()
        .into_iter()
        .filter(|n| guardrails.permits(n))
        .collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::registry::builtin_only_registry;

    fn r() -> ToolRegistry {
        builtin_only_registry()
    }

    #[test]
    fn permissive_default_allows_everything() {
        let g = Guardrails::default();
        assert_eq!(g.decide("echo"), Decision::Allow);
        assert_eq!(g.decide("anything"), Decision::Allow);
        assert!(g.permits("echo"));
    }

    #[test]
    fn deny_all_denies_everything() {
        let g = Guardrails::deny_all();
        assert!(matches!(g.decide("echo"), Decision::Deny(_)));
        assert!(matches!(g.decide("xyz"), Decision::Deny(_)));
    }

    #[test]
    fn allowlist_only_permits_listed() {
        let g = Guardrails::default().with_allow(Some(["echo"]));
        assert_eq!(g.decide("echo"), Decision::Allow);
        assert!(matches!(g.decide("now"), Decision::Deny(_)));
    }

    #[test]
    fn denylist_blocks_listed() {
        let g = Guardrails::default().with_deny(["echo"]);
        assert!(matches!(g.decide("echo"), Decision::Deny(_)));
        assert_eq!(g.decide("now"), Decision::Allow);
    }

    #[test]
    fn deny_wins_over_allow() {
        let g = Guardrails::default()
            .with_allow(Some(["echo", "now"]))
            .with_deny(["echo"]);
        assert!(matches!(g.decide("echo"), Decision::Deny(_)));
        assert_eq!(g.decide("now"), Decision::Allow);
    }

    #[test]
    fn allow_tool_initialises_set_when_none() {
        let g = Guardrails::default().allow_tool("echo");
        assert!(g.allow.is_some());
        assert_eq!(g.decide("echo"), Decision::Allow);
        assert!(matches!(g.decide("now"), Decision::Deny(_)));
    }

    #[test]
    fn allow_tool_appends_to_existing_set() {
        let g = Guardrails::default()
            .with_allow(Some(["echo"]))
            .allow_tool("now");
        assert_eq!(g.decide("echo"), Decision::Allow);
        assert_eq!(g.decide("now"), Decision::Allow);
    }

    #[test]
    fn deny_tool_appends_to_existing_set() {
        let g = Guardrails::default().deny_tool("echo").deny_tool("now");
        assert!(matches!(g.decide("echo"), Decision::Deny(_)));
        assert!(matches!(g.decide("now"), Decision::Deny(_)));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let g = Guardrails::default().with_allow(Some(Vec::<String>::new()));
        assert!(matches!(g.decide("echo"), Decision::Deny(_)));
    }

    #[test]
    fn deny_decision_includes_tool_name() {
        let g = Guardrails::default().with_deny(["echo"]);
        match g.decide("echo") {
            Decision::Deny(reason) => assert!(reason.contains("echo")),
            Decision::Allow => panic!("expected deny"),
        }
    }

    #[test]
    fn filter_llm_tools_keeps_only_permitted() {
        let registry = r();
        let g = Guardrails::default().with_allow(Some(["echo"]));
        let tools = filter_llm_tools(&registry, &g);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
    }

    #[test]
    fn filter_llm_tools_permissive_returns_all() {
        let registry = r();
        let g = Guardrails::default();
        let tools = filter_llm_tools(&registry, &g);
        assert_eq!(tools.len(), registry.len());
    }

    #[test]
    fn filter_llm_tools_deny_all_returns_none() {
        let registry = r();
        let g = Guardrails::deny_all();
        let tools = filter_llm_tools(&registry, &g);
        assert!(tools.is_empty());
    }

    #[test]
    fn get_filtered_returns_none_for_denied() {
        let registry = r();
        let g = Guardrails::default().with_deny(["echo"]);
        assert!(get_filtered(&registry, &g, "echo").is_none());
        assert!(get_filtered(&registry, &g, "now").is_some());
    }

    #[test]
    fn get_filtered_returns_none_for_unknown_tool() {
        let registry = r();
        let g = Guardrails::default();
        assert!(get_filtered(&registry, &g, "nonexistent").is_none());
    }

    #[test]
    fn get_filtered_returns_arc_for_allowed() {
        let registry = r();
        let g = Guardrails::default();
        let t = get_filtered(&registry, &g, "echo").expect("echo allowed");
        assert_eq!(t.name(), "echo");
    }

    #[test]
    fn permitted_names_returns_sorted_subset() {
        let registry = r();
        let g = Guardrails::default().with_deny(["echo"]);
        let names = permitted_names(&registry, &g);
        assert_eq!(names, vec!["now"]);
    }

    #[test]
    fn permitted_names_empty_when_deny_all() {
        let registry = r();
        let g = Guardrails::deny_all();
        assert!(permitted_names(&registry, &g).is_empty());
    }

    #[test]
    fn permits_helper_matches_decide() {
        let g = Guardrails::default().with_deny(["x"]);
        assert!(g.permits("y"));
        assert!(!g.permits("x"));
    }

    #[test]
    fn guardrails_clone_independent() {
        let g1 = Guardrails::default().with_deny(["x"]);
        let g2 = g1.clone().with_deny(["y"]);
        assert!(matches!(g1.decide("x"), Decision::Deny(_)));
        assert_eq!(g1.decide("y"), Decision::Allow);
        assert_eq!(g2.decide("x"), Decision::Allow);
        assert!(matches!(g2.decide("y"), Decision::Deny(_)));
    }
}
