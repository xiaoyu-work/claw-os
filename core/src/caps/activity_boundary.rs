//! Live Activity constraints. A binding records policy, never permission.

use std::future::Future;
use std::sync::Arc;

use crate::activities::{ActivityCapabilityPolicy, ActivityService, CapabilityBoundaryDecision};

use super::{Cap, CapSet, Scope, Verb};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PolicyBinding {
    pub(crate) activity_id: String,
    pub(crate) owner_uid: u32,
    pub(crate) revision: Option<u64>,
    confirmed_caps: CapSet,
}

pub(crate) struct ActivityBoundary {
    binding: PolicyBinding,
    session_id: Option<String>,
    service: Arc<dyn ActivityService>,
}

impl std::fmt::Debug for ActivityBoundary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityBoundary")
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

tokio::task_local! {
    static CURRENT: Option<Arc<ActivityBoundary>>;
}

pub(crate) async fn scope<F: Future>(
    boundary: Option<Arc<ActivityBoundary>>,
    future: F,
) -> F::Output {
    CURRENT.scope(boundary, future).await
}

pub(crate) fn current() -> Option<Arc<ActivityBoundary>> {
    CURRENT.try_with(Clone::clone).ok().flatten()
}

impl ActivityBoundary {
    pub(crate) fn for_job(job: &crate::agent::service::Job) -> Result<Option<Arc<Self>>, String> {
        let Some(activity_id) = job.activity_id.as_deref() else {
            return Ok(None);
        };
        let owner = job
            .owner_uid
            .ok_or_else(|| "Activity capability policy requires an owner".to_string())?;
        Self::open(owner, activity_id, job.session_id.clone()).map(Some)
    }

    pub(crate) fn for_session(owner: u32, session_id: &str) -> Result<Option<Arc<Self>>, String> {
        if let Some(boundary) = current() {
            boundary.require_identity(owner, Some(session_id))?;
            return Ok(Some(boundary));
        }
        let Ok(id) = session_id.parse::<crate::session::SessionId>() else {
            // Process-only launch identities do not identify durable sessions.
            return Ok(None);
        };
        let meta = match crate::session::get_meta(&id) {
            Ok(meta) => meta,
            Err(error) => return Err(format!("read Activity session association: {error}")),
        };
        let Some(activity_id) = meta.activity_id.as_deref() else {
            return Ok(None);
        };
        if meta.id != id || meta.owner_uid != Some(owner) {
            return Err("Activity session belongs to a different owner".to_string());
        }
        Self::open(owner, activity_id, Some(session_id.to_string())).map(Some)
    }

    fn open(
        owner: u32,
        activity_id: &str,
        session_id: Option<String>,
    ) -> Result<Arc<Self>, String> {
        let service: Arc<dyn ActivityService> =
            Arc::new(crate::activities::open_default().map_err(|error| error.to_string())?);
        Self::with_service(owner, activity_id, session_id, service)
    }

    fn with_service(
        owner: u32,
        activity_id: &str,
        session_id: Option<String>,
        service: Arc<dyn ActivityService>,
    ) -> Result<Arc<Self>, String> {
        let activity = service
            .get(owner, activity_id)
            .map_err(|error| format!("read Activity capability boundary: {error}"))?;
        let policy = service
            .capability_policy(owner, &activity.id)
            .map_err(|error| format!("read Activity capability policy: {error}"))?;
        let boundary = Arc::new(Self {
            binding: PolicyBinding {
                activity_id: activity.id,
                owner_uid: owner,
                revision: policy.as_ref().map(|policy| policy.revision),
                confirmed_caps: CapSet::new(),
            },
            session_id,
            service,
        });
        boundary.check()?;
        Ok(boundary)
    }

    pub(crate) fn binding(&self) -> PolicyBinding {
        self.binding.clone()
    }

    pub(crate) fn binding_for_authorized(&self, caps: &CapSet) -> Result<PolicyBinding, String> {
        let mut binding = self.binding();
        binding.confirmed_caps = CapSet::from_caps(self.approval_needs(caps)?);
        Ok(binding)
    }

    pub(crate) fn require_identity(
        &self,
        owner: u32,
        session_id: Option<&str>,
    ) -> Result<(), String> {
        if self.binding.owner_uid != owner || self.session_id.as_deref() != session_id {
            return Err("Activity capability boundary does not match the task identity".into());
        }
        Ok(())
    }

