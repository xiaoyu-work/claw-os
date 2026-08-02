//! Auto-triggered memory curator.
//!
//! After every turn that produces a final answer, the runtime
//! fires `MemoryCurator::curate_session(...)` in the background. The
//! curator itself short-circuits on `skip_if_no_new_messages` (its
//! default) so calling it after every turn is cheap when nothing new
//! has been said — the LLM call only fires when there ARE new
//! messages to extract facts from.
//!
//! On by default. When `[agent].auxiliary_provider` is set the
//! curator routes through it (cheap subtask path); otherwise it
//! falls back to the main `[agent].provider + .model` so the
//! curator works out of the box for every configured agent. The
//! one exception is `provider = "mock"`: there's no point spending
//! cycles on a mock LLM, so curation is silently skipped in tests
//! and on fresh-out-of-the-box installs.
//!
//! All errors (LLM down, MEMORY.md unwritable, etc.) are logged but
//! never propagate — curation is best-effort augmentation.

use std::sync::Arc;

use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
use crate::agent::llm::registry as llm_registry;
use crate::agent::memory::curator::{default_log_path, MemoryCurator};
use crate::agent::memory::notes::NotesStore;
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::agent::runtime::loop_::{auxiliary_from_cfg, AgentError};
use crate::config::AgentConfig;

/// Wraps a [`MemoryCurator`] + [`MemoryDb`] with a fire-and-forget
/// `spawn_curate` surface. Cheap to clone (two `Arc`s + cloned DB
/// handle, all internally `Arc<Mutex<_>>`).
#[derive(Clone)]
pub struct AutoCurator {
    curator: Arc<MemoryCurator>,
    db: MemoryDb,
}

impl AutoCurator {
    /// Build from `cfg`. Returns `None` only when **both** the
    /// auxiliary path and the main-provider fallback are unusable —
    /// i.e. the main provider is `mock` or build fails. Errors are
    /// logged at `warn!` and downgrade to `None`.
    pub fn from_cfg_logged(cfg: &AgentConfig, db: &MemoryDb) -> Option<Arc<Self>> {
        let aux = match auxiliary_from_cfg(cfg) {
            Ok(Some(a)) => a,
            Ok(None) => match aux_from_main(cfg) {
                Ok(Some(a)) => a,
                Ok(None) => {
                    tracing::debug!(
                        "curator: main provider is mock — auto-curation skipped"
                    );
                    return None;
                }
                Err(e) => {
                    tracing::warn!(
                        "curator: main-provider fallback build failed ({e}); \
                         auto-curation skipped"
                    );
                    return None;
                }
            },
            Err(e) => {
                tracing::warn!("curator: aux build failed ({e}); auto-curation skipped");
                return None;
            }
        };
        let notes = NotesStore::system_default();
        let log_path = default_log_path();
        let curator = MemoryCurator::new(aux, notes, log_path);
        Some(Arc::new(Self {
            curator: Arc::new(curator),
            db: db.clone(),
        }))
    }

