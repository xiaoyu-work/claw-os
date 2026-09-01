//! Tool registry — immutable descriptors and implementations keyed by name.
//!
//! Optionally carries an [`ApprovalGate`](super::super::runtime::approval::ApprovalGate)
//! for legacy tool-name filters and hard operator denies. Capability-aware
//! tools derive consent from their exact validated operation at execution.
//!
//! Session authorization and reachability are deliberately absent from the
//! cached entries. Every model projection and execution lookup receives a
//! [`ToolExposureContext`](super::exposure::ToolExposureContext), so one
//! session cannot populate process-global availability state for another.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use super::exposure::{ExposureDecision, ToolExposure, ToolExposureContext};
use super::{Tool, ToolResult};
use crate::agent::llm;
use crate::agent::runtime::approval::ApprovalGate;

#[derive(Clone)]
struct ToolEntry {
    tool: Arc<dyn Tool>,
    descriptor: llm::Tool,
    exposure: ToolExposure,
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
    approval: ApprovalGate,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Last write wins for duplicate names.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let descriptor = llm::Tool {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            input_schema: tool.input_schema(),
        };
        let exposure = tool.exposure();
        self.tools.insert(
            descriptor.name.clone(),
            ToolEntry {
                tool,
                descriptor,
                exposure,
            },
        );
    }

    /// Register an untrusted/dynamic tool without allowing it to replace an
    /// existing immutable descriptor.
    pub fn register_unique(&mut self, tool: Arc<dyn Tool>) -> Result<(), String> {
        if self.tools.contains_key(tool.name()) {
            return Err("tool name is already registered".to_string());
        }
        self.register(tool);
        Ok(())
    }

    /// Replace the active approval gate. Call once at construction time.
    pub fn set_approval(&mut self, approval: ApprovalGate) {
        self.approval = approval;
    }

    pub(crate) fn policy_fork(&self) -> Self {
        Self {
            tools: HashMap::new(),
            approval: self.approval.clone(),
        }
    }

    pub(crate) fn policy_visible(&self, context: &ToolExposureContext, name: &str) -> bool {
        self.exposure_decision(context, name).is_visible() && !self.approval.is_auto_denied(name)
    }

    /// Returns the exposure decision for a registered tool.
    pub fn exposure_decision(&self, context: &ToolExposureContext, name: &str) -> ExposureDecision {
        let Some(entry) = self.tools.get(name) else {
            return ExposureDecision::Hidden("tool is not registered".to_string());
        };
        if let super::guardrails::Decision::Deny(reason) = context.guardrails().decide(name) {
            return ExposureDecision::Hidden(reason);
        }
        entry.exposure.decide(context)
    }

    /// Returns `Some(tool)` only when the current session may see and reach it.
    pub fn get_for(&self, context: &ToolExposureContext, name: &str) -> Option<Arc<dyn Tool>> {
        if !self.exposure_decision(context, name).is_visible() {
            return None;
        }
        self.tools.get(name).map(|entry| entry.tool.clone())
    }

    /// Descriptor-cache lookup without session projection. Runtime dispatch
    /// must use [`get_for`](Self::get_for).
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.get_unfiltered(name)
    }

    /// Compatibility alias for raw descriptor-cache lookup.
    /// Production runtime code must use [`get_for`](Self::get_for).
    pub fn get_unfiltered(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).map(|entry| entry.tool.clone())
    }

    pub fn descriptor_unfiltered(&self, name: &str) -> Option<&llm::Tool> {
        self.tools.get(name).map(|entry| &entry.descriptor)
    }

    /// Whether the named tool opts into concurrent dispatch with
    /// siblings in the same turn (see [`Tool::parallel_safe`]).
    /// Unknown / denied tools return `false` — they'll be handled by
    /// the normal serial path which already raises a clear error.
    pub fn is_parallel_safe_for(&self, context: &ToolExposureContext, name: &str) -> bool {
        if !self.exposure_decision(context, name).is_visible() {
            return false;
        }
        self.tools
            .get(name)
            .map(|entry| entry.tool.parallel_safe())
            .unwrap_or(false)
    }

    pub fn is_parallel_safe(&self, name: &str) -> bool {
        self.tools
            .get(name)
            .map(|entry| entry.tool.parallel_safe())
            .unwrap_or(false)
    }

    /// Names visible in this session, sorted.
    pub fn names_for(&self, context: &ToolExposureContext) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .tools
            .keys()
            .map(String::as_str)
            .filter(|name| self.exposure_decision(context, name).is_visible())
            .collect();
        names.sort_unstable();
        names
    }

    /// Names in the immutable descriptor cache. Runtime model projection must
    /// use [`names_for`](Self::names_for).
    pub fn names(&self) -> Vec<&str> {
        self.names_unfiltered()
    }

    /// Names of every registered tool ignoring guardrails. For diagnostics.
    pub fn names_unfiltered(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    pub fn len_for(&self, context: &ToolExposureContext) -> usize {
        self.names_for(context).len()
    }

    pub fn is_empty_for(&self, context: &ToolExposureContext) -> bool {
        self.len_for(context) == 0
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Convert the current session projection to the representation passed in
    /// `ChatRequest.tools`.
    pub fn as_llm_tools_for(&self, context: &ToolExposureContext) -> Vec<llm::Tool> {
        let mut out: Vec<llm::Tool> = self
            .tools
            .iter()
            .filter(|(name, _)| self.exposure_decision(context, name).is_visible())
            .map(|(_, entry)| entry.descriptor.clone())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Every immutable descriptor, without session projection.
    pub fn as_llm_tools(&self) -> Vec<llm::Tool> {
        let mut out: Vec<llm::Tool> = self
            .tools
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Execute through the same session projection used for schema exposure.
    ///
    /// This is intentionally only the coarse reachability check. Tool
    /// implementations still validate their arguments and rerun exact
    /// capability/scope enforcement before side effects.
    pub async fn execute(
        &self,
        context: &ToolExposureContext,
        name: &str,
        input: Value,
        approval_reason: &str,
    ) -> ToolResult {
        let Some(tool) = self.get_for(context, name) else {
            let reason = self
                .exposure_decision(context, name)
                .reason()
                .unwrap_or("unavailable")
                .to_string();
            return ToolResult::err(format!("tool `{name}` is unavailable: {reason}"));
        };

        if self.approval.is_classified(name) {
            match self
                .approval
                .evaluate_for(name, &input, approval_reason, tool.approval_boundary())
                .await
            {
                crate::agent::runtime::approval::ApprovalOutcome::Approved { .. } => {}
                crate::agent::runtime::approval::ApprovalOutcome::Denied { reason } => {
                    return ToolResult::err(format!(
                        "approval denied for `{name}`: {}",
                        reason.unwrap_or_else(|| "no reason".to_string())
                    ));
                }
                crate::agent::runtime::approval::ApprovalOutcome::Deferred { prompt } => {
                    return ToolResult::err(format!(
                        "approval pending for `{name}`: {}",
                        prompt.unwrap_or_else(|| "user approval required".to_string())
                    ));
                }
            }
        }

        let guardrails = context.guardrails().clone();
        let approval = self.approval.clone();
        let exposure = context.clone();
        super::exposure::scope(
            exposure.clone(),
            super::delegate::PARENT_GUARDRAILS.scope(
                guardrails,
                super::delegate::PARENT_APPROVAL.scope(
                    approval,
                    super::delegate::PARENT_EXPOSURE.scope(exposure, tool.exec(input)),
                ),
            ),
        )
        .await
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
        Err(e) => {
            tracing::warn!("cos_recall/cos_app_memory: failed to open default memory DB: {e}")
        }
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
        Err(e) => tracing::warn!("cos_recall_semantic: failed to open default semantic DB: {e}"),
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
