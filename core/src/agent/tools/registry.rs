//! Tool registry — collection of `Arc<dyn Tool>` keyed by name.
//!
//! Optionally carries a [`Guardrails`](super::guardrails::Guardrails) that
//! restricts which tools the model can see and call. The default
//! is `Guardrails::permissive()` (every registered tool is permitted).
//!
//! Optionally carries an [`ApprovalGate`](super::super::runtime::approval::ApprovalGate)
//! that gates per-call invocations of tools the policy classifies as
//! dangerous. The default is an empty gate (every call short-circuits
//! to `Approved`).

use std::collections::HashMap;
use std::sync::Arc;

use super::guardrails::Guardrails;
use super::Tool;
use crate::agent::llm;
use crate::agent::runtime::approval::ApprovalGate;

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    guardrails: Guardrails,
    approval: ApprovalGate,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Last write wins for duplicate names.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_owned(), tool);
    }

    /// Replace the active guardrails. Call once at construction time.
    pub fn set_guardrails(&mut self, guardrails: Guardrails) {
        self.guardrails = guardrails;
    }

    /// Borrow the active guardrails.
    pub fn guardrails(&self) -> &Guardrails {
        &self.guardrails
    }

    /// Replace the active approval gate. Call once at construction time.
    pub fn set_approval(&mut self, approval: ApprovalGate) {
        self.approval = approval;
    }

    /// Borrow the active approval gate.
    pub fn approval(&self) -> &ApprovalGate {
        &self.approval
    }

    /// Returns `Some(tool)` only when the tool is registered AND permitted
    /// by the active guardrails. Returns `None` for absent OR denied tools.
    /// Used by the runtime turn dispatcher so denied calls are uniformly
    /// rejected, regardless of whether the model saw the tool in its
    /// schema list.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if !self.guardrails.permits(name) {
            return None;
        }
        self.tools.get(name).cloned()
    }

    /// Like [`get`] but ignores guardrails. Use only when you specifically
    /// need to bypass policy (e.g. printing the registered set in
    /// diagnostics). Production runtime code should use [`get`].
    pub fn get_unfiltered(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Whether the named tool opts into concurrent dispatch with
    /// siblings in the same turn (see [`Tool::parallel_safe`]).
    /// Unknown / denied tools return `false` — they'll be handled by
    /// the normal serial path which already raises a clear error.
    pub fn is_parallel_safe(&self, name: &str) -> bool {
        self.tools
            .get(name)
            .map(|t| t.parallel_safe())
            .unwrap_or(false)
    }

    /// Names of every permitted tool, sorted.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .tools
            .keys()
            .map(String::as_str)
            .filter(|n| self.guardrails.permits(n))
            .collect();
        names.sort_unstable();
        names
    }

    /// Names of every registered tool ignoring guardrails. For diagnostics.
    pub fn names_unfiltered(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    pub fn len(&self) -> usize {
        self.names().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Convert to the LLM-trait-facing representation passed in
    /// `ChatRequest.tools`. Honours guardrails — denied tools are NOT
    /// surfaced to the model.
    pub fn as_llm_tools(&self) -> Vec<llm::Tool> {
        let mut out: Vec<llm::Tool> = self
            .tools
            .values()
            .filter(|t| self.guardrails.permits(t.name()))
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
/// - The compact `cos_app_catalog` / `cos_app_run` progressive App gateways
///   plus any explicitly active stateful App-session tools.
/// - `cos_memory` (notes) and, if the default memory DB opens cleanly,
///   `cos_recall` (FTS5 history search).
pub fn default_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(super::builtin::Echo));
    r.register(Arc::new(super::builtin::Now));
    r.register(Arc::new(super::delegate::Delegate));
    r.register(Arc::new(super::todo::Todo::default_tool()));
    r.register(Arc::new(super::clarify::Clarify::new()));
    r.register(Arc::new(super::skills::SkillDisclosure::new()));
    super::cos_proxy::register_all(&mut r);
    super::cos_apps::register_default(&mut r);
    super::cos_apps_session::register_all(&mut r);
    super::media::register_default_media_tools(&mut r);
    // Best-effort: open the default memory DB; if it fails (read-only fs,
    // etc.) the agent still works, just without searchable history.
    match crate::agent::memory::sqlite_fts::MemoryDb::open_default() {
        Ok(db) => {
            super::cos_proxy::register_recall(&mut r, db.clone());
            super::cos_proxy::register_app_memory(&mut r, db);
        }
        Err(e) => tracing::warn!("cos_recall/cos_app_memory: failed to open default memory DB: {e}"),
    }
    // Best-effort: open the default semantic store; only registered
    // when `[embed]` is configured. When disabled the tool silently
    // doesn't exist (the LLM falls back to cos_recall keyword search).
    use crate::agent::memory::semantic::{SemanticStore, SemanticStoreExt};
    match SemanticStore::open_default() {
        Ok(Some(store)) => {
            super::cos_proxy::register_recall_semantic(&mut r, std::sync::Arc::new(store))
        }
        Ok(None) => {
            tracing::debug!("cos_recall_semantic: [embed] disabled — tool not registered")
        }
        Err(e) => tracing::warn!(
            "cos_recall_semantic: failed to open default semantic DB: {e}"
        ),
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/registry.rs"
    ));
}
