//! Where a capability denial goes when the process cannot reach the
//! consent store itself.
//!
//! In-process callers file and spend approvals directly against
//! `<caps-data>/approvals`, which is root-owned. An `agentd` worker runs
//! as the task owner with no access to that tree and no broker route, so
//! it installs a gateway that carries exactly two questions to `clawd`
//! over its private job channel: "is there an approved grant for this
//! exact verb and scope?" and "file or reuse a pending request for it".
//!
//! The gateway never carries a session, an owner, a task, a decision or
//! a capability set. It may carry a digest of validated operation inputs,
//! never the raw arguments. The broker derives identity from the verified
//! job grant, so a worker cannot request against another session, spend
//! another owner's grant, or hand itself authority. A gateway that cannot
//! answer leaves the gate closed.

use std::sync::{Arc, RwLock};

use super::{ConsentContext, Scope, Verb};

/// Outcome of filing (or reusing) a pending approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    /// Bounded identifier the user will act on. `None` when the broker
    /// mediated the request but chose not to disclose an id.
    pub request_id: Option<String>,
}

/// Consent mediation for a process that cannot touch the approvals
/// store. Both calls are short, bounded round-trips; neither holds the
/// worker while a human decides. The worker reports the filed request
/// to `clawd`, which persists and resumes the task through its queue.
pub trait ApprovalGateway: Send + Sync + std::fmt::Debug {
    /// Trusted execution context supplied by the broker assignment.
    /// The broker independently enforces the same value.
    fn context(&self) -> ConsentContext;

    /// Spend an exactly-matching approved grant, if one exists. `true`
    /// means the gate may proceed this once.
    fn consume(
        &self,
        verb: Verb,
        scope: &Scope,
        operation_digest: Option<&str>,
    ) -> Result<bool, String>;

    /// File or reuse a pending request for this exact verb and scope.
    fn request(
        &self,
        verb: Verb,
        scope: &Scope,
        operation_digest: Option<&str>,
    ) -> Result<PendingApproval, String>;
}

static GATEWAY: RwLock<Option<Arc<dyn ApprovalGateway>>> = RwLock::new(None);

/// Install the process-wide gateway. Called once by `claw-agentd`
/// before the agent runtime starts; never by `clawd` or the CLI, which
/// reach the store directly.
pub(crate) fn install(gateway: Arc<dyn ApprovalGateway>) {
    if let Ok(mut slot) = GATEWAY.write() {
        *slot = Some(gateway);
    }
}

pub(crate) fn installed() -> Option<Arc<dyn ApprovalGateway>> {
    GATEWAY.read().ok().and_then(|slot| slot.clone())
}

#[cfg(test)]
pub(crate) fn clear_for_test() {
    if let Ok(mut slot) = GATEWAY.write() {
        *slot = None;
    }
}
