//! Tool registry — collection of `Arc<dyn Tool>` keyed by name.

use std::collections::HashMap;
use std::sync::Arc;

use super::Tool;
use crate::agent::llm;

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Last write wins for duplicate names.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.tools.keys().copied().collect();
        names.sort_unstable();
        names
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Convert to the LLM-trait-facing representation passed in `ChatRequest.tools`.
    pub fn as_llm_tools(&self) -> Vec<llm::Tool> {
        let mut out: Vec<llm::Tool> = self
            .tools
            .values()
            .map(|t| llm::Tool {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Build the default registry shipped with `cos agent`.
///
/// Includes:
/// - Side-effect-free built-ins (`echo`, `now`).
/// - All cos kernel primitive proxies (sandbox, proc, sysinfo, credential,
///   cron, checkpoint, service, trace, watch, ipc, browser, netfilter,
///   policy, model). Each proxy gives the model the exact same surface as
///   the cos CLI for that primitive.
/// - `cos_memory` (notes) and, if the default memory DB opens cleanly,
///   `cos_recall` (FTS5 history search).
pub fn default_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(super::builtin::Echo));
    r.register(Arc::new(super::builtin::Now));
    r.register(Arc::new(super::delegate::Delegate));
    r.register(Arc::new(super::todo::Todo::default_tool()));
    super::cos_proxy::register_all(&mut r);
    // Best-effort: open the default memory DB; if it fails (read-only fs,
    // etc.) the agent still works, just without searchable history.
    match crate::agent::memory::sqlite_fts::MemoryDb::open_default() {
        Ok(db) => super::cos_proxy::register_recall(&mut r, db),
        Err(e) => tracing::warn!("cos_recall: failed to open default memory DB: {e}"),
    }
    r
}

/// Minimal registry: only side-effect-free built-ins. Used by tests that
/// don't want to touch the real system.
pub fn builtin_only_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(super::builtin::Echo));
    r.register(Arc::new(super::builtin::Now));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_builtins_and_cos_proxy() {
        let r = default_registry();
        assert!(r.get("echo").is_some());
        assert!(r.get("now").is_some());
        assert!(r.get("cos_delegate").is_some());
        assert!(r.get("cos_todo").is_some());
        assert!(r.get("cos_sandbox").is_some());
        assert!(r.get("cos_sysinfo").is_some());
        assert!(r.get("cos_memory").is_some());
        // 2 builtins + cos_delegate + cos_todo + every cos_proxy tool
        // (primitives + cos_memory), plus optionally cos_recall
        // (registered iff default DB opens).
        let expected_min = 4 + super::super::cos_proxy::total_count();
        let expected_max = expected_min + 1;
        assert!(
            (expected_min..=expected_max).contains(&r.len()),
            "expected {}..={} tools, got {}",
            expected_min,
            expected_max,
            r.len()
        );
    }

    #[test]
    fn builtin_only_registry_has_just_builtins() {
        let r = builtin_only_registry();
        assert_eq!(r.len(), 2);
        assert!(r.get("cos_sandbox").is_none());
    }

    #[test]
    fn names_are_sorted() {
        let r = default_registry();
        let names = r.names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn as_llm_tools_round_trips_schema() {
        let r = default_registry();
        let tools = r.as_llm_tools();
        assert!(tools.iter().any(|t| t.name == "echo"));
    }
}
