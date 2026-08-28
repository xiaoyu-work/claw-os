//! The grant store — the daemon's single source of live authority.
//!
//! Everything here is in memory and dies with the process. That is the
//! design, not a limitation: a grant is bound to a running process and
//! a `clawd` that restarted can no longer prove any of the bindings it
//! made, so every ephemeral grant must fail closed rather than survive
//! into a daemon that cannot re-verify it. Work that has to outlive a
//! restart — a scheduled job — is re-issued from root-owned durable
//! provenance through the narrow delegation policy in
//! [`crate::clawd::system_caps`], never from a serialized handle.
//!
//! ## Concurrency
//!
//! One `Mutex` guards the maps, and nothing slow ever runs inside it:
//! `/proc` reads happen before the lock is taken (issuance) or on the
//! bounded sweep path, and no I/O, no audit write and no handler call
//! is made while it is held. Every state transition — spend a use,
//! retire an exhausted grant, revoke a lineage — happens in one
//! critical section, so a one-shot grant cannot be spent twice by two
//! concurrent requests and a multi-capability spend is all-or-none.
//!
//! ## Bounds
//!
//! Grants are capped globally, per owner, per session and per process,
//! and swept on every mutating entry point plus a periodic tick. A
//! process that exits, a session that finishes, a lease that lapses and
//! a grant that expires all drop their rows without needing anybody to
//! call a cleanup.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet};

use super::grant::{
    Attenuation, AttenuationError, Audience, AudienceSet, Binding, Grant, Issuance, Issuer,
    Principal, Subject, Uses,
};
use super::handle::{GrantHandle, GrantId, HandleKey};

/// Grants the daemon will hold at once across every owner.
pub const MAX_GRANTS_TOTAL: usize = 4096;
/// Grants one uid may hold at once.
pub const MAX_GRANTS_PER_OWNER: usize = 512;
/// Grants bound to one session id.
pub const MAX_GRANTS_PER_SESSION: usize = 128;
/// Grants bound to one process.
///
/// Above [`super::grant::MAX_CHILDREN`] so a launcher that fills one
/// grant's child budget is refused by the lineage bound, which names
/// the actual problem, rather than by the process ceiling.
pub const MAX_GRANTS_PER_PROCESS: usize = 256;

/// Why a grant could not be resolved or spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityError {
    /// No live grant answers to this handle. Deliberately the same
    /// answer for a guessed handle, an expired one and a revoked one,
    /// so a caller learns nothing by probing.
    UnknownGrant,
    /// The presenting process is not the one the grant is bound to.
    PrincipalMismatch,
    /// The grant does not cover this route's audience.
    Audience { route: &'static str },
    /// The grant does not act for the subject the request named.
    Subject,
    /// The grant is out of uses.
    Exhausted,
    /// The grant expired.
    Expired,
    /// The grant was revoked, directly or by an ancestor.
    Revoked,
    /// The grant does not carry a capability the route requires.
    Capability { verb: &'static str, scope: String },
    /// A store ceiling was reached.
    Quota(&'static str),
    /// An attenuation broke a monotonic property.
    Attenuation(AttenuationError),
    /// Issuance could not name the process it was asked to bind to.
    UnverifiablePrincipal,
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthorityError::UnknownGrant => {
                f.write_str("no live capability grant answers to this reference")
            }
            AuthorityError::PrincipalMismatch => {
                f.write_str("capability grant belongs to a different process")
            }
            AuthorityError::Audience { route } => {
                write!(f, "capability grant is not valid for `{route}`")
            }
            AuthorityError::Subject => {
                f.write_str("capability grant does not act for the requested subject")
            }
            AuthorityError::Exhausted => f.write_str("capability grant is out of uses"),
            AuthorityError::Expired => f.write_str("capability grant has expired"),
            AuthorityError::Revoked => f.write_str("capability grant was revoked"),
            AuthorityError::Capability { verb, scope } => {
                write!(f, "capability grant lacks {verb}:{scope}")
            }
            AuthorityError::Quota(bound) => write!(f, "capability grant ceiling reached: {bound}"),
            AuthorityError::Attenuation(error) => write!(f, "{error}"),
            AuthorityError::UnverifiablePrincipal => {
                f.write_str("cannot bind a capability grant to an unverifiable process")
            }
        }
    }
}

