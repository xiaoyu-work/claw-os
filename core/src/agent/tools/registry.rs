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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use super::exposure::{ExposureDecision, ToolExposure, ToolExposureContext};
use super::guardrails::Guardrails;
use super::progressive::{self, CatalogEntry, ToolDisclosure};
use super::{Tool, ToolResult};
use crate::agent::llm::{self, ToolCall};
use crate::agent::runtime::approval::ApprovalGate;
use crate::config::CosConfig;

#[derive(Debug, Clone)]
pub struct RegistryPaths {
    pub apps_dir: PathBuf,
    pub todos_dir: PathBuf,
    pub system_skills_dir: PathBuf,
    pub user_skills_dir: PathBuf,
    pub skills_usage_path: PathBuf,
    pub media_outputs_dir: PathBuf,
    pub memory_db_path: PathBuf,
    pub semantic_db_path: PathBuf,
    pub notes_dir: PathBuf,
    pub hooks_config_path: PathBuf,
    pub audit_log_path: PathBuf,
    pub nudges_path: PathBuf,
    pub system_skills_origin: crate::agent::skills::loader::SkillOrigin,
    pub curation_log_path: PathBuf,
}

impl RegistryPaths {
    pub fn from_process() -> Self {
        let apps_dir = std::env::var_os("COS_APPS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/lib/cos/apps"));
        let system_skills_origin = if std::env::var_os("COS_SYSTEM_SKILLS_DIR").is_some() {
            crate::agent::skills::loader::SkillOrigin::Local
        } else {
            crate::agent::skills::loader::SkillOrigin::BuiltIn
        };
        Self {
            apps_dir,
            todos_dir: crate::paths::agent_todos_dir(),
            system_skills_dir: crate::paths::system_skills_dir(),
            user_skills_dir: crate::paths::agent_skills_dir(),
            skills_usage_path: crate::paths::agent_skills_usage_path(),
            media_outputs_dir: crate::paths::agent_media_outputs_dir(),
            memory_db_path: crate::paths::agent_memory_db_path(),
            semantic_db_path: crate::paths::agent_semantic_db_path(),
            notes_dir: crate::paths::agent_notes_dir(),
            hooks_config_path: crate::paths::agent_hooks_path(),
            audit_log_path: crate::paths::agent_audit_log_path(),
            nudges_path: crate::paths::agent_nudges_path(),
            system_skills_origin,
            curation_log_path: crate::paths::agent_curation_log_path(),
        }
    }

    pub fn runtime_paths(&self) -> crate::agent::runtime::deps::RuntimePaths {
        crate::agent::runtime::deps::RuntimePaths {
            hooks_config: self.hooks_config_path.clone(),
            audit_log: self.audit_log_path.clone(),
            notes_dir: self.notes_dir.clone(),
            nudges_path: self.nudges_path.clone(),
            system_skills_dir: self.system_skills_dir.clone(),
            user_skills_dir: self.user_skills_dir.clone(),
            system_skills_origin: self.system_skills_origin,
            curation_log: self.curation_log_path.clone(),
        }
    }
}

#[derive(Clone)]
pub struct RegistryDeps {
    pub config: Arc<CosConfig>,
    pub paths: RegistryPaths,
    pub memory: Option<crate::agent::memory::sqlite_fts::MemoryDb>,
    pub semantic: Option<Arc<crate::agent::memory::semantic::SemanticStore>>,
    pub runtime: crate::agent::runtime::deps::RuntimeDeps,
    app_sessions: Vec<super::cos_apps_session::RegisteredAppSession>,
}

impl RegistryDeps {
    pub fn load_current() -> Self {
        Self::load(
            crate::config::current_snapshot(),
            RegistryPaths::from_process(),
        )
    }

    pub fn load(config: Arc<CosConfig>, paths: RegistryPaths) -> Self {
        Self::load_with_hooks(
            config,
            paths,
            crate::agent::runtime::hooks::HookRegistry::new(),
        )
    }

