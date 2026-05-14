//! Memory subsystem.
//!
//! Phase 3 layers (decided in Q6):
//!   - notes (MEMORY.md / USER.md)         — frozen snapshot
//!   - sqlite_fts                          — built-in, FTS5 + trigram
//!   - semantic                            — fastembed via crate::model::tasks::embed
//!   - honcho                              — only retained external plugin
//!   - curator                             — async fact extractor

pub mod curator;
pub mod honcho;
pub mod notes;
pub mod semantic;
pub mod sqlite_fts;
