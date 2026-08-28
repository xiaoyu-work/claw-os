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
use std::path::PathBuf;
use std::sync::Arc;

use super::guardrails::Guardrails;
use super::Tool;
use crate::agent::llm;
use crate::agent::runtime::approval::ApprovalGate;
use crate::config::CosConfig;

/// Filesystem locations consumed by tools in the default registry.
///
/// Process environment and routed-path state are resolved once by
/// [`RegistryDeps::load`] at the composition boundary. Registry construction
/// and tool execution then use this immutable snapshot.
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
        }
    }
}

/// Explicit resources used to assemble the default tool registry.
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

    /// Resolve paths, discover App-session manifests, and open optional stores.
    ///
    /// This is intentionally separate from [`default_registry`]: callers can
    /// perform process-state reads and fallible I/O at a visible composition
    /// boundary, while registry assembly itself remains deterministic.
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
        let app_sessions = crate::apps::discover(&paths.apps_dir)
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
        );
        Self {
            config,
            paths,
            memory,
            semantic,
            runtime,
            app_sessions,
        }
    }

    /// Side-effect-free dependency set for tests and deliberately minimal
    /// compositions.
    pub fn without_optional_resources(config: Arc<CosConfig>, paths: RegistryPaths) -> Self {
        let runtime =
            crate::agent::runtime::deps::RuntimeDeps::load(&paths.runtime_paths(), None);
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
/// - `cos_help`, the read-only progressive view of the public CLI tree.
/// - All cos kernel primitive proxies (sandbox, proc, sysinfo, credential,
///   cron, checkpoint, service, trace, watch, ipc, browser, netfilter,
///   policy, model). Each proxy gives the model the exact same surface as
///   the cos CLI for that primitive.
/// - The compact `cos_app_catalog` / `cos_app_run` progressive App gateways
///   plus any explicitly active stateful App-session tools.
/// - `cos_memory` (notes) and, if the default memory DB opens cleanly,
///   `cos_recall` (FTS5 history search).
pub fn default_registry(deps: &RegistryDeps) -> ToolRegistry {
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
    super::cos_apps::register_default(&mut r);
    super::cos_apps_session::register_manifests(
        &mut r,
        &deps.paths.apps_dir,
        &deps.app_sessions,
    );
    super::media::register_default_media_tools(&mut r, deps.paths.media_outputs_dir.clone());
    if let Some(db) = &deps.memory {
        super::cos_proxy::register_recall(&mut r, db.clone());
        super::cos_proxy::register_app_memory(&mut r, db.clone());
    }
    if let Some(store) = &deps.semantic {
        super::cos_proxy::register_recall_semantic(&mut r, Arc::clone(store));
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