    pub fn load_with_hooks(
        config: Arc<CosConfig>,
        paths: RegistryPaths,
        hooks: crate::agent::runtime::hooks::HookRegistry,
    ) -> Self {
        let memory = match crate::agent::memory::sqlite_fts::MemoryDb::open(&paths.memory_db_path) {
            Ok(db) => Some(db),
            Err(error) => {
                tracing::warn!("cos_recall/cos_app_memory: failed to open memory DB: {error}");
                None
            }
        };
        let semantic = match crate::agent::memory::semantic::open_with_config(
            &config.embed,
            &config.agent,
            paths.semantic_db_path.clone(),
        ) {
            Ok(store) => store.map(Arc::new),
            Err(error) => {
                tracing::warn!("cos_recall_semantic: failed to open semantic DB: {error}");
                None
            }
        };
        let app_sessions = crate::apps::discover_verified(&paths.apps_dir)
            .values()
            .filter(|app| app.manifest.session.is_some())
            .map(|app| super::cos_apps_session::RegisteredAppSession {
                manifest: Arc::new(app.manifest.clone()),
                app_dir: app.dir.clone(),
            })
            .collect();
        let runtime = crate::agent::runtime::deps::RuntimeDeps::load_with_hooks(
            &paths.runtime_paths(),
            semantic.clone(),
            hooks,
        )
        .with_config_snapshot(Arc::clone(&config));
        Self {
            config,
            paths,
            memory,
            semantic,
            runtime,
            app_sessions,
        }
    }

    pub fn without_optional_resources(config: Arc<CosConfig>, paths: RegistryPaths) -> Self {
        let runtime = crate::agent::runtime::deps::RuntimeDeps::load(&paths.runtime_paths(), None)
            .with_config_snapshot(Arc::clone(&config));
        Self {
            config,
            paths,
            memory: None,
            semantic: None,
            runtime,
            app_sessions: Vec::new(),
        }
    }
}

#[derive(Clone)]
struct ToolEntry {
    tool: Arc<dyn Tool>,
    descriptor: Arc<llm::Tool>,
    exposure: ToolExposure,
    disclosure: Arc<ToolDisclosure>,
}

pub struct PreparedExtensionProposal {
    tool: Arc<dyn Tool>,
    call: ToolCall,
    required_capability: crate::caps::Cap,
    operation_digest: String,
}

impl PreparedExtensionProposal {
    pub fn binding(&self) -> ExtensionProposalBinding {
        ExtensionProposalBinding {
            action_id: self.call.id.clone(),
            tool: self.call.name.clone(),
            policy_id: self
                .tool
                .extension_proposal_policy()
                .expect("prepared extension proposal has a policy")
                .policy_id
                .to_string(),
            input_digest: crate::crypto::sha256_hex(
                &serde_json::to_vec(&self.call.input).unwrap_or_default(),
            ),
            capability: self.required_capability.clone(),
            operation_digest: self.operation_digest.clone(),
        }
    }