impl AuthorityError {
    /// Stable class for audit. Never carries a handle, a scope value or
    /// any caller-authored string.
    pub fn class(&self) -> &'static str {
        match self {
            AuthorityError::UnknownGrant => "unknown_grant",
            AuthorityError::PrincipalMismatch => "principal_mismatch",
            AuthorityError::Audience { .. } => "audience",
            AuthorityError::Subject => "subject",
            AuthorityError::Exhausted => "exhausted",
            AuthorityError::Expired => "expired",
            AuthorityError::Revoked => "revoked",
            AuthorityError::Capability { .. } => "capability",
            AuthorityError::Quota(_) => "quota",
            AuthorityError::Attenuation(_) => "attenuation",
            AuthorityError::UnverifiablePrincipal => "unverifiable_principal",
        }
    }
}

/// Immutable view of a resolved grant, taken under the store lock.
///
/// Carries no handle: a holder of this snapshot can decide, audit and
/// re-check, but cannot re-present the grant on another connection.
#[derive(Debug, Clone)]
pub struct GrantView {
    pub id: GrantId,
    pub issuer: Issuer,
    pub subject: Subject,
    pub audience: AudienceSet,
    pub caps: CapSet,
    pub owner_uid: u32,
    pub bound_pid: u32,
    pub generation: u64,
    pub depth: u16,
    pub parent: Option<GrantId>,
    pub issued_ago: Duration,
    pub expires_in: Duration,
    pub uses_remaining: Option<u32>,
}

/// What a resolve must prove about the caller.
///
/// Every field is taken from the credentials the kernel stamped on the
/// request message and the route the registry resolved. Nothing in the
/// request body selects any of them.
#[derive(Debug, Clone)]
pub struct Presentation {
    /// uid the kernel stamped on this request message.
    pub uid: u32,
    /// pid the kernel stamped on this request message, already
    /// re-verified through `/proc` by the transport.
    pub pid: u32,
    /// Start time read for that pid at verification time.
    pub start_time_ticks: Option<u64>,
    /// Route family the caller is trying to reach.
    pub audience: Audience,
    /// The route's registry name, for the audit class only.
    pub route: &'static str,
    /// Session the request named, when it named one.
    pub session_id: Option<String>,
}

impl Presentation {
    /// Base presentation with no session named yet.
    pub fn new(
        uid: u32,
        pid: u32,
        start_time_ticks: Option<u64>,
        audience: Audience,
        route: &'static str,
    ) -> Self {
        Self {
            uid,
            pid,
            start_time_ticks,
            audience,
            route,
            session_id: None,
        }
    }
}

/// Evidence that a relay grant was resolved for one exact session.
///
/// Unforgeable by construction: the type is private to the authority
/// module, its only constructor is [`RelayProof::for_session`], and
/// that is `pub(super)` so it can be reached only after
/// [`super::authorize_relayed`] has resolved a live relay grant bound
/// `Process`-tight to the presenting process. Nothing in a request body
/// or a handler can produce one.
#[derive(Debug, Clone)]
pub(super) struct RelayProof {
    session_id: String,
}

impl RelayProof {
    pub(super) fn for_session(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
        }
    }

    /// Does this proof authorize presenting `grant`?
    ///
    /// Only the one session the relay grant named. A relay for session A
    /// is inert against session B's grant, and against every grant that
    /// names no session at all.
    fn covers(&self, grant: &Grant) -> bool {
        grant
            .subject
            .session_id
            .as_deref()
            .is_some_and(|session| session == self.session_id)
    }
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    grants: HashMap<HandleKey, Grant>,
    /// Handle key of the grant that owns a session id. One session has
    /// exactly one root grant; children are reached through lineage.
    by_session: HashMap<String, HandleKey>,
    children: HashMap<GrantId, Vec<HandleKey>>,
    id_index: HashMap<GrantId, HandleKey>,
    last_sweep: Option<Instant>,
}