    pub(crate) fn policy(&self) -> Result<Option<ActivityCapabilityPolicy>, String> {
        let policy = self
            .service
            .capability_policy(self.binding.owner_uid, &self.binding.activity_id)
            .map_err(|error| format!("read live Activity capability policy: {error}"))?;
        if policy.as_ref().is_some_and(|policy| {
            policy.owner_uid != self.binding.owner_uid
                || policy.activity_id != self.binding.activity_id
        }) {
            return Err("Activity capability policy identity is inconsistent".into());
        }
        if policy.as_ref().is_some_and(|policy| !policy.enabled) {
            return Err("Activity capability policy is disabled".into());
        }
        if policy.as_ref().map(|policy| policy.revision) != self.binding.revision {
            return Err(
                "Activity capability policy changed; a fresh execution attempt is required".into(),
            );
        }
        Ok(policy)
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        self.policy().map(|_| ())
    }

    pub(crate) fn decision(&self, requested: &Cap) -> Result<CapabilityBoundaryDecision, String> {
        Ok(self
            .policy()?
            .map_or(CapabilityBoundaryDecision::Normal, |policy| {
                live_decision(&policy, requested)
            }))
    }

    pub(crate) fn approval_needs(&self, caps: &CapSet) -> Result<Vec<Cap>, String> {
        let policy = self.policy()?;
        let mut approvals = Vec::new();
        for cap in caps.iter() {
            match policy
                .as_ref()
                .map_or(CapabilityBoundaryDecision::Normal, |policy| {
                    live_decision(policy, cap)
                }) {
                CapabilityBoundaryDecision::Normal => {}
                CapabilityBoundaryDecision::RequireApproval => approvals.push(cap.clone()),
                CapabilityBoundaryDecision::Deny => {
                    return Err(format!(
                        "Activity capability policy denies {}; an approval cannot override it",
                        cap.verb.as_str()
                    ));
                }
            }
        }
        Ok(approvals)
    }
}

fn live_decision(policy: &ActivityCapabilityPolicy, requested: &Cap) -> CapabilityBoundaryDecision {
    let decision = policy.decision(requested);
    if decision != CapabilityBoundaryDecision::Deny
        && matches!(requested.scope, Scope::Path(_))
        && policy
            .rules
            .iter()
            .find(|rule| rule.verb == requested.verb)
            .is_some_and(|rule| {
                !rule
                    .scopes
                    .iter()
                    .any(|scope| runtime_scope_covers(requested.verb, scope, &requested.scope))
            })
    {
        return CapabilityBoundaryDecision::Deny;
    }
    decision
}

fn runtime_scope_covers(verb: Verb, boundary: &Scope, requested: &Scope) -> bool {
    crate::activities::capability_scope_covers(verb, boundary, requested)
        // Executable paths also use the ordinary symlink-aware check. Pure
        // matching alone cannot see an alias escaping the configured subtree.
        && (!matches!(requested, Scope::Path(_)) || boundary.covers(requested))
}

impl PolicyBinding {
    pub(crate) fn check(&self) -> Result<(), String> {
        self.boundary()?.check()
    }

    pub(crate) fn check_delegated(&self, required: &[Cap]) -> Result<(), String> {
        // App approval was settled once for its invocation/call, before this
        // binding was installed on a grant. Effects recheck policy, not consent.
        let approvals = self
            .boundary()?
            .approval_needs(&CapSet::from_caps(required.iter().cloned()))?;
        if approvals.iter().any(|cap| {
            !self.confirmed_caps.iter().any(|confirmed| {
                confirmed.verb == cap.verb
                    && runtime_scope_covers(cap.verb, &confirmed.scope, &cap.scope)
            })
        }) {
            return Err(
                "Activity confirmation must be settled by an authorized App invocation or call"
                    .into(),
            );
        }
        Ok(())
    }

    fn boundary(&self) -> Result<ActivityBoundary, String> {
        let service: Arc<dyn ActivityService> =
            Arc::new(crate::activities::open_default().map_err(|error| error.to_string())?);
        Ok(ActivityBoundary {
            binding: self.clone(),
            session_id: None,
            service,
        })
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/activity_boundary.rs"
    ));
}
