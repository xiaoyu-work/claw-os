//! Auto-triggered memory curator.
//!
//! After every turn that produces a final answer, the runtime
//! fires `MemoryCurator::curate_session(...)` in the background. The
//! curator itself short-circuits on `skip_if_no_new_messages` (its
//! default) so calling it after every turn is cheap when nothing new
//! has been said — the LLM call only fires when there ARE new
//! messages to extract facts from.
//!
//! Opt-in via `[agent] auxiliary_provider + auxiliary_model`. When
//! either is unset, [`AutoCurator::from_cfg_logged`] returns `None`
//! and the runtime simply doesn't auto-curate — the manual
//! `cos agent dev learn extract --session <sid>` workflow still
//! works.
//!
//! All errors (LLM down, MEMORY.md unwritable, etc.) are logged but
//! never propagate — curation is best-effort augmentation.

use std::sync::Arc;

use crate::agent::memory::curator::{default_log_path, MemoryCurator};
use crate::agent::memory::notes::NotesStore;
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::agent::runtime::loop_::auxiliary_from_cfg;
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
    /// Build from `[agent] auxiliary_*` config. Returns `None` when
    /// the auxiliary provider/model are unset (curation needs an LLM
    /// to extract facts, so without one there's nothing to do).
    pub fn from_cfg_logged(cfg: &AgentConfig, db: &MemoryDb) -> Option<Arc<Self>> {
        match auxiliary_from_cfg(cfg) {
            Ok(Some(aux)) => {
                let notes = NotesStore::system_default();
                let log_path = default_log_path();
                let curator = MemoryCurator::new(aux, notes, log_path);
                Some(Arc::new(Self {
                    curator: Arc::new(curator),
                    db: db.clone(),
                }))
            }
            Ok(None) => {
                tracing::debug!(
                    "curator: auxiliary not configured — auto-curation skipped \
                     (set agent.auxiliary_provider + auxiliary_model to enable)"
                );
                None
            }
            Err(e) => {
                tracing::warn!("curator: aux build failed ({e}); auto-curation skipped");
                None
            }
        }
    }

    /// Fire-and-forget curation pass over `session_id`. The curator
    /// reads recent messages from the DB, asks the auxiliary LLM to
    /// extract durable facts, and appends accepted facts to
    /// `MEMORY.md`. Logs at `info!` on a successful pass that
    /// actually added facts; logs at `warn!` on failure.
    pub fn spawn_curate(self: &Arc<Self>, session_id: String) {
        let curator = self.curator.clone();
        let db = self.db.clone();
        tokio::spawn(async move {
            match curator.curate_session(&db, &session_id, false).await {
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