pub struct Authority {
    inner: Mutex<Inner>,
}

impl Authority {
    fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Issue a root grant.
    ///
    /// The principal must already have been read from a live process:
    /// a grant with no start time cannot detect pid reuse, so it is
    /// refused rather than issued unbound.
    pub fn issue(&self, issuance: Issuance) -> Result<(GrantHandle, GrantView), AuthorityError> {
        if issuance.principal.start_time_ticks.is_none() {
            return Err(AuthorityError::UnverifiablePrincipal);
        }
        if issuance.audience.is_empty() {
            return Err(AuthorityError::Audience { route: "issue" });
        }
        let handle = GrantHandle::generate().map_err(|_| AuthorityError::UnverifiablePrincipal)?;
        let key = handle.key();
        let now = Instant::now();
        let expires_at = now + issuance.lifetime;

        let mut inner = self.lock();
        inner.sweep(now);
        if inner
            .check_quota(&issuance.principal, issuance.subject.session_id.as_deref())
            .is_err()
        {
            // A burst of short-lived request-scoped grants can reach a
            // ceiling while expired rows are still waiting for the next
            // rate-limited sweep. Force one, then answer honestly.
            inner.force_sweep(now);
            inner.check_quota(&issuance.principal, issuance.subject.session_id.as_deref())?;
        }
        if issuance.index_session {
            inner.check_session_index(issuance.subject.session_id.as_deref())?;
        }
        let id = inner.allocate_id();
        let grant = Grant {
            id,
            key,
            issuer: issuance.issuer,
            principal: issuance.principal,
            binding: issuance.binding,
            subject: issuance.subject,
            audience: issuance.audience,
            caps: issuance.caps,
            issued_at: now,
            expires_at,
            uses: issuance.uses,
            revoked: false,
            generation: 0,
            parent: None,
            depth: 0,
            children: 0,
        };
        let view = inner.insert(grant, now, issuance.index_session);
        Ok((handle, view))
    }

    /// Derive a child grant from `parent_handle`.
    ///
    /// Every monotonic property is checked against the live parent
    /// inside the same critical section that installs the child, so a
    /// parent revoked concurrently cannot leave a wider descendant
    /// behind.
    pub fn attenuate(
        &self,
        parent_handle: &str,
        request: Attenuation,
    ) -> Result<(GrantHandle, GrantView), AuthorityError> {
        if request.principal.start_time_ticks.is_none() {
            return Err(AuthorityError::UnverifiablePrincipal);
        }
        let handle = GrantHandle::generate().map_err(|_| AuthorityError::UnverifiablePrincipal)?;
        let key = handle.key();
        let parent_key = HandleKey::of(parent_handle);
        let now = Instant::now();

        let mut inner = self.lock();
        inner.sweep(now);
        let parent = inner
            .grants
            .get(&parent_key)
            .ok_or(AuthorityError::UnknownGrant)?;
        let expires_at = request
            .check(parent, now)
            .map_err(AuthorityError::Attenuation)?;
        let parent_id = parent.id;
        let depth = parent.depth + 1;
        inner.check_quota(&request.principal, request.subject.session_id.as_deref())?;
        if request.index_session {
            inner.check_session_index(request.subject.session_id.as_deref())?;
        }
        let id = inner.allocate_id();
        let grant = Grant {
            id,
            key,
            issuer: request.issuer,
            principal: request.principal,
            binding: request.binding,
            subject: request.subject,
            audience: request.audience,
            caps: request.caps,
            issued_at: now,
            expires_at,
            uses: request.uses,
            revoked: false,
            generation: 0,
            parent: Some(parent_id),
            depth,
            children: 0,
        };
        let view = inner.insert(grant, now, request.index_session);
        if let Some(parent) = inner.grants.get_mut(&parent_key) {
            parent.children = parent.children.saturating_add(1);
        }
        inner.children.entry(parent_id).or_default().push(key);
        Ok((handle, view))
    }

