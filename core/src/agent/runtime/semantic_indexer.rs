//! Best-effort background semantic indexer.
//!
//! Every message recorded into [`crate::agent::memory::sqlite_fts::MemoryDb`]
//! is also mirrored into [`crate::agent::memory::semantic::SemanticStore`]
//! so the LLM can do similarity search via the `cos_recall_semantic`
//! tool. Indexing is fire-and-forget — failures are logged at `warn!`
//! and never propagate to the turn loop.
//!
//! Skipped silently when `[embed]` is unconfigured (the common
//! pre-setup case). The runtime calls [`SemanticIndexer::from_default_logged`]
//! once per ask; when it returns `None`, every subsequent
//! [`SemanticIndexer::spawn_index`] is a no-op (callers check the
//! `Option` before invoking).
//!
//! ## Key scheme
//!
//! Each row is stored under `namespace = "session/<sid>"` with
//! `key = "<role>-<sqlite_rowid>"`. Using the SQLite rowid as the
//! key makes re-indexing idempotent (upserts the same row) and lets
//! offline jobs cross-reference semantic hits back to the FTS5 table.

use std::sync::Arc;

use crate::agent::memory::semantic::{SemanticError, SemanticStore, SemanticStoreExt};

/// Wraps a [`SemanticStore`] with a fire-and-forget `spawn_index`
/// surface. Cheap to clone (single `Arc`).
#[derive(Clone)]
pub struct SemanticIndexer {
    store: Arc<SemanticStore>,
}

impl SemanticIndexer {
    /// Open the default-path semantic store using the configured
    /// embedder. Returns `None` (and logs at `debug!`) when
    /// embedding is disabled in config; `None` and a `warn!` when
    /// the DB cannot be opened. The runtime treats both as "skip
    /// auto-indexing" and continues without semantic memory.
    pub fn from_default_logged() -> Option<Arc<Self>> {
        match SemanticStore::open_default() {
            Ok(Some(store)) => Some(Arc::new(Self {
                store: Arc::new(store),
            })),
            Ok(None) => {
                tracing::debug!("semantic: [embed] disabled — auto-indexing skipped");
                None
            }
            Err(e) => {
                tracing::warn!("semantic: open_default failed ({e}); auto-indexing skipped");
                None
            }
        }
    }

    /// Fire-and-forget upsert of one conversation message into the
    /// semantic store. Returns immediately; the actual embed + write
    /// runs on a background `tokio` task. Empty / whitespace-only
    /// text is silently dropped (embedding nothing wastes a request).
    ///
    /// Routes through [`runtime::background::spawn`] so the one-shot
    /// `cos agent ask` CLI path can drain pending embeds before the
    /// runtime drops. Without the registry these tasks would be
    /// cancelled mid-embed and the last turn's user/assistant
    /// messages would be missing from semantic search.
    pub fn spawn_index(
        self: &Arc<Self>,
        session_id: String,
        role: &str,
        msg_id: i64,
        text: String,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let store = self.store.clone();
        let namespace = format!("session/{session_id}");
        let key = format!("{role}-{msg_id}");
        let role_owned = role.to_string();
        let trusted_session = crate::proc::current_trusted_session_for_caps();
        crate::agent::runtime::background::spawn(async move {
            let run = async move {
                match store.index(&namespace, &key, &text).await {
                    Ok(_) => {
                        tracing::trace!(
                            "semantic: indexed {role_owned} msg {msg_id} (session={session_id})"
                        );
                    }
                    Err(SemanticError::Disabled) => {
                        // The store was built with embedder=None — nothing to do.
                    }
                    Err(e) => {
                        tracing::warn!(
                            "semantic: index failed for {key} (session={session_id}): {e}"
                        );
                    }
                }
            };
            match trusted_session {
                Some(session) => crate::proc::with_trusted_session_override(session, run).await,
                None => run.await,
            }
        });
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/semantic_indexer.rs"
    ));
}
