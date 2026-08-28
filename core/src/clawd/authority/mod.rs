//! The capability authority — one place that decides what a broker
//! request may do.
//!
//! ## What changed and why
//!
//! Authority used to be *described* rather than *held*. A privileged
//! provider was handed a session id out of the request body, looked the
//! row up in the routed process registry, re-derived the same five
//! checks every other provider had hand-copied, and then trusted a
//! serialized [`CapSet`] it found on disk. Thirty copies of a policy is
//! thirty places for it to drift, and a serialized cap set is a
//! *description* of authority that anything able to write the registry
//! could mint.
//!
//! Now there is exactly one authority, it lives in `clawd`, and it
//! hands out **grants**:
//!
//! ```text
//!   register / approve / delegate
//!            │
//!            ▼
//!   Authority::issue ──▶ Grant { principal, subject, audience,
//!            │                   caps, expiry, uses, lineage }
//!            │                              │
//!            │  opaque handle               │  session index
//!            ▼                              ▼
//!   launcher holds a reference     provider request names a session
//!            │                              │
//!            └────────────┬─────────────────┘
//!                         ▼
//!              authority::authorize(route, params, client)
//!                         │  kernel credentials + /proc identity
//!                         │  audience + subject + caps + budget
//!                         ▼
//!                     Decision  ──▶  handler, which re-checks
//!                                    through the same Decision
//! ```
//!
//! ## The two things that are not authority
//!
//! * **A handle is not a bearer token.** Possession is necessary and
//!   insufficient: every grant is bound to a uid, a pid, that pid's
//!   start time and (where `/proc` reports one) its cgroup, so a
//!   same-uid sibling, an fd recipient, a recycled pid or a process
//!   that re-`exec`ed cannot present it. See
//!   [`store::check_presentation`].
//! * **A session id is not authority.** It is an *index* into the
//!   store. Naming somebody else's session finds their grant and then
//!   fails the principal check, which is the same answer a caller gets
//!   for a session that does not exist.
//!
//! ## Mandatory, not optional
//!
//! Every row in [`crate::clawd::routes`] declares an
//! [`RouteAuthority`]. The `routes!` macro takes the field positionally
//! and does not compile without it, so a route cannot exist without an
//! access class, an audience, a subject source, a capability resolver
//! and an approval classification. A unit test walks `ROUTES` and
//! asserts the descriptor and the handler agree.
//!
//! Provider-side checks stay — a privileged mutation should be refused
//! twice — but they now run through [`Decision::require_all`], which
//! consults the same live grant the middleware resolved. A route that
//! declares [`Requirement::RouteDerived`] and never calls it has its
//! response refused, so "forgot to check" fails closed.

pub mod audit;
pub mod decision;
pub mod grant;
pub mod handle;
pub mod store;

use serde_json::Value;

pub use decision::{Authorized, Decision};
pub use grant::{
    Attenuation, AttenuationError, Audience, AudienceSet, Binding, Issuance, Issuer, Principal,
    Requirement, Subject, Uses, MAX_CHILDREN, MAX_LINEAGE_DEPTH,
};
pub use handle::{GrantHandle, GrantId, GrantRef, HandleKey};
use store::RelayProof;
pub use store::{authority, Authority, AuthorityError, GrantView, Presentation};

use super::client_identity::ClientIdentity;
use super::wire::Fault;

/// Where a route's subject comes from.
///
/// The subject is an identifier the request may name, never authority
/// in itself. `Session` reads the `session` field the typed body
/// already validated; `Handle` reads an opaque handle the daemon minted
/// earlier; `PeerSession` names the caller's *own* registered session;
/// `Peer` means the route acts only for the connecting process and
/// names nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectSource {
    /// The route acts for the peer itself. No grant is resolved; the
    /// decision is the authenticated peer identity plus the access
    /// class the registry already enforced.
    Peer,
    /// The `session` field of the typed body names an App/MCP session
    /// whose grant this request runs under.
    Session,
    /// The `session` field names the caller's own registered session in
    /// the root-owned routed registry — a rollback client or an agent
    /// runtime asking the broker to finish work it is already
    /// authorized for.
    ///
    /// There is no standing grant for such a session, so the middleware
    /// authenticates it from the peer's process ancestry and mints a
    /// short, single-purpose one. The rest of the pipeline — audience,
    /// capability spend, audit, obligation — is then identical, so this
    /// is one implementation rather than a second policy.
    PeerSession,
    /// The `session_id` + `handle` fields name a grant directly.
    Handle,
}