    /// Resolve a presented handle for one route, without spending it.
    pub fn resolve(
        &self,
        presented: &str,
        presentation: &Presentation,
    ) -> Result<GrantView, AuthorityError> {
        let key = HandleKey::of(presented);
        let now = Instant::now();
        let mut inner = self.lock();
        inner.sweep(now);
        let grant = inner.grants.get(&key).ok_or(AuthorityError::UnknownGrant)?;
        check_presentation(grant, presentation, now)?;
        Ok(view_of(grant, now))
    }

    /// Resolve the grant that owns a session id.
    ///
    /// The session id is an *identifier*, not authority: it is used
    /// only to find the row, and the caller still has to be the bound
    /// principal for the presentation to succeed. A sibling that
    /// guessed or read the id from a process listing gets
    /// [`AuthorityError::PrincipalMismatch`].
    pub fn resolve_session(
        &self,
        session_id: &str,
        presentation: &Presentation,
    ) -> Result<GrantView, AuthorityError> {
        self.resolve_session_inner(session_id, presentation, None)
    }

    /// Resolve a session grant that a trusted launcher is presenting on
    /// the sandboxed worker's behalf.
    ///
    /// `proof` can only exist after a relay grant bound to this exact
    /// process was resolved, so this widens the process-tree check by
    /// exactly one already-authenticated hop and nothing else.
    pub(super) fn resolve_session_relayed(
        &self,
        session_id: &str,
        presentation: &Presentation,
        proof: &RelayProof,
    ) -> Result<GrantView, AuthorityError> {
        self.resolve_session_inner(session_id, presentation, Some(proof))
    }

    fn resolve_session_inner(
        &self,
        session_id: &str,
        presentation: &Presentation,
        relay: Option<&RelayProof>,
    ) -> Result<GrantView, AuthorityError> {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.sweep(now);
        let key = *inner
            .by_session
            .get(session_id)
            .ok_or(AuthorityError::UnknownGrant)?;
        let grant = inner.grants.get(&key).ok_or(AuthorityError::UnknownGrant)?;
        check_presentation_with(grant, presentation, now, relay)?;
        Ok(view_of(grant, now))
    }

    /// Spend one use of a grant after checking it covers every
    /// requested capability.
    ///
    /// All-or-none: if any capability is missing, nothing is spent. The
    /// check and the decrement happen in one critical section, so two
    /// concurrent callers cannot both spend the last use of a one-shot
    /// grant.
    pub fn consume(
        &self,
        id: GrantId,
        required: &[Cap],
        presentation: &Presentation,
    ) -> Result<GrantView, AuthorityError> {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.sweep(now);
        let key = *inner
            .id_index
            .get(&id)
            .ok_or(AuthorityError::UnknownGrant)?;
        let grant = inner.grants.get(&key).ok_or(AuthorityError::UnknownGrant)?;
        check_presentation(grant, presentation, now)?;
        for cap in required {
            if !grant.caps.covers(cap) {
                return Err(AuthorityError::Capability {
                    verb: cap.verb.as_str(),
                    scope: cap.scope.to_string(),
                });
            }
        }
        let grant = inner
            .grants
            .get_mut(&key)
            .ok_or(AuthorityError::UnknownGrant)?;
        if let Uses::Budget(remaining) = grant.uses {
            if remaining == 0 {
                return Err(AuthorityError::Exhausted);
            }
            grant.uses = Uses::Budget(remaining - 1);
        }
        let view = view_of(grant, now);
        if grant.uses.is_exhausted() {
            // Exhaustion retires this grant only. A child was already
            // clamped to the parent's caps, audience and expiry at
            // issuance, so it does not become wider by outliving a
            // spent parent — while revocation and expiry, which *do*
            // mean the authority behind the lineage is gone, still
            // cascade.
            let id = grant.id;
            inner.retire(id);
        }
        Ok(view)
    }

