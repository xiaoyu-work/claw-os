//! Capability model for Claw OS.
//!
//! Claw OS gates every privileged action behind one primitive: a
//! [`Cap`] — a `(verb, scope)` pair — held by the calling session in
//! its [`CapSet`]. The kernel, built-in apps, and third-party apps all
//! consume this same primitive; there is no second permission system
//! and no special "root" code path.
//!
//! ## Mental model
//!
//! ```text
//!  user                       agent                       kernel
//!  ──────────────────────────────────────────────────────────────
//!  picks a `Role`  ───▶  spawns session  ───▶  CapSet stored
//!     +  scopes              with CapSet           in proc registry
//!                                                       │
//!                                                       ▼
//!                                    every gated op calls
//!                                    `policy::require(verb, scope)`
//!                                    which consults the CapSet.
//! ```
//!
//! ## Submodules
//!
//! - [`verb`] — the closed set of [`Verb`]s the OS recognises.
//! - [`scope`] — [`Scope`] sum type plus cover/glob logic.
//! - [`risk`] — [`Risk`] rating used by the approval UI.
//! - [`cap`] — [`Cap`] and [`CapSet`] types.
//! - [`catalog`] — user-visible metadata for every verb (label, blurb,
//!   icon, risk, scope kind).
//! - [`role`] — built-in [`Role`] bundles ("worker", "automator", …)
//!   that expand to cap sets.
//! - [`denial`] — structured failure type returned by `require()`.
//!
//! The actual `require(...)` enforcement function will live in the
//! kernel-side `policy` module once the migration of existing call
//! sites lands. This crate ships the data layer first so the rest of
//! the system can begin moving over without a big-bang switch.

pub mod cap;
pub mod catalog;
pub mod denial;
pub mod risk;
pub mod role;
pub mod scope;
pub mod verb;

pub use cap::{Cap, CapSet};
pub use catalog::{lookup as lookup_meta, CapMeta, CATALOG};
pub use denial::{Denial, DenialReason};
pub use risk::Risk;
pub use role::{user_selectable, Role, ALL_ROLES};
pub use scope::{Scope, ScopeKind};
pub use verb::{Verb, ALL_VERBS};

/// Run all static self-checks on the cap subsystem. Intended to be
/// called once at boot (after [`crate::i18n::init_locale_from_env`])
/// so any author who forgets to update the catalog gets a loud, early
/// crash instead of a runtime mystery.
///
/// Returns `Ok(())` on success or a human-readable description of the
/// drift on failure. Callers can `unwrap()` in debug builds and log +
/// continue in release builds.
pub fn self_check() -> Result<(), String> {
    catalog::self_check()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_self_check_passes() {
        self_check().unwrap();
    }
}