/// Whether a denial on this route may become a consent prompt.
///
/// Approval-eligible routes are the ones a user can meaningfully be
/// asked about: a named resource, an App they launched, an action they
/// initiated. The consent surface itself and the daemon's own status
/// are not — asking the user to approve reading the approval queue is
/// how a prompt-fatigue attack starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    Eligible,
    Ineligible,
}

/// Whether a route may act on an App session's *transient* capabilities.
///
/// Transient capabilities exist for exactly one MCP session tool call:
/// `app_session.set_transient` widens the session for that call and
/// narrows it again afterwards. A route that was never allowed to see
/// them must not start seeing them merely because both now go through
/// one authority — that would let a credential refresh borrow the
/// capability an unrelated tool call had just been granted.
///
/// The setting is declared per route rather than inferred, so widening
/// it is a visible edit to the registry with a test to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientCaps {
    /// Only the session's own base capabilities are in scope.
    Excluded,
    /// The base set plus whatever the current tool call was granted.
    Included,
}

/// The authorization contract of one route.
///
/// Declared next to the route's wire name, typed body, access class and
/// budget, so all of it is read together and none of it can be added
/// later by a caller.
#[derive(Debug)]
pub struct RouteAuthority {
    /// Route family a grant must be valid for.
    pub audience: Audience,
    /// Where this route's subject comes from.
    pub subject: SubjectSource,
    /// Derives the exact capabilities this request needs from the
    /// body the wire layer already validated. Owned by the module that
    /// owns the route, so canonicalization and enforcement cannot
    /// disagree.
    pub requirement: RequirementResolver,
    /// Whether a denial here may be turned into a consent request.
    pub approval: Approval,
    /// Whether an App session's one-call transient capabilities count
    /// for this route. Only consulted where the middleware builds the
    /// capability set itself, i.e. [`SubjectSource::PeerSession`].
    pub transient: TransientCaps,
}

pub type RequirementResolver = fn(&Value) -> Result<Requirement, String>;

/// A route that needs no capability at all.
pub fn no_requirement(_params: &Value) -> Result<Requirement, String> {
    Ok(Requirement::None)
}

/// A route whose exact capability is canonicalized by the owning module
/// during dispatch, and which must exercise the decision before it
/// answers.
pub fn route_derived(_params: &Value) -> Result<Requirement, String> {
    Ok(Requirement::RouteDerived)
}

/// How long a grant minted for one peer-session request may live. Long
/// enough for a slow `apt` restore to finish the call it was minted
/// for, short enough that nothing accumulates.
const PEER_SESSION_GRANT_TTL: std::time::Duration = std::time::Duration::from_secs(120);

/// Uses a peer-session grant carries.
///
/// Exactly one. The handle is dropped without being indexed, so the
/// grant is reachable only through the decision this request holds, and
/// that decision authorizes one capability set. Any budget above the
/// number actually needed is authority nobody asked for.
const PEER_SESSION_GRANT_USES: u32 = 1;

