//! Semantic memory backed by an [`Embedder`] (cloud or local).
//!
//! The storage engine and trait surface live in the workspace-internal
//! [`claw_embed`] crate. This module is the **config-aware adapter**:
//! it re-exports the engine types so existing call-sites keep working,
//! and adds an extension trait that opens the store at the system
//! default path (`<data_dir>/agent/semantic.db`) with the configured
//! default embedder ([`crate::config::EmbedConfig`]).
//!
//! Why split?
//!
//! The daemon at `crates/claw-semantic` (and any future embed
//! consumer) needs the [`SemanticStore`] type and [`Embedder`] trait
//! but **must not** depend on the rest of `core` (TOML/JSONC config
//! globals, paths layout, agent runtime). Putting the storage engine
//! in a leaf crate keeps that boundary clean while letting `cos` itself
//! retain its config-driven convenience constructors.

use std::path::PathBuf;
use std::sync::Arc;

// Re-export the storage engine + trait surface so callers can keep
// using `crate::agent::memory::semantic::{SemanticStore, SemanticHit,
// ...}` exactly as before. Everything is defined in `claw-embed`.
pub use claw_embed::{
    SemanticError, SemanticHit, SemanticRow, SemanticStore, MAX_EMBED_TEXT_CHARS,
};

/// Default path under the cos data dir: `<data_dir>/agent/semantic.db`.
pub fn default_path() -> PathBuf {
    crate::paths::agent_semantic_db_path()
}

/// Open the store at the system default path with the configured
/// default embedder. Returns `Ok(None)` if embedding is disabled
/// (`[embed].provider = "none"`), or if `provider = "auto"` and the
/// main `[agent]` provider isn't OpenAI-shape.
///
/// Free function rather than an inherent method because
/// [`SemanticStore`] is defined in `claw_embed` (orphan rule prevents
/// adding inherent methods cross-crate). Most callers pull this in
/// via the [`SemanticStoreExt`] trait below so the original
/// `SemanticStore::open_default()` call-site shape keeps working.
pub fn open_default() -> Result<Option<SemanticStore>, SemanticError> {
    let config = crate::config::get();
    open_with_config(
        &config.embed,
        &config.agent,
        crate::paths::agent_semantic_db_path(),
    )
}

/// Open a semantic store from an explicit configuration snapshot and path.
pub fn open_with_config(
    embed: &crate::config::EmbedConfig,
    agent: &crate::config::AgentConfig,
    path: impl Into<PathBuf>,
) -> Result<Option<SemanticStore>, SemanticError> {
    let embedder = match crate::model::tasks::embed::build_from_with_agent(embed, agent) {
        Ok(Some(e)) => e,
        Ok(None) => return Ok(None),
        Err(e) => return Err(SemanticError::Embed(e)),
    };
    let store = SemanticStore::open(path.into(), Some(Arc::from(embedder)))?;
    Ok(Some(store))
}

/// Open the default-path store **without** consulting the embed
/// config. Used by maintenance commands (e.g. `semantic clear-all`)
/// that must work even when the embedder is misconfigured or broken —
/// the whole point of clear-all is to recover from a broken state.
pub fn open_default_without_embedder() -> Result<SemanticStore, SemanticError> {
    SemanticStore::open(default_path(), None)
}

/// Extension trait that lets call-sites keep the original
/// `SemanticStore::open_default()` / `::open_default_without_embedder()`
/// shape. With this trait `use`d, the short associated-function syntax
/// resolves to these methods. Without the trait in scope, fall back to
/// the free functions [`open_default`] / [`open_default_without_embedder`].
pub trait SemanticStoreExt: Sized {
    fn open_default() -> Result<Option<Self>, SemanticError>;
    fn open_default_without_embedder() -> Result<Self, SemanticError>;
}

impl SemanticStoreExt for SemanticStore {
    fn open_default() -> Result<Option<Self>, SemanticError> {
        open_default()
    }
    fn open_default_without_embedder() -> Result<Self, SemanticError> {
        open_default_without_embedder()
    }
}
