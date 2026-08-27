//! Who may ask for which event kind.
//!
//! The journal has exactly one writer — root `clawd` — but not every
//! caller inside that process speaks for the same authority. A frame
//! that arrived on an `agentd` worker's private channel is a *request*
//! to record a model or tool lifecycle fact; it must never be able to
//! state that a capability was issued, that an approval was granted or
//! that a privileged mutation committed.
//!
//! The check is structural: [`EventSource::may_write`] matches on the
//! event variant, so a new event kind does not compile until its ACL
//! row is decided. There is no name list to fall out of sync with the
//! schema and no default-allow arm.

use super::event::JournalEvent;

/// The authority a caller speaks with when it asks for an append.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventSource {
    /// Broker, capability authority, approvals store and privileged
    /// providers running inside root `clawd`. May record every kind.
    Kernel,
    /// A `claw-agentd` worker's private channel. May record only model
    /// and tool lifecycle, and only for the session named on its
    /// verified grant — the writer derives task, session and owner from
    /// the lease, never from the frame.
    Worker,
    /// The startup and resume scan. May record only what recovery
    /// concludes; it cannot invent lifecycle or authority facts.
    Recovery,
}

impl EventSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kernel => "kernel",
            Self::Worker => "worker",
            Self::Recovery => "recovery",
        }
    }

    /// Whether this source may record `event`.
    ///
    /// Deliberately exhaustive over the schema rather than a lookup:
    /// adding a variant to [`JournalEvent`] fails to compile until this
    /// table decides who owns it.
    pub fn may_write(self, event: &JournalEvent) -> bool {
        match self {
            Self::Kernel => true,
            Self::Worker => matches!(
                event,
                JournalEvent::ToolProposed { .. }
                    | JournalEvent::ToolStarted { .. }
                    | JournalEvent::ToolFinished { .. }
                    | JournalEvent::ModelTurnCompleted { .. }
            ),
            Self::Recovery => matches!(
                event,
                JournalEvent::MutationOrphaned { .. }
                    | JournalEvent::MutationIndeterminate { .. }
                    | JournalEvent::RecoveryScanned { .. }
                    | JournalEvent::RetentionApplied { .. }
            ),
        }
    }

    /// Whether an append from this source may draw on the capacity
    /// reserved for closing and recovering mutations.
    ///
    /// Only the kernel and the recovery scan may. Worker volume — which
    /// a model or a compromised tool controls — is refused before the
    /// reserve is touched, so a flood cannot stop a mutation from being
    /// closed.
    pub fn is_reserved_capacity(self) -> bool {
        matches!(self, Self::Kernel | Self::Recovery)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/acl.rs"
    ));
}