/// Authenticate the caller's own registered session and mint the grant
/// this one request runs under.
///
/// This is the seam for callers that hold no standing grant — the
/// rollback client finishing a mutation it already recorded, an agent
/// runtime refreshing a credential its session was granted. The session
/// row is root-owned state in the routed registry, and it is believed
/// only when the peer can prove it *is* that session: the row is bound
/// to a live process, that process's start time still matches, and the
/// peer is that process or a descendant of it. A sibling naming
/// somebody else's session fails the ancestry check.
///
/// Capabilities are read from the row rather than invented, and they
/// are only a ceiling: the route still has to spend the exact one it
/// needs, and the grant expires in minutes whatever happens.
async fn mint_peer_session_grant(
    route_name: &'static str,
    descriptor: &RouteAuthority,
    client: &ClientIdentity,
    uid: u32,
    session_id: &str,
) -> Result<GrantView, AuthorityError> {
    let pid = client.pid.ok_or(AuthorityError::PrincipalMismatch)?;
    let home = client.home_dir().ok_or(AuthorityError::Subject)?;
    let lookup = session_id.to_string();
    let session = crate::paths::with_user_override(uid, home, async move {
        crate::proc::session_info_by_id(&lookup)
    })
    .await
    .ok_or(AuthorityError::Subject)?;

    if session.pending_bind || session.pid == 0 {
        return Err(AuthorityError::Subject);
    }
    let expected_start = session
        .start_time_ticks
        .ok_or(AuthorityError::PrincipalMismatch)?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err(AuthorityError::PrincipalMismatch);
    }
    if pid != session.pid && !crate::proc::process_descends_from(pid, session.pid) {
        return Err(AuthorityError::PrincipalMismatch);
    }

    let mut caps = session.caps.clone().unwrap_or_default();
    match descriptor.transient {
        TransientCaps::Included => {
            if let Some(transient) = session.transient_caps.clone() {
                caps.extend(transient.iter().cloned());
            }
        }
        // The route was never entitled to the capability a tool call
        // was granted for one invocation, and routing both through one
        // authority must not change that.
        TransientCaps::Excluded => {}
    }
    if caps.is_empty() {
        return Err(AuthorityError::Capability {
            verb: "*",
            scope: String::new(),
        });
    }

    let principal =
        Principal::of_process(uid, session.pid).ok_or(AuthorityError::UnverifiablePrincipal)?;
    let (_handle, view) = authority().issue(Issuance {
        issuer: Issuer::TrustedSession,
        principal,
        binding: Binding::ProcessTree,
        subject: Subject::session(session_id).with_app(session.app_id.clone()),
        audience: AudienceSet::one(descriptor.audience),
        caps,
        lifetime: PEER_SESSION_GRANT_TTL,
        uses: Uses::Budget(PEER_SESSION_GRANT_USES),
        index_session: false,
    })?;
    tracing::debug!(
        route = route_name,
        "minted a request-scoped grant for an authenticated peer session"
    );
    audit::record_issued(&view, None);
    Ok(view)
}

/// Resolve the authority for one request.
///
/// Runs after the transport verified the peer and after the registry
/// decoded the typed body, and before the handler is entered. Returns
/// `Ok(None)` for a peer-scoped route, which carries no grant.
pub async fn authorize(
    route_name: &'static str,
    descriptor: &'static RouteAuthority,
    params: &Value,
    client: &ClientIdentity,
) -> Result<Option<Decision>, Fault> {
    let uid = client.uid.ok_or(Fault::MissingCredentials)?;
    let pid = client.pid.ok_or(Fault::MissingCredentials)?;
    let requirement = (descriptor.requirement)(params).map_err(|error| {
        tracing::debug!(route = route_name, error = %error, "route refused its own request shape");
        Fault::InvalidParams
    })?;

    if descriptor.subject == SubjectSource::Peer {
        // Nothing to resolve: the access class the registry enforced is
        // the whole decision, and a peer-scoped route may not declare a
        // capability requirement (a unit test asserts it).
        return Ok(None);
    }

    let presentation = Presentation::new(
        uid,
        pid,
        client.start_time_ticks,
        descriptor.audience,
        route_name,
    );

    let view = match descriptor.subject {
        SubjectSource::Peer => unreachable!("handled above"),
        SubjectSource::Session => {
            let session_id = string_field(params, "session").ok_or_else(|| {
                unresolved(route_name, descriptor, uid, None, AuthorityError::Subject)
            })?;
            authority()
                .resolve_session(&session_id, &presentation)
                .map_err(|error| {
                    unresolved(route_name, descriptor, uid, Some(&session_id), error)
                })?
        }
        SubjectSource::PeerSession => {
            let session_id = string_field(params, "session").ok_or_else(|| {
                unresolved(route_name, descriptor, uid, None, AuthorityError::Subject)
            })?;
            mint_peer_session_grant(route_name, descriptor, client, uid, &session_id)
                .await
                .map_err(|error| {
                    unresolved(route_name, descriptor, uid, Some(&session_id), error)
                })?
        }
        SubjectSource::Handle => {
            let handle = string_field(params, "handle").ok_or_else(|| {
                unresolved(
                    route_name,
                    descriptor,
                    uid,
                    None,
                    AuthorityError::UnknownGrant,
                )
            })?;
            authority()
                .resolve(&handle, &presentation)
                .map_err(|error| unresolved(route_name, descriptor, uid, None, error))?
        }
    };

    let mut presentation = presentation;
    presentation.session_id = view.subject.session_id.clone();
    finish(
        route_name,
        descriptor,
        requirement,
        view,
        presentation,
        uid,
        client,
    )
    .await
}