    /// Revoke one grant and every descendant.
    pub fn revoke(&self, id: GrantId) -> usize {
        let mut inner = self.lock();
        inner.revoke_lineage(id)
    }

    /// Revoke the grant a session owns, and everything derived from it.
    /// Called when a session finishes, is cancelled, or is deregistered.
    pub fn revoke_session(&self, session_id: &str) -> usize {
        let mut inner = self.lock();
        let Some(key) = inner.by_session.get(session_id).copied() else {
            return 0;
        };
        let Some(id) = inner.grants.get(&key).map(|grant| grant.id) else {
            return 0;
        };
        inner.revoke_lineage(id)
    }

    /// Revoke everything one owner holds. Used when a worker lease
    /// lapses or an owner's task tree is torn down.
    pub fn revoke_owner(&self, uid: u32) -> usize {
        let mut inner = self.lock();
        let roots: Vec<GrantId> = inner
            .grants
            .values()
            .filter(|grant| grant.principal.uid == uid)
            .map(|grant| grant.id)
            .collect();
        roots.into_iter().map(|id| inner.revoke_lineage(id)).sum()
    }

    /// Drop expired, exhausted, revoked and orphaned grants. Safe to
    /// call from a timer; bounded by the number of live grants.
    pub fn sweep_now(&self) -> usize {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.force_sweep(now)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().grants.len()
    }

    #[cfg(test)]
    pub(crate) fn clear_for_test(&self) {
        let mut inner = self.lock();
        inner.grants.clear();
        inner.by_session.clear();
        inner.children.clear();
        inner.id_index.clear();
        inner.last_sweep = None;
    }
}

impl Inner {
    fn allocate_id(&mut self) -> GrantId {
        self.next_id = self.next_id.wrapping_add(1);
        GrantId(self.next_id)
    }

    fn insert(&mut self, grant: Grant, now: Instant, index_session: bool) -> GrantView {
        let key = grant.key;
        let id = grant.id;
        if index_session {
            if let Some(session_id) = grant.subject.session_id.clone() {
                self.by_session.insert(session_id, key);
            }
        }
        let view = view_of(&grant, now);
        self.grants.insert(key, grant);
        self.id_index.insert(id, key);
        view
    }

    /// Refuse a second claim on a live session index.
    ///
    /// One session id resolves to exactly one grant. A re-registration
    /// while the first is still live is an attempt to re-point an
    /// identifier a provider already trusts, so it is refused instead
    /// of replacing the row.
    fn check_session_index(&self, session_id: Option<&str>) -> Result<(), AuthorityError> {
        let Some(session_id) = session_id else {
            return Ok(());
        };
        if self.by_session.contains_key(session_id) {
            return Err(AuthorityError::Quota("session-index"));
        }
        Ok(())
    }

    fn check_quota(
        &self,
        principal: &Principal,
        session_id: Option<&str>,
    ) -> Result<(), AuthorityError> {
        if self.grants.len() >= MAX_GRANTS_TOTAL {
            return Err(AuthorityError::Quota("total"));
        }
        let owner = self
            .grants
            .values()
            .filter(|grant| grant.principal.uid == principal.uid)
            .count();
        if owner >= MAX_GRANTS_PER_OWNER {
            return Err(AuthorityError::Quota("owner"));
        }
        let process = self
            .grants
            .values()
            .filter(|grant| {
                grant.principal.pid == principal.pid
                    && grant.principal.start_time_ticks == principal.start_time_ticks
            })
            .count();
        if process >= MAX_GRANTS_PER_PROCESS {
            return Err(AuthorityError::Quota("process"));
        }
        if let Some(session_id) = session_id {
            let session = self
                .grants
                .values()
                .filter(|grant| grant.subject.session_id.as_deref() == Some(session_id))
                .count();
            if session >= MAX_GRANTS_PER_SESSION {
                return Err(AuthorityError::Quota("session"));
            }
        }
        Ok(())
    }

