//! Execution context used by capability-aware consent.
//!
//! Existing capabilities remain the authority. This context only
//! decides whether a missing exact capability may be presented to a
//! human for approval; it never makes a denied capability sufficient.

/// Whether a human is expected to be present for this execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentContext {
    /// A user-initiated conversation with an approval surface.
    Attended,
    /// Scheduled, triggered, restored, or otherwise background work.
    Unattended,
}

impl ConsentContext {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attended => "attended",
            Self::Unattended => "unattended",
        }
    }
}
