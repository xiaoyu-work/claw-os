//! The authorization decision a route runs under.
//!
//! One decision is produced per request, before dispatch, by
//! [`super::authorize`]. It is the *only* authority a handler has: the
//! grant it resolved, the capabilities that grant carries, the subject
//! it acts for, and a keyed reference for the audit trail.
//!
//! Providers keep their own final checks — that is deliberate defence
//! in depth — but they now run them *through* this object rather than
//! by re-reading the process registry and re-deriving policy. There is
//! therefore one decision per request and one place it can be wrong,
//! instead of thirty hand-copied variants that can drift apart.
//!
//! A route that declared [`Requirement::RouteDerived`] must call
//! [`Decision::require_all`] before it answers. The middleware checks
//! the flag after the handler returns and refuses to release the
//! response if it was never set, so "the provider forgot to check"
//! fails closed instead of succeeding silently.
//!
//! A successful spend returns an [`Authorized`] proof. It is
//! `#[must_use]`, carries no way to construct it outside this module,
//! and is neither `Clone` nor `Copy`, so a privileged mutation helper
//! that takes one cannot be reached by code that skipped — or ignored
//! the failure of — the check.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::caps::{Cap, CapSet};
use crate::proc::SessionInfo;

use super::grant::{Audience, Issuer, Requirement, Subject};
use super::handle::{GrantId, GrantRef};
use super::store::{authority, GrantView, Presentation, RelayProof};

/// Proof that the authority spent an exact capability set for this
/// request.
///
/// The only constructor is [`Decision::require_all`], and it runs after
/// the store has atomically checked and debited the grant. Privileged
/// mutation helpers take one by reference, so the type system sequences
/// the side effect after the authorization rather than trusting each
/// call site to have done the check and to have handled the `Err`.
#[must_use = "an authorization proof must be handed to the operation it authorizes"]
#[derive(Debug)]
pub struct Authorized {
    grant: GrantRef,
    spent: Vec<Cap>,
}

impl Authorized {
    /// The keyed reference of the grant this proof was spent against.
    /// Safe for audit; reverses to nothing.
    pub fn grant_ref(&self) -> &GrantRef {
        &self.grant
    }

    /// The exact capabilities that were spent.
    pub fn spent(&self) -> &[Cap] {
        &self.spent
    }
}

/// The authorization context for one in-flight request.
#[derive(Debug)]
pub struct Decision {
    grant_id: GrantId,
    grant_ref: GrantRef,
    issuer: Issuer,
    audience: Audience,
    route: &'static str,
    subject: Subject,
    caps: CapSet,
    owner_uid: u32,
    bound_pid: u32,
    generation: u64,
    presentation: Presentation,
    relay: Option<RelayProof>,
    /// The registry row the subject session refers to, resolved once
    /// under the owner's own path view. Providers that need the row
    /// read it from here instead of looking it up again.
    session: Option<SessionInfo>,
    /// Whether the owning route still owes a capability check.
    obligation: bool,
    exercised: AtomicBool,
}

impl Decision {
    pub(super) fn new(
        view: GrantView,
        route: &'static str,
        audience: Audience,
        presentation: Presentation,
        relay: Option<RelayProof>,
        session: Option<SessionInfo>,
        requirement: &Requirement,
    ) -> Self {
        Self {
            grant_id: view.id,
            grant_ref: view.id.audit_ref(),
            issuer: view.issuer,
            audience,
            route,
            subject: view.subject,
            caps: view.caps,
            owner_uid: view.owner_uid,
            bound_pid: view.bound_pid,
            generation: view.generation,
            presentation,
            relay,
            session,
            obligation: requirement.is_route_derived(),
            exercised: AtomicBool::new(false),
        }
    }

    /// Keyed, non-reversible reference for audit records.
    pub fn grant_ref(&self) -> &GrantRef {
        &self.grant_ref
    }

    pub fn issuer(&self) -> Issuer {
        self.issuer
    }

    pub fn audience(&self) -> Audience {
        self.audience
    }

