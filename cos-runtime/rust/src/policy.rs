//! Permission helper — every claw-os bundled app or desktop binary
//! that touches a gated verb calls [`require`] before performing the
//! side effect. Mirrors the Python `cos_runtime.policy` module so
//! apps written in either language behave identically.
//!
//! ```no_run
//! use cos_runtime::policy;
//!
//! fn handle_rm(path: &str) -> Result<(), Box<dyn std::error::Error>> {
//!     policy::require("fs.delete", policy::Scope::path(path))?;
//!     std::fs::remove_file(path)?;
//!     Ok(())
//! }
//! ```
//!
//! [`require`] returns `Err(PolicyError::Denied(decision))` when the
//! kernel refuses; the inner [`Decision`] is the wire v1 perms reply.

use std::ffi::OsString;

use serde::{Deserialize, Serialize};

use crate::{cos_call_json, BridgeError};

/// Decision envelope returned by the hidden policy bridge. See
/// `wire/v1/perms.schema.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub decision: String,
    pub verb: String,
    #[serde(default)]
    pub scope: Option<serde_json::Value>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub granted: Option<bool>,
}

impl Decision {
    pub fn is_allow(&self) -> bool {
        self.decision == "allow"
    }
}

/// Errors returned by [`require`] and [`check`].
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// The kernel said no.
    #[error("permission denied: {} {:?}", .0.verb, .0.reason)]
    Denied(Decision),

    /// We couldn't even reach the kernel.
    #[error("policy unavailable: {0}")]
    Unavailable(String),

    /// Transport failure (cos binary, JSON decode, …).
    #[error(transparent)]
    Bridge(#[from] BridgeError),
}

/// Scope passed to [`require`] / [`check`]. Exactly one variant
/// corresponds to one `--path` / `--host` / `--name` / `--self` /
/// `--wild` flag on the hidden policy bridge.
#[derive(Debug, Clone)]
pub enum Scope {
    Path(String),
    Host(String),
    Name(String),
    SelfRef(String),
    Wild,
    Unscoped,
}

impl Scope {
    pub fn path(p: impl Into<String>) -> Self { Scope::Path(p.into()) }
    pub fn host(h: impl Into<String>) -> Self { Scope::Host(h.into()) }
    pub fn name(n: impl Into<String>) -> Self { Scope::Name(n.into()) }
    pub fn self_ref(s: impl Into<String>) -> Self { Scope::SelfRef(s.into()) }

    fn argv(&self) -> Vec<OsString> {
        match self {
            Scope::Path(v)    => vec!["--path".into(), v.into()],
            Scope::Host(v)    => vec!["--host".into(), v.into()],
            Scope::Name(v)    => vec!["--name".into(), v.into()],
            Scope::SelfRef(v) => vec!["--self".into(), v.into()],
            Scope::Wild       => vec!["--wild".into()],
            Scope::Unscoped   => vec![],
        }
    }
}

/// Ask the kernel whether the current process is allowed to perform
/// `verb` against `scope`. Returns `Ok(())` if allowed,
/// `Err(PolicyError::Denied(..))` if not.
pub fn require(verb: &str, scope: Scope) -> Result<(), PolicyError> {
    let decision = check(verb, scope)?;
    if decision.is_allow() {
        Ok(())
    } else {
        Err(PolicyError::Denied(decision))
    }
}

/// Same as [`require`] but returns the [`Decision`] envelope verbatim
/// — handy when the app wants to surface a "would-be-denied" hint
/// without aborting.
pub fn check(verb: &str, scope: Scope) -> Result<Decision, PolicyError> {
    let mut argv: Vec<OsString> = vec!["__policy".into(), "check".into(), verb.into()];
    argv.extend(scope.argv());

    let value = cos_call_json("policy", verb, argv).map_err(PolicyError::from)?;

    if value.get("decision").is_none() {
        return Err(PolicyError::Unavailable(format!(
            "policy check returned unrecognised envelope: {value}"
        )));
    }

    serde_json::from_value(value.clone()).map_err(|e| {
        PolicyError::Unavailable(format!("decision decode failed ({e}): {value}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_argv_shapes() {
        assert_eq!(Scope::Path("/etc".into()).argv(), &[OsString::from("--path"), OsString::from("/etc")]);
        assert_eq!(Scope::Wild.argv(), &[OsString::from("--wild")]);
        assert!(Scope::Unscoped.argv().is_empty());
    }

    #[test]
    fn decision_is_allow_helper() {
        let d = Decision { decision: "allow".into(), verb: "fs.read".into(), scope: None, reason: None, hint: None, granted: Some(true) };
        assert!(d.is_allow());
        let d = Decision { decision: "deny".into(), verb: "fs.read".into(), scope: None, reason: None, hint: None, granted: Some(false) };
        assert!(!d.is_allow());
    }
}