    pub async fn execute(
        self,
        exposure: ToolExposureContext,
        approval: ApprovalGate,
        approval_reason: &'static str,
    ) -> ToolResult {
        if approval.is_classified(&self.call.name) {
            match approval
                .evaluate_for(
                    &self.call.name,
                    &self.call.input,
                    approval_reason,
                    self.tool.approval_boundary(),
                )
                .await
            {
                crate::agent::runtime::approval::ApprovalOutcome::Approved { .. } => {}
                crate::agent::runtime::approval::ApprovalOutcome::Denied { reason } => {
                    return ToolResult::err(format!(
                        "approval denied for `{}`: {}",
                        self.call.name,
                        reason.unwrap_or_else(|| "no reason".to_string())
                    ));
                }
                crate::agent::runtime::approval::ApprovalOutcome::Deferred { prompt } => {
                    return ToolResult::err(format!(
                        "approval pending for `{}`: {}",
                        self.call.name,
                        prompt.unwrap_or_else(|| "user approval required".to_string())
                    ));
                }
            }
        }

        let ceiling = crate::caps::CapSet::from_caps([self.required_capability.clone()]);
        crate::caps::enforcement::with_capability_ceiling(ceiling, async move {
            let check = super::exposure::scope(exposure.clone(), async {
                crate::caps::enforcement::require_or_json_for_operation(
                    self.required_capability.verb,
                    self.required_capability.scope.clone(),
                    &self.operation_digest,
                )
            })
            .await;
            if let Err(denial) = check {
                return ToolResult::err(denial.to_string());
            }
            let guardrails = exposure.guardrails().clone();
            super::exposure::scope(
                exposure.clone(),
                super::delegate::PARENT_GUARDRAILS.scope(
                    guardrails,
                    super::delegate::PARENT_APPROVAL.scope(
                        approval,
                        super::delegate::PARENT_EXPOSURE
                            .scope(exposure, self.tool.exec(self.call.input)),
                    ),
                ),
            )
            .await
        })
        .await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionProposalBinding {
    pub action_id: String,
    pub tool: String,
    pub policy_id: String,
    pub input_digest: String,
    pub capability: crate::caps::Cap,
    pub operation_digest: String,
}

#[derive(Debug)]
struct ToolAttachmentState {
    active: AtomicBool,
    generation: Arc<AtomicU64>,
}

/// Shared liveness token for one dynamically attached extension server.
///
/// Every proxy registered from the same attachment holds a clone. Dropping
/// the owning server handle deactivates the token and increments the registry
/// generation, so subsequent projection and dispatch checks fail closed.
#[derive(Clone, Debug)]
pub(crate) struct ToolAttachment {
    state: Arc<ToolAttachmentState>,
}

impl ToolAttachment {
    fn new(generation: Arc<AtomicU64>) -> Self {
        Self {
            state: Arc::new(ToolAttachmentState {
                active: AtomicBool::new(true),
                generation,
            }),
        }
    }

    pub(crate) fn standalone() -> Self {
        Self::new(Arc::new(AtomicU64::new(0)))
    }

    pub(crate) fn is_active(&self) -> bool {
        self.state.active.load(Ordering::Acquire)
    }

    pub(crate) fn detach(&self) {
        if self.state.active.swap(false, Ordering::AcqRel) {
            self.state.generation.fetch_add(1, Ordering::AcqRel);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolProjectionDiagnostics {
    pub catalog_generation: u64,
    pub budget_tokens: u32,
    pub raw_schema_tokens: u32,
    pub schema_tokens: u32,
    pub deferred_schema_tokens: u32,
    pub permitted_count: usize,
    pub direct_count: usize,
    pub deferred_count: usize,
    pub bridge_count: usize,
    pub progressive: bool,
}

#[derive(Clone, Debug)]
pub struct ToolProjection {
    tools: Vec<llm::Tool>,
    deferred: Vec<CatalogEntry>,
    diagnostics: ToolProjectionDiagnostics,
}

impl ToolProjection {
    pub fn tools(&self) -> &[llm::Tool] {
        &self.tools
    }

    pub fn into_tools(self) -> Vec<llm::Tool> {
        self.tools
    }

    pub fn diagnostics(&self) -> &ToolProjectionDiagnostics {
        &self.diagnostics
    }

    fn contains_model_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name == name)
    }

    fn deferred_entry(&self, name: &str) -> Option<&CatalogEntry> {
        self.deferred
            .iter()
            .find(|entry| entry.descriptor.name == name)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ResolvedToolKind {
    Registry,
    Catalog,
    Rejected(String),
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedToolCall {
    pub call: ToolCall,
    pub kind: ResolvedToolKind,
}

#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
    guardrails: Guardrails,
    approval: ApprovalGate,
    catalog_generation: Arc<AtomicU64>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            tools: HashMap::new(),
            guardrails: Guardrails::permissive(),
            approval: ApprovalGate::default(),
            catalog_generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Last write wins for duplicate names.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let descriptor = Arc::new(llm::Tool {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            input_schema: tool.input_schema(),
        });
        if progressive::is_bridge_tool(&descriptor.name) {
            tracing::warn!(
                tool = %descriptor.name,
                "refusing to register reserved progressive-disclosure bridge name"
            );
            return;
        }
        let exposure = tool.exposure();
        let mut disclosure = tool.disclosure();
        if !disclosure.defer_eligible {
            if let Some(extension) = exposure.extension_id() {
                disclosure = ToolDisclosure::extension(
                    "extension",
                    Some(extension.to_string()),
                    None,
                    ["extension".to_string()],
                );
            }
        }
        self.tools.insert(
            descriptor.name.clone(),
            ToolEntry {
                tool,
                descriptor,
                exposure,
                disclosure: Arc::new(disclosure),
            },
        );
        self.catalog_generation.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn new_attachment(&self) -> ToolAttachment {
        ToolAttachment::new(self.catalog_generation.clone())
    }

    pub fn catalog_generation(&self) -> u64 {
        self.catalog_generation.load(Ordering::Acquire)
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

    pub fn set_guardrails(&mut self, guardrails: Guardrails) {
        self.guardrails = guardrails;
    }

    pub fn guardrails(&self) -> &Guardrails {
        &self.guardrails
    }

    pub fn approval(&self) -> &ApprovalGate {
        &self.approval
    }

    pub fn validate_extension_action_policy(
        &self,
        tool: &str,
        expected_policy_id: &str,
    ) -> Result<(), String> {
        let entry = self
            .tools
            .get(tool)
            .ok_or_else(|| "extension action policy names an unregistered tool".to_string())?;
        let policy = entry
            .tool
            .extension_proposal_policy()
            .ok_or_else(|| "tool does not accept Agent-extension proposed actions".to_string())?;
        if policy.policy_id != expected_policy_id
            || policy.execution != super::ExtensionProposalExecution::Cooperative
        {
            return Err("extension action policy does not match the tool policy".to_string());
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_extension_proposal(
        &self,
        context: &ToolExposureContext,
        extension_id: &str,
        package_digest: &str,
        manifest_digest: &str,
        event_id: &str,
        action: &crate::extension_host::abi::ProposedAction,
        expected_policy_id: &str,
        expected_capability: &crate::caps::Cap,
    ) -> Result<PreparedExtensionProposal, String> {
        let entry = self
            .tools
            .get(&action.tool)
            .ok_or_else(|| "extension proposed an unregistered tool".to_string())?;
        let policy = entry
            .tool
            .extension_proposal_policy()
            .ok_or_else(|| "tool does not accept Agent-extension proposed actions".to_string())?;
        if policy.policy_id != expected_policy_id
            || policy.execution != super::ExtensionProposalExecution::Cooperative
        {
            return Err("extension action policy does not match the tool policy".to_string());
        }
        let prepared = entry
            .tool
            .prepare_extension_proposal(action.input.clone())?;
        if &prepared.required_capability != expected_capability {
            return Err(
                "extension action capability does not exactly match its declared policy"
                    .to_string(),
            );
        }
        let decision = self.exposure_decision(context, &action.tool);
        if !decision.is_visible() || self.approval.is_auto_denied(&action.tool) {
            return Err(format!(
                "extension proposed tool is unavailable: {}",
                decision.reason().unwrap_or("policy denied")
            ));
        }
        let canonical = serde_json::to_vec(&serde_json::json!({
            "domain": "claw.agent-extension.action/v2",
            "extension_id": extension_id,
            "package_digest": package_digest,
            "manifest_digest": manifest_digest,
            "event_id": event_id,
            "action_id": action.action_id,
            "tool": action.tool,
            "policy_id": policy.policy_id,
            "input": prepared.canonical_input,
            "capability": prepared.required_capability,
            "catalog_generation": self.catalog_generation(),
            "capability_generation": context.capability_generation(),
        }))
        .map_err(|error| format!("encode extension action binding: {error}"))?;
        Ok(PreparedExtensionProposal {
            tool: entry.tool.clone(),
            call: ToolCall {
                id: action.action_id.clone(),
                name: action.tool.clone(),
                input: prepared.canonical_input,
            },
            required_capability: prepared.required_capability,
            operation_digest: crate::crypto::sha256_hex(&canonical),
        })
    }

    pub(crate) fn policy_fork(&self) -> Self {
        Self {
            tools: HashMap::new(),
            guardrails: self.guardrails.clone(),
            approval: self.approval.clone(),
            catalog_generation: Arc::new(AtomicU64::new(0)),
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
        if !entry.tool.is_available() {
            return ExposureDecision::Hidden("tool attachment is detached".to_string());
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
        if !self.guardrails.permits(name) {
            return None;
        }
        self.get_unfiltered(name)
    }

    /// Compatibility alias for raw descriptor-cache lookup.
    /// Production runtime code must use [`get_for`](Self::get_for).
    pub fn get_unfiltered(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).map(|entry| entry.tool.clone())
    }

    pub fn descriptor_unfiltered(&self, name: &str) -> Option<&llm::Tool> {
        self.tools.get(name).map(|entry| entry.descriptor.as_ref())
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
        let mut names = self.names_unfiltered();
        names.retain(|name| self.guardrails.permits(name));
        names
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
        self.names().len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Convert the current session projection to the representation passed in
    /// `ChatRequest.tools`.
    pub fn as_llm_tools_for(&self, context: &ToolExposureContext) -> Vec<llm::Tool> {
        self.projection_for(context).into_tools()
    }

    /// Build one deterministic model projection from the current trusted
    /// exposure facts. No authorization decision is cached across contexts.
    pub fn projection_for(&self, context: &ToolExposureContext) -> ToolProjection {
        let mut direct = Vec::new();
        let mut eligible = Vec::new();
        for (name, entry) in &self.tools {
            if !self.exposure_decision(context, name).is_visible() {
                continue;
            }
            if entry.disclosure.defer_eligible {
                eligible.push(CatalogEntry {
                    descriptor: entry.descriptor.clone(),
                    disclosure: entry.disclosure.clone(),
                });
            } else {
                direct.push(entry.descriptor.as_ref().clone());
            }
        }
        direct.sort_by(|left, right| left.name.cmp(&right.name));
        eligible.sort_by(|left, right| left.descriptor.name.cmp(&right.descriptor.name));

        let deferred_schema_tokens = eligible.iter().fold(0u32, |total, entry| {
            total.saturating_add(progressive::schema_tokens_for_tool(&entry.descriptor))
        });
        let raw_schema_tokens =
            progressive::schema_tokens(&direct).saturating_add(deferred_schema_tokens);
        // Third-party descriptions and schemas are authenticated metadata,
        // not trusted model instructions. Always keep them behind fixed local
        // bridge schemas, independent of their token size.
        let progressive = !eligible.is_empty();

        let (mut tools, deferred, direct_count) = if progressive {
            let direct_count = direct.len();
            let bridges = progressive::bridge_tools()
                .into_iter()
                .filter(|tool| !context.guardrails().explicitly_denies(&tool.name))
                .collect::<Vec<_>>();
            direct.extend(bridges);
            (direct, eligible, direct_count)
        } else {
            let direct_count = direct.len() + eligible.len();
            direct.extend(
                eligible
                    .iter()
                    .map(|entry| entry.descriptor.as_ref().clone()),
            );
            (direct, Vec::new(), direct_count)
        };
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        let bridge_count = tools
            .iter()
            .filter(|tool| progressive::is_bridge_tool(&tool.name))
            .count();
        let diagnostics = ToolProjectionDiagnostics {
            catalog_generation: self.catalog_generation(),
            budget_tokens: context.tool_schema_budget_tokens(),
            raw_schema_tokens,
            schema_tokens: progressive::schema_tokens(&tools),
            deferred_schema_tokens: if progressive {
                deferred_schema_tokens
            } else {
                0
            },
            permitted_count: direct_count + deferred.len(),
            direct_count,
            deferred_count: deferred.len(),
            bridge_count,
            progressive,
        };
        ToolProjection {
            tools,
            deferred,
            diagnostics,
        }
    }

    /// The direct per-session projection before schema-budget disclosure.
    pub fn direct_llm_tools_for(&self, context: &ToolExposureContext) -> Vec<llm::Tool> {
        let mut out: Vec<llm::Tool> = self
            .tools
            .iter()
            .filter(|(name, _)| self.exposure_decision(context, name).is_visible())
            .map(|(_, entry)| entry.descriptor.as_ref().clone())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub(crate) fn resolve_model_call(
        &self,
        context: &ToolExposureContext,
        call: &ToolCall,
    ) -> ResolvedToolCall {
        if progressive::is_bridge_tool(&call.name)
            && self.approval.config().auto_deny.contains(&call.name)
        {
            return ResolvedToolCall {
                call: call.clone(),
                kind: ResolvedToolKind::Rejected(format!(
                    "approval denied for `{}`: tool is in auto_deny list",
                    call.name
                )),
            };
        }
        let projection = self.projection_for(context);
        match call.name.as_str() {
            progressive::TOOL_SEARCH | progressive::TOOL_DESCRIBE => {
                if projection.contains_model_tool(&call.name) {
                    ResolvedToolCall {
                        call: call.clone(),
                        kind: ResolvedToolKind::Catalog,
                    }
                } else {
                    ResolvedToolCall {
                        call: call.clone(),
                        kind: ResolvedToolKind::Rejected(format!(
                            "tool `{}` is unavailable for the current catalog",
                            call.name
                        )),
                    }
                }
            }
            progressive::TOOL_CALL => {
                let (target_name, input) = match progressive::resolve_call_envelope(&call.input) {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        return ResolvedToolCall {
                            call: call.clone(),
                            kind: ResolvedToolKind::Rejected(error),
                        }
                    }
                };
                let resolved = progressive::resolved_tool_call(call, target_name.clone(), input);
                if !projection.contains_model_tool(progressive::TOOL_CALL) {
                    return ResolvedToolCall {
                        call: resolved,
                        kind: ResolvedToolKind::Rejected(
                            "progressive tool invocation is unavailable for the current catalog"
                                .to_string(),
                        ),
                    };
                }
                if projection.deferred_entry(&target_name).is_none() {
                    return ResolvedToolCall {
                        call: resolved,
                        kind: ResolvedToolKind::Rejected(format!(
                            "tool `{target_name}` is not available in the current deferred catalog"
                        )),
                    };
                }
                ResolvedToolCall {
                    call: resolved,
                    kind: ResolvedToolKind::Registry,
                }
            }
            _ if projection.deferred_entry(&call.name).is_some() => ResolvedToolCall {
                call: call.clone(),
                kind: ResolvedToolKind::Rejected(format!(
                    "tool `{}` is deferred; invoke it through `{}`",
                    call.name,
                    progressive::TOOL_CALL
                )),
            },
            _ => ResolvedToolCall {
                call: call.clone(),
                kind: ResolvedToolKind::Registry,
            },
        }
    }

    pub(crate) fn is_parallel_safe_resolved(
        &self,
        context: &ToolExposureContext,
        resolved: &ResolvedToolCall,
    ) -> bool {
        match resolved.kind {
            ResolvedToolKind::Catalog => true,
            ResolvedToolKind::Registry => self.is_parallel_safe_for(context, &resolved.call.name),
            ResolvedToolKind::Rejected(_) => false,
        }
    }

    pub(crate) fn execute_catalog(
        &self,
        context: &ToolExposureContext,
        name: &str,
        input: &Value,
    ) -> ToolResult {
        let projection = self.projection_for(context);
        if !projection.contains_model_tool(name) {
            return ToolResult::err(format!(
                "tool `{name}` is unavailable for the current catalog"
            ));
        }
        match name {
            progressive::TOOL_SEARCH => progressive::search_tools(
                &projection.deferred,
                projection.diagnostics.catalog_generation,
                input,
            ),
            progressive::TOOL_DESCRIBE => progressive::describe_tool(
                &projection.deferred,
                projection.diagnostics.catalog_generation,
                input,
            ),
            _ => ToolResult::err(format!("tool `{name}` is not a catalog operation")),
        }
    }

    /// Every immutable descriptor, without session projection.
    pub fn as_llm_tools(&self) -> Vec<llm::Tool> {
        let mut out: Vec<llm::Tool> = self
            .tools
            .values()
            .filter(|entry| self.guardrails.permits(&entry.descriptor.name))
            .map(|entry| entry.descriptor.as_ref().clone())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn as_llm_tools_progressive(&self) -> Vec<llm::Tool> {
        let exposure = ToolExposureContext::isolated(self.guardrails.clone());
        self.as_llm_tools_for(&exposure)
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
        let approval_input = input.clone();
        self.execute_with_approval_input(context, name, input, approval_input, approval_reason)
            .await
    }

    pub(crate) async fn execute_with_approval_input(
        &self,
        context: &ToolExposureContext,
        name: &str,
        input: Value,
        approval_input: Value,
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
                .evaluate_for(
                    name,
                    &approval_input,
                    approval_reason,
                    tool.approval_boundary(),
                )
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
pub fn default_registry_with_deps(deps: &RegistryDeps) -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(super::builtin::Echo));
    r.register(Arc::new(super::builtin::Now));
    r.register(Arc::new(super::delegate::Delegate::new(deps.clone())));
    r.register(Arc::new(super::todo::Todo::new(
        super::todo::TodoStore::new(deps.paths.todos_dir.clone()),
    )));
    r.register(Arc::new(super::clarify::Clarify::new()));
    r.register(Arc::new(super::skills::SkillDisclosure::with_paths(
        deps.paths.system_skills_dir.clone(),
        deps.paths.user_skills_dir.clone(),
        deps.paths.skills_usage_path.clone(),
        deps.paths.system_skills_origin,
    )));
    r.register(Arc::new(super::cos_help::CosHelp));
    super::cos_proxy::register_all_with_notes(&mut r, deps.runtime.notes().clone());
    super::cos_apps::register_default_with_root(&mut r, deps.paths.apps_dir.clone());
    super::cos_apps_session::register_manifests(&mut r, &deps.paths.apps_dir, &deps.app_sessions);
    super::media::register_default_media_tools_with_outputs_dir(
        &mut r,
        deps.paths.media_outputs_dir.clone(),
    );
    if let Some(db) = &deps.memory {
        super::cos_proxy::register_recall(&mut r, db.clone());
        super::cos_proxy::register_app_memory(&mut r, db.clone());
    }
    if let Some(store) = &deps.semantic {
        super::cos_proxy::register_recall_semantic(&mut r, Arc::clone(store));
    }
    r
}

#[deprecated(note = "use default_registry_with_deps for explicit composition")]
pub fn default_registry() -> ToolRegistry {
    let deps = RegistryDeps::load_current();
    default_registry_with_deps(&deps)
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