    pub fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    pub fn session_id(&self) -> Option<&str> {
        self.subject.session_id.as_deref()
    }

    pub fn app_id(&self) -> Option<&str> {
        self.subject.app_id.as_deref()
    }

    pub fn task_id(&self) -> Option<&str> {
        self.subject.task_id.as_deref()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn bound_pid(&self) -> u32 {
        self.bound_pid
    }

    /// The authorized capability set. Read-only: a handler can inspect
    /// what it holds but has no way to add to it.
    pub fn caps(&self) -> &CapSet {
        &self.caps
    }

    /// The registry row for the subject session, when the route has
    /// one. Resolved by the middleware, not by the provider.
    pub fn session(&self) -> Result<&SessionInfo, String> {
        self.session
            .as_ref()
            .ok_or_else(|| "this route has no authorized session".to_string())
    }

    /// Defence in depth for a provider whose route belongs to exactly
    /// one first-party App.
    ///
    /// The App identity comes from the grant the daemon minted, not
    /// from the registry row and not from the request, so an App that
    /// renamed its own row cannot reach another App's provider.
    pub fn require_app(&self, app_id: &str) -> Result<(), String> {
        match self.subject.app_id.as_deref() {
            Some(held) if held == app_id => Ok(()),
            _ => Err(format!("this route is restricted to the {app_id} App")),
        }
    }

    /// Defence in depth for a provider that accepts either its own App
    /// or any authenticated session holding the capability — the shape
    /// `packages` and `systemd` use for their non-App callers.
    pub fn app_is(&self, app_id: &str) -> bool {
        self.subject.app_id.as_deref() == Some(app_id)
    }

    /// Check and spend one capability.
    pub fn require(&self, cap: Cap) -> Result<Authorized, String> {
        self.require_all(std::slice::from_ref(&cap))
    }

    /// Check and spend a whole capability set, all or none.
    ///
    /// The check runs against the live grant, not against the snapshot
    /// this object holds, so a grant revoked while the handler was
    /// awaiting is refused here. `required` is spent as one unit: a
    /// missing capability leaves the grant's use budget untouched.
    ///
    /// An empty set is refused. "Authorized for nothing" is not an
    /// authorization, and accepting it would let a route satisfy its
    /// obligation — and obtain an [`Authorized`] proof — without ever
    /// naming a capability.
    pub fn require_all(&self, required: &[Cap]) -> Result<Authorized, String> {
        if required.is_empty() {
            // Deliberately does not mark the obligation met: a route
            // that asked for nothing has authorized nothing.
            super::audit::record_empty_requirement(self);
            return Err("a capability check must name at least one capability".to_string());
        }
        let spent = match self.relay.as_ref() {
            Some(proof) => {
                authority().consume_relayed(self.grant_id, required, &self.presentation, proof)
            }
            None => authority().consume(self.grant_id, required, &self.presentation),
        };
        match spent {
            Ok(view) => {
                // Only a *successful* spend satisfies the obligation.
                // A refusal the provider ignored leaves the route owing
                // the authority a check, so the response is withheld.
                self.exercised.store(true, Ordering::SeqCst);
                super::audit::record_use(self, required, view.uses_remaining);
                Ok(Authorized {
                    grant: self.grant_ref.clone(),
                    spent: required.to_vec(),
                })
            }
            Err(error) => {
                super::audit::record_denied(self, required, &error);
                Err(error.to_string())
            }
        }
    }

    /// Did the route satisfy the obligation its descriptor declared?
    pub(super) fn obligation_met(&self) -> bool {
        !self.obligation || self.exercised.load(Ordering::SeqCst)
    }

    pub(super) fn audit_route(&self) -> &'static str {
        self.route
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        view: GrantView,
        route: &'static str,
        audience: Audience,
        presentation: Presentation,
        session: Option<SessionInfo>,
        requirement: &Requirement,
    ) -> Self {
        Self::new(
            view,
            route,
            audience,
            presentation,
            None,
            session,
            requirement,
        )
    }
}
