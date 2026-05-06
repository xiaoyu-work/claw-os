//! Engine package source adapters.
//!
//! Each submodule is a concrete way to discover and fetch engine
//! release archives:
//!   - `github`: GitHub Releases REST API
//!   - `asset_select`: per-engine asset filename rules (host-aware)
//!
//! Future: `manifest` (signed JSON manifest server), `local_mirror`
//! (corp internal artifact store).

pub mod asset_select;
pub mod github;