/// The tail every authorization shares: read the session row under the
/// owner's own path view, build the decision, and spend an exact
/// requirement before the handler is entered.
async fn finish(
    route_name: &'static str,
    descriptor: &'static RouteAuthority,
    requirement: Requirement,
    view: GrantView,
    presentation: Presentation,
    uid: u32,
    client: &ClientIdentity,
) -> Result<Option<Decision>, Fault> {
    // The routed registry is partitioned per owner, so the row is read
    // under the owner's own path view — the same view the provider used
    // to read it before this decision existed.
    let session = match (view.subject.session_id.clone(), client.home_dir()) {
        (Some(session_id), Some(home)) => {
            crate::paths::with_user_override(uid, home, async move {
                crate::proc::session_info_by_id(&session_id)
            })
            .await
        }
        _ => None,
    };
    let decision = Decision::new(
        view,
        route_name,
        descriptor.audience,
        presentation,
        session,
        &requirement,
    );

    if let Requirement::Exact(caps) = &requirement {
        if !caps.is_empty() {
            // The proof is dropped here on purpose: the middleware
            // authorized the request as a whole, and a route with an
            // exact requirement has no privileged helper of its own to
            // hand it to. Naming it keeps the discard deliberate.
            let _authorized = decision.require_all(caps).map_err(|error| {
                tracing::debug!(route = route_name, error = %error, "authority refused a route requirement");
                Fault::NotAuthorized
            })?;
        }
    }
    Ok(Some(decision))
}

/// Authorize one inner route that a trusted launcher is relaying for a
/// sandboxed worker.
///
/// The relay is *not* the authority. What this does is prove, in order:
///
/// 1. the presenting process holds a live relay grant — bound
///    `Process`-tight to it, in the [`Audience::AppRelay`] family, not
///    expired and not revoked;
/// 2. that grant names exactly the session the caller asked to act for;
/// 3. the inner route is one a session may reach at all — a
///    `Session`-subject `SystemService` route, never a root, peer,
///    peer-session, handle-addressed, identity, consent, journal or
///    scheduler route;
/// 4. the App session grant itself still resolves, is live, covers the
///    audience, and satisfies the inner route's requirement.
///
/// Step 4 runs against the *current* session grant, so a transient
/// capability the kernel granted for one MCP call is visible while it
/// is set and gone the moment it is cleared. Nothing here widens what
/// the session holds; the relay only supplies the identity the worker
/// cannot present from inside its namespace.
pub async fn authorize_relayed(
    relay_handle: &str,
    session_id: &str,
    route_name: &'static str,
    descriptor: &'static RouteAuthority,
    params: &Value,
    client: &ClientIdentity,
) -> Result<Option<Decision>, Fault> {
    let uid = client.uid.ok_or(Fault::MissingCredentials)?;
    let pid = client.pid.ok_or(Fault::MissingCredentials)?;
    // Belt and braces: the relay route already refuses anything else,
    // and a future route table change must not turn this into a way to
    // reach a peer-scoped or root surface.
    if descriptor.subject != SubjectSource::Session
        || descriptor.audience != Audience::SystemService
    {
        return Err(Fault::NotAuthorized);
    }

    let relay_presentation = Presentation::new(
        uid,
        pid,
        client.start_time_ticks,
        Audience::AppRelay,
        route_name,
    );
    let relay = authority()
        .resolve(relay_handle, &relay_presentation)
        .map_err(|error| unresolved(route_name, descriptor, uid, Some(session_id), error))?;
    if relay.subject.session_id.as_deref() != Some(session_id) {
        return Err(unresolved(
            route_name,
            descriptor,
            uid,
            Some(session_id),
            AuthorityError::Subject,
        ));
    }

    let requirement = (descriptor.requirement)(params).map_err(|error| {
        tracing::debug!(route = route_name, error = %error, "relayed route refused its own request shape");
        Fault::InvalidParams
    })?;
    let proof = RelayProof::for_session(session_id);
    // The subject is named *before* the resolve, not patched in after
    // it, so the store's own subject check decides this call: a relay
    // proof answers "who is speaking", and the grant still has to be
    // the one for this session and to carry the inner route's audience.
    let mut presentation = Presentation::new(
        uid,
        pid,
        client.start_time_ticks,
        descriptor.audience,
        route_name,
    );
    presentation.session_id = Some(session_id.to_string());
    let view = authority()
        .resolve_session_relayed(session_id, &presentation, &proof)
        .map_err(|error| unresolved(route_name, descriptor, uid, Some(session_id), error))?;
    presentation.session_id = view.subject.session_id.clone();
    finish(
        route_name,
        descriptor,
        requirement,
        view,
        presentation,
        uid,
        client,
    )
    .await
}

