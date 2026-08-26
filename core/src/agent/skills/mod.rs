//! Layered Agent Skill registry and progressive disclosure.
//!
//! Read-only vendor skills are loaded from `/usr/lib/cos/skills`; user skills
//! are loaded from the per-user Agent state directory. The prompt sees only
//! manifest metadata. Full instructions and child resources are exposed
//! through `cos_skill` one layer at a time.

pub mod disclosure;
pub mod hub;
pub mod loader;
pub mod manifest;
pub mod provenance;
pub mod sync;