    /// Drop one grant without touching its lineage. Used when a use
    /// budget runs out.
    fn retire(&mut self, id: GrantId) {
        let Some(key) = self.id_index.remove(&id) else {
            return;
        };
        let Some(grant) = self.grants.remove(&key) else {
            return;
        };
        if let Some(session_id) = grant.subject.session_id.as_deref() {
            if self.by_session.get(session_id) == Some(&key) {
                self.by_session.remove(session_id);
            }
        }
        if let Some(parent) = grant.parent {
            if let Some(parent_key) = self.id_index.get(&parent).copied() {
                if let Some(parent) = self.grants.get_mut(&parent_key) {
                    parent.children = parent.children.saturating_sub(1);
                }
            }
        }
        // Children survive: they were clamped to this grant at
        // issuance and carry their own expiry and budget.
        self.children.remove(&id);
    }

    /// Revoke `id` and everything below it, then drop the rows.
    ///
    /// Returns how many grants were retired. Depth is already bounded
    /// by [`super::grant::MAX_LINEAGE_DEPTH`] at issuance, and the walk
    /// tracks what it has visited, so a lineage that somehow contained
    /// a cycle still terminates.
    fn revoke_lineage(&mut self, id: GrantId) -> usize {
        let mut pending = vec![id];
        let mut seen: Vec<GrantId> = Vec::new();
        let mut removed = 0;
        while let Some(current) = pending.pop() {
            if seen.contains(&current) {
                continue;
            }
            seen.push(current);
            if let Some(children) = self.children.remove(&current) {
                for child_key in children {
                    if let Some(child) = self.grants.get(&child_key) {
                        pending.push(child.id);
                    }
                }
            }
            let Some(key) = self.id_index.remove(&current) else {
                continue;
            };
            if let Some(mut grant) = self.grants.remove(&key) {
                grant.revoked = true;
                grant.generation = grant.generation.saturating_add(1);
                if let Some(session_id) = grant.subject.session_id.as_deref() {
                    if self.by_session.get(session_id) == Some(&key) {
                        self.by_session.remove(session_id);
                    }
                }
                if let Some(parent) = grant.parent {
                    if let Some(parent_key) = self.id_index.get(&parent).copied() {
                        if let Some(parent) = self.grants.get_mut(&parent_key) {
                            parent.children = parent.children.saturating_sub(1);
                        }
                    }
                }
                removed += 1;
            }
        }
        removed
    }

    /// Bounded sweep run at the head of every entry point.
    ///
    /// Rate-limited so a burst of requests does not walk the whole map
    /// on every one; [`Authority::sweep_now`] forces a pass.
    fn sweep(&mut self, now: Instant) {
        const SWEEP_INTERVAL: Duration = Duration::from_secs(5);
        if let Some(last) = self.last_sweep {
            if now.duration_since(last) < SWEEP_INTERVAL {
                return;
            }
        }
        self.force_sweep(now);
    }

    fn force_sweep(&mut self, now: Instant) -> usize {
        self.last_sweep = Some(now);
        let dead: Vec<GrantId> = self
            .grants
            .values()
            .filter(|grant| !grant.is_live(now))
            .map(|grant| grant.id)
            .collect();
        dead.into_iter().map(|id| self.revoke_lineage(id)).sum()
    }
}

fn view_of(grant: &Grant, now: Instant) -> GrantView {
    GrantView {
        id: grant.id,
        issuer: grant.issuer,
        subject: grant.subject.clone(),
        audience: grant.audience,
        caps: grant.caps.clone(),
        owner_uid: grant.principal.uid,
        bound_pid: grant.principal.pid,
        generation: grant.generation,
        depth: grant.depth,
        parent: grant.parent,
        issued_ago: now.saturating_duration_since(grant.issued_at),
        expires_in: grant.expires_at.saturating_duration_since(now),
        uses_remaining: grant.uses.remaining(),
    }
}