/// Did a handler satisfy the obligation its descriptor declared?
///
/// Called after dispatch. A route that declared `RouteDerived` and
/// answered without ever consulting the decision has its response
/// withheld: the authority has no record that anything was authorized,
/// so there is nothing to release.
pub fn obligation_met(decision: Option<&Decision>) -> bool {
    decision.map(Decision::obligation_met).unwrap_or(true)
}

fn unresolved(
    route: &'static str,
    descriptor: &RouteAuthority,
    uid: u32,
    session_id: Option<&str>,
    error: AuthorityError,
) -> Fault {
    audit::record_unresolved(route, descriptor.audience, uid, session_id, &error);
    match error {
        AuthorityError::Quota(_) => Fault::TooManyRequests,
        _ => Fault::NotAuthorized,
    }
}

fn string_field(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Revoke every grant bound to a session. Called when a session is
/// deregistered, finishes, or is cancelled.
///
/// Reusable approvals bound to the same session go with it. A grant the
/// user approved "for this session" must not outlive the session, and
/// the only way to say so durably is to raise its revocation
/// generation.
pub fn revoke_session(session_id: &str) {
    let retired = authority().revoke_session(session_id);
    audit::record_revoked("session", Some(session_id), retired);
    crate::approvals::generations::revoke_session_best_effort(
        crate::paths::current_owner_uid_override(),
        session_id,
    );
}

/// Revoke every grant bound to a session on behalf of a known owner.
///
/// Same as [`revoke_session`], but for callers outside the owner's path
/// scope — a supervisor tearing down a worker lease knows the uid from
/// the lease rather than from a task-local override.
pub fn revoke_session_for_owner(session_id: &str, owner_uid: u32) {
    let retired = authority().revoke_session(session_id);
    audit::record_revoked("session", Some(session_id), retired);
    crate::approvals::generations::revoke_session_best_effort(Some(owner_uid), session_id);
}

/// Revoke every grant one owner holds. Called when a worker lease
/// lapses or an owner's task tree is torn down.
pub fn revoke_owner(uid: u32) {
    let retired = authority().revoke_owner(uid);
    audit::record_revoked("owner", None, retired);
    let scope = crate::approvals::RevocationScope::Owner { uid: Some(uid) };
    match crate::approvals::generations::revoke(&scope) {
        Ok(generation) => {
            super::audit::record_approval_revocation(&scope, "*", generation);
        }
        Err(error) => {
            tracing::error!(
                error = %error,
                "could not retire reusable approvals for an owner"
            );
        }
    }
}

/// Drop expired, exhausted and orphaned grants.
pub fn sweep() {
    let retired = authority().sweep_now();
    audit::record_revoked("sweep", None, retired);
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/authority.rs"
    ));
}