    /// Fire-and-forget curation pass over `session_id`. The curator
    /// reads recent messages from the DB, asks the auxiliary LLM to
    /// extract durable facts, and appends accepted facts to
    /// `MEMORY.md`. Logs at `info!` on a successful pass that
    /// actually added facts; logs at `warn!` on failure.
    ///
    /// Spawned via [`runtime::background::spawn`] so a
    /// `runtime::background::drain` call (used by the one-shot
    /// `cos agent ask` CLI path) can await completion before the
    /// tokio runtime is dropped. Without the registry the curator
    /// would be cancelled mid-LLM-call by runtime shutdown.
    pub fn spawn_curate(self: &Arc<Self>, session_id: String) {
        let curator = self.curator.clone();
        let db = self.db.clone();
        let trusted_session = crate::proc::current_trusted_session_for_caps();
        crate::agent::runtime::background::spawn(async move {
            let curate_session_id = session_id.clone();
            let run = async move {
                curator
                    .curate_session(&db, &curate_session_id, false)
                    .await
            };
            let result = match trusted_session {
                Some(session) => {
                    crate::proc::with_trusted_session_override(session, run).await
                }
                None => run.await,
            };
            match result {
                Ok(outcome) => {
                    if outcome.skipped_no_new_messages {
                        tracing::trace!(
                            "curator: session={session_id} skipped (no new messages)"
                        );
                    } else if !outcome.facts_added.is_empty() {
                        tracing::info!(
                            "curator: session={} examined={} facts_added={}",
                            session_id,
                            outcome.messages_examined,
                            outcome.facts_added.len()
                        );
                    } else {
                        tracing::debug!(
                            "curator: session={} examined={} facts_added=0 \
                             (no new durable facts found)",
                            session_id,
                            outcome.messages_examined,
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("curator: session={session_id} extract failed: {e}");
                }
            }
        });
    }
}

/// Build an [`AuxiliaryClient`] from the **main** `[agent]` provider
/// + model — the default-on fallback used when the auxiliary block
/// is unset. Returns `Ok(None)` when the main provider is `mock`
/// (no point curating against canned responses, and tests rely on
/// this short-circuit). Returns `Err` when the build itself fails
/// (e.g. credential lookup error).
fn aux_from_main(cfg: &AgentConfig) -> Result<Option<AuxiliaryClient>, AgentError> {
    if cfg.provider == "mock" || cfg.provider.is_empty() {
        // No auxiliary curator when there's no real LLM behind the main
        // provider — either it's the canned-response `mock` or the OS
        // owner has not yet picked one. Curating against either is
        // pointless and just spends cycles.
        return Ok(None);
    }
    if cfg.model.trim().is_empty() {
        return Err(AgentError::Internal(
            "main agent.model is empty — curator fallback cannot build".into(),
        ));
    }
    let provider = llm_registry::build(&cfg.provider, &cfg.model, cfg)
        .map_err(|e| AgentError::Internal(format!("aux fallback build: {e}")))?;
    let provider = crate::ai::gate::wrap_for_system(provider);
    let mut acfg = AuxiliaryConfig::new(&cfg.provider, &cfg.model)
        .with_max_tokens(cfg.auxiliary_max_tokens);
    if let Some(t) = cfg.auxiliary_temperature {
        acfg = acfg.with_temperature(t);
    }
    Ok(Some(AuxiliaryClient::new(provider, acfg)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::memory::sqlite_fts::MemoryDb;

    fn mem_db() -> MemoryDb {
        MemoryDb::open_in_memory().expect("in-memory db")
    }

    /// Curator stays off when the main provider is unconfigured (empty
    /// — the default for fresh installs) or `mock`. Avoids spending
    /// cycles on canned/non-existent responses.
    #[test]
    fn auto_curator_disabled_when_main_is_unconfigured() {
        let cfg = AgentConfig::default();
        assert!(cfg.provider.is_empty());
        assert!(AutoCurator::from_cfg_logged(&cfg, &mem_db()).is_none());
    }

    #[test]
    fn auto_curator_disabled_when_main_is_mock() {
        let mut cfg = AgentConfig::default();
        cfg.provider = "mock".into();
        cfg.model = "mock-model".into();
        assert!(AutoCurator::from_cfg_logged(&cfg, &mem_db()).is_none());
    }

    /// When `auxiliary_provider` is unset but the main provider is a
    /// real LLM, the curator falls back to it — that's the
    /// default-on path the user asked for.
    #[test]
    fn auto_curator_falls_back_to_main_when_aux_unset() {
        let mut cfg = AgentConfig::default();
        cfg.provider = "openai".into();
        cfg.model = "gpt-4o-mini".into();
        cfg.api_key_env = Some("OPENAI_API_KEY".into());
        assert!(cfg.auxiliary_provider.is_none());
        assert!(AutoCurator::from_cfg_logged(&cfg, &mem_db()).is_some());
    }

    /// Explicit `auxiliary_provider` still wins (the fallback is a
    /// pure default-on path; it does not override an explicit
    /// setting).
    #[test]
    fn auto_curator_respects_explicit_aux() {
        let mut cfg = AgentConfig::default();
        cfg.provider = "openai".into();
        cfg.model = "gpt-4o".into();
        cfg.auxiliary_provider = Some("openai".into());
        cfg.auxiliary_model = Some("gpt-4o-mini".into());
        cfg.api_key_env = Some("OPENAI_API_KEY".into());
        assert!(AutoCurator::from_cfg_logged(&cfg, &mem_db()).is_some());
    }

    /// `aux_from_main` errors when the main model is empty — we
    /// shouldn't silently swallow a malformed config.
    #[test]
    fn aux_from_main_errors_when_model_empty() {
        let mut cfg = AgentConfig::default();
        cfg.provider = "openai".into();
        cfg.model = String::new();
        assert!(aux_from_main(&cfg).is_err());
    }
}
