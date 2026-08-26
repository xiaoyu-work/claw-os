//! Structured reason a capability check denied an operation.
//!
//! Returning a typed `Denial` (instead of a stringly-typed error) lets
//! the kernel uniformly render the same information across:
//!   - audit logs (machine-readable JSON),
//!   - approval prompts (human-readable, localized),
//!   - LLM-facing error reports (so the agent can self-correct).

use crate::i18n::LocalizedStr;

use super::cap::{Cap, CapSet};
use super::scope::Scope;
use super::verb::Verb;

/// Why a `require()` failed.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Denial {
    /// Verb that was being attempted.
    pub verb: Verb,
    /// Scope the action wanted to act on.
    pub requested_scope: Scope,
    /// Caps the session actually held for this verb (helpful diagnostic).
    pub granted_scopes: Vec<Scope>,
    /// Why the denial happened.
    pub reason: DenialReason,
    /// Optional remediation hint, localized via [`LocalizedStr`].
    /// Kept as a plain `&'static str` when set from constants; dynamic
    /// hints go through the formatter.
    pub hint: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DenialReason {
    /// Session has no capability for the requested verb at all.
    VerbNotGranted,
    /// Session holds the verb but at scopes that don't cover the request.
    ScopeOutOfRange,
    /// Session does not exist or no `COS_SESSION` is set. Strict policy
    /// mode rejects rather than implicitly allowing.
    NoSession,
    /// Caller process is not the session's process tree (anti-spoofing).
    PidAncestryMismatch { caller_pid: u32, session_pid: u32 },
}

impl Denial {
    pub fn verb_not_granted(verb: Verb, requested_scope: Scope) -> Self {
        Self {
            verb,
            requested_scope,
            granted_scopes: vec![],
            reason: DenialReason::VerbNotGranted,
            hint: None,
        }
    }

    pub fn scope_out_of_range(
        verb: Verb,
        requested_scope: Scope,
        granted: &CapSet,
    ) -> Self {
        let granted_scopes = granted
            .iter()
            .filter(|c| c.verb == verb)
            .map(|c| c.scope.clone())
            .collect();
        Self {
            verb,
            requested_scope,
            granted_scopes,
            reason: DenialReason::ScopeOutOfRange,
            hint: None,
        }
    }

    pub fn no_session(verb: Verb, requested_scope: Scope) -> Self {
        Self {
            verb,
            requested_scope,
            granted_scopes: vec![],
            reason: DenialReason::NoSession,
            hint: None,
        }
    }

    pub fn pid_ancestry_mismatch(
        verb: Verb,
        requested_scope: Scope,
        caller_pid: u32,
        session_pid: u32,
    ) -> Self {
        Self {
            verb,
            requested_scope,
            granted_scopes: vec![],
            reason: DenialReason::PidAncestryMismatch {
                caller_pid,
                session_pid,
            },
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Localized one-line summary suitable for logs and CLI errors.
    pub fn summary(&self) -> String {
        let header = match self.reason {
            DenialReason::VerbNotGranted => SUMMARY_VERB_NOT_GRANTED.current(),
            DenialReason::ScopeOutOfRange => SUMMARY_SCOPE_OUT_OF_RANGE.current(),
            DenialReason::NoSession => SUMMARY_NO_SESSION.current(),
            DenialReason::PidAncestryMismatch { .. } => SUMMARY_PID_MISMATCH.current(),
        };
        format!(
            "{}: {} on {}",
            header,
            self.verb.as_str(),
            self.requested_scope
        )
    }

    /// Convenience for the legacy JSON-error layer used by the router:
    /// emit a `serde_json::Value` shaped for the existing error
    /// envelope.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "error": "permission denied",
            "verb": self.verb.as_str(),
            "requested_scope": self.requested_scope,
            "granted_scopes": self.granted_scopes,
            "reason": self.reason,
            "hint": self.hint,
            "summary": self.summary(),
        })
    }

    /// The full requested capability, useful when forwarding to an
    /// approval gate.
    pub fn requested_cap(&self) -> Cap {
        Cap::new(self.verb, self.requested_scope.clone())
    }
}

impl std::fmt::Display for Denial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())?;
        if let Some(h) = &self.hint {
            write!(f, " — {h}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Denial {}

const SUMMARY_VERB_NOT_GRANTED: LocalizedStr =
    LocalizedStr::new("Permission denied (capability not granted)");
const SUMMARY_SCOPE_OUT_OF_RANGE: LocalizedStr =
    LocalizedStr::new("Permission denied (outside granted scope)");
const SUMMARY_NO_SESSION: LocalizedStr =
    LocalizedStr::new("Permission denied (no active session)");
const SUMMARY_PID_MISMATCH: LocalizedStr =
    LocalizedStr::new("Permission denied (process tree mismatch)");

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/denial.rs"
    ));
}