/// Everything a presented grant has to prove before it decides
/// anything.
///
/// Order matters: liveness first, so a revoked or expired grant is
/// never probed for its bindings; then the process identity the kernel
/// reported for *this* message; then the audience; then the subject.
fn check_presentation(
    grant: &Grant,
    presentation: &Presentation,
    now: Instant,
) -> Result<(), AuthorityError> {
    check_presentation_with(grant, presentation, now, None)
}

fn check_presentation_with(
    grant: &Grant,
    presentation: &Presentation,
    now: Instant,
    relay: Option<&RelayProof>,
) -> Result<(), AuthorityError> {
    if grant.revoked {
        return Err(AuthorityError::Revoked);
    }
    if grant.is_expired(now) {
        return Err(AuthorityError::Expired);
    }
    if grant.uses.is_exhausted() {
        return Err(AuthorityError::Exhausted);
    }
    if !grant.principal.is_live() {
        // The process the grant names is gone, or its pid was recycled
        // by an unrelated one. Either way nothing may act under it.
        return Err(AuthorityError::PrincipalMismatch);
    }
    if grant.principal.uid != presentation.uid {
        return Err(AuthorityError::PrincipalMismatch);
    }
    // Set only by the relay path, and only for the cgroup comparison.
    let mut skip_unit_check = false;
    match grant.binding {
        Binding::Process => {
            if grant.principal.pid != presentation.pid {
                return Err(AuthorityError::PrincipalMismatch);
            }
            if presentation.start_time_ticks.is_some()
                && grant.principal.start_time_ticks != presentation.start_time_ticks
            {
                return Err(AuthorityError::PrincipalMismatch);
            }
        }
        Binding::ProcessTree => {
            let in_tree = grant.principal.pid == presentation.pid
                || crate::proc::process_descends_from(presentation.pid, grant.principal.pid);
            // A relay presents a session grant from outside the tree.
            // That is only reachable through a relay grant this same
            // process already proved it holds, bound `Process`-tight to
            // it and naming this exact session, so the identity check
            // has already happened — one layer out.
            let relayed = relay.is_some_and(|proof| proof.covers(grant));
            if !in_tree && !relayed {
                return Err(AuthorityError::PrincipalMismatch);
            }
            // The bound principal is a sandboxed worker whose cgroup is
            // by construction not the launcher's, so that one
            // comparison is skipped — and only that one. Audience and
            // subject are still decided below: a relay proves *who is
            // speaking*, never *what may be said*.
            skip_unit_check = !in_tree;
        }
    }
    if !skip_unit_check {
        if let (Some(bound), Some(current)) = (
            grant.principal.unit.as_deref(),
            read_presenting_unit(presentation.pid),
        ) {
            if bound != current {
                return Err(AuthorityError::PrincipalMismatch);
            }
        }
    }
    if !grant.audience.contains(presentation.audience) {
        return Err(AuthorityError::Audience {
            route: presentation.route,
        });
    }
    if let Some(session_id) = presentation.session_id.as_deref() {
        if grant.subject.session_id.as_deref() != Some(session_id) {
            return Err(AuthorityError::Subject);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_presenting_unit(pid: u32) -> Option<String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let line = raw.lines().next()?.trim().to_string();
    (!line.is_empty() && line.len() <= 512).then_some(line)
}

#[cfg(not(target_os = "linux"))]
fn read_presenting_unit(_pid: u32) -> Option<String> {
    None
}

/// The daemon-wide store.
pub fn authority() -> &'static Authority {
    static AUTHORITY: OnceLock<Authority> = OnceLock::new();
    AUTHORITY.get_or_init(Authority::new)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/authority/store.rs"
    ));
}
