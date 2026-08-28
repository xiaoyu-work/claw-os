use std::path::PathBuf;
use std::sync::Arc;

use super::hooks::HookRegistry;
use super::semantic_indexer::SemanticIndexer;

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub hooks_config: PathBuf,
    pub audit_log: PathBuf,
    pub notes_dir: PathBuf,
    pub nudges_path: PathBuf,
    pub system_skills_dir: PathBuf,
    pub user_skills_dir: PathBuf,
    pub system_skills_origin: crate::agent::skills::loader::SkillOrigin,
}

impl RuntimePaths {
    pub fn from_process() -> Self {
        Self {
            hooks_config: crate::paths::agent_hooks_path(),
            audit_log: crate::paths::agent_audit_log_path(),
            notes_dir: crate::paths::agent_notes_dir(),
            nudges_path: crate::paths::agent_nudges_path(),
            system_skills_dir: crate::paths::system_skills_dir(),
            user_skills_dir: crate::paths::agent_skills_dir(),
            system_skills_origin: if std::env::var_os("COS_SYSTEM_SKILLS_DIR").is_some() {
                crate::agent::skills::loader::SkillOrigin::Local
            } else {
                crate::agent::skills::loader::SkillOrigin::BuiltIn
            },
        }
    }
}

#[derive(Clone)]
pub struct RuntimeDeps {
    hooks: HookRegistry,
    clock: Arc<dyn Clock>,
    semantic_indexer: Option<Arc<SemanticIndexer>>,
    paths: Option<RuntimePaths>,
    _auto_hook_guard: Option<Arc<super::hooks_config::AutoHookGuard>>,
}

impl RuntimeDeps {
    pub fn new(
        hooks: HookRegistry,
        clock: Arc<dyn Clock>,
        semantic_indexer: Option<Arc<SemanticIndexer>>,
    ) -> Self {
        Self {
            hooks,
            clock,
            semantic_indexer,
            paths: None,
            _auto_hook_guard: None,
        }
    }

    pub fn isolated() -> Self {
        Self::new(HookRegistry::new(), Arc::new(SystemClock), None)
    }

    /// Compatibility context for legacy public runtime adapters.
    ///
    /// Production composition roots use [`Self::load`] and pass the result to
    /// `run_with_deps`; this preserves the historical global-hook behavior for
    /// direct library callers while keeping it out of core request flows.
    pub fn compatibility(recording: bool) -> Self {
        use crate::agent::memory::semantic::{SemanticStore, SemanticStoreExt};

        let semantic = if recording {
            match SemanticStore::open_default() {
                Ok(store) => store.map(Arc::new),
                Err(error) => {
                    tracing::warn!(
                        "semantic: open_default failed ({error}); auto-indexing skipped"
                    );
                    None
                }
            }
        } else {
            None
        };
        Self::load_with_hooks(
            &RuntimePaths::from_process(),
            semantic,
            super::hooks::global_registry(),
        )
    }

    pub fn load(
        paths: &RuntimePaths,
        semantic_store: Option<Arc<crate::agent::memory::semantic::SemanticStore>>,
    ) -> Self {
        Self::load_with_hooks(paths, semantic_store, HookRegistry::new())
    }

    pub fn load_with_hooks(
        paths: &RuntimePaths,
        semantic_store: Option<Arc<crate::agent::memory::semantic::SemanticStore>>,
        hooks: HookRegistry,
    ) -> Self {
        let config = super::hooks_config::load(&paths.hooks_config).unwrap_or_default();
        let names = super::hooks_config::register_into_at(&hooks, &config, &paths.audit_log);
        let semantic_indexer = semantic_store.map(SemanticIndexer::from_shared_store);
        let mut deps = Self::new(hooks, Arc::new(SystemClock), semantic_indexer);
        deps.paths = Some(paths.clone());
        deps._auto_hook_guard = Some(Arc::new(super::hooks_config::AutoHookGuard::new(
            deps.hooks.clone(),
            names,
        )));
        deps
    }

    pub fn hooks(&self) -> &HookRegistry {
        &self.hooks
    }

    pub fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    pub fn semantic_indexer(&self) -> Option<Arc<SemanticIndexer>> {
        self.semantic_indexer.clone()
    }

    pub fn paths(&self) -> Option<&RuntimePaths> {
        self.paths.as_ref()
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/deps.rs"
    ));
}
