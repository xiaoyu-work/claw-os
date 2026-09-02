//! What a grant asserts, and the rules a child may be derived under.
//!
//! A grant is the daemon's own record of authority it decided to hand
//! out. It is never parsed from a request, never reconstructed from a
//! serialized [`CapSet`], and never widened after issuance. Everything
//! that decides whether it may be exercised is recorded here at
//! issuance time:
//!
//! * the **principal** the kernel named — uid, pid, process start time,
//!   and the cgroup/unit path when `/proc` reports one;
//! * the **subject** it acts for — session, App and task identity;
//! * the **audience** it is good for — the route families that may
//!   resolve it, so a grant minted for an App launch is meaningless on
//!   a system-service route;
//! * the exact **capabilities** it carries;
//! * the **issuer**, issue and expiry instants, remaining **use
//!   budget**, revocation state, and **lineage** back to the parent it
//!   was attenuated from.
//!
//! Attenuation is the only way a grant is derived from another, and it
//! is monotonic in every dimension: caps shrink, audience shrinks,
//! expiry moves earlier, the use budget shrinks, depth grows. The
//! checks live in [`Attenuation::apply`] so no caller can assemble a
//! child that skips one.

use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, ScopeKind, Verb};

use super::handle::{GrantId, HandleKey};

/// Longest lineage the store will build. A chain this deep already
/// means something is deriving grants in a loop.
pub const MAX_LINEAGE_DEPTH: u16 = 8;

/// Children one grant may have outstanding at once.
pub const MAX_CHILDREN: u32 = 64;

/// Route families a grant may be resolved for.
///
/// One route declares exactly one audience. A grant carries the set it
/// was issued for, and a child may only ever carry a subset, so
/// authority cannot be re-pointed at a different part of the surface
/// after the fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Audience {
    /// Daemon health and status.
    Daemon,
    /// Agent task submission, inspection and cancellation.
    Task,
    /// One capability approved for one supervised Agent worker.
    AgentWorker,
    /// Memory, context and journal reads.
    Context,
    /// The consent surface itself.
    Permission,
    /// Privileged-mutation transactions.
    Transaction,
    /// Minting and lifecycle of App/MCP sessions.
    AppLaunch,
    /// The proactive scheduler.
    Scheduler,
    /// Credential material.
    Credential,
    /// Privileged system providers (audio, packages, users, …).
    SystemService,
}

impl Audience {
    pub fn as_str(self) -> &'static str {
        match self {
            Audience::Daemon => "daemon",
            Audience::Task => "task",
            Audience::AgentWorker => "agent-worker",
            Audience::Context => "context",
            Audience::Permission => "permission",
            Audience::Transaction => "transaction",
            Audience::AppLaunch => "app-launch",
            Audience::Scheduler => "scheduler",
            Audience::Credential => "credential",
            Audience::SystemService => "system-service",
        }
    }

    fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// The audiences one grant is good for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudienceSet(u32);

impl AudienceSet {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub fn of(audiences: &[Audience]) -> Self {
        let mut set = Self::empty();
        for audience in audiences {
            set.0 |= audience.bit();
        }
        set
    }

    pub fn one(audience: Audience) -> Self {
        Self(audience.bit())
    }

    pub fn contains(self, audience: Audience) -> bool {
        self.0 & audience.bit() != 0
    }

    /// True when `self` is a subset of `other` — the only direction an
    /// attenuation may move.
    pub fn is_subset_of(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }

    pub fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn names(self) -> Vec<&'static str> {
        [
            Audience::Daemon,
            Audience::Task,
            Audience::AgentWorker,
            Audience::Context,
            Audience::Permission,
            Audience::Transaction,
            Audience::AppLaunch,
            Audience::Scheduler,
            Audience::Credential,
            Audience::SystemService,
        ]
        .into_iter()
        .filter(|audience| self.contains(*audience))
        .map(Audience::as_str)
        .collect()
    }
}

/// How tightly a grant is tied to the process that may exercise it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Binding {
    /// Only the exact process may present it. Used for handles that
    /// cross a socket to one named launcher.
    Process,
    /// The bound process or a descendant of it may present it. Used for
    /// App sessions, whose helper children legitimately act under the
    /// same authority; a same-uid sibling is still refused because it
    /// is not in the tree.
    ProcessTree,
}

/// The principal a grant was bound to, exactly as the kernel reported
/// it when the grant was issued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub uid: u32,
    pub pid: u32,
    /// Field 22 of `/proc/<pid>/stat`. Present for every grant the
    /// daemon issues on Linux; its absence is what makes a recycled pid
    /// undetectable, so issuance refuses to proceed without it.
    pub start_time_ticks: Option<u64>,
    /// `/proc/<pid>/cgroup`, when readable. Compared only when both
    /// sides have one, so a container or a kernel without the
    /// controller does not turn into a silent bypass.
    pub unit: Option<String>,
}

impl Principal {
    /// Read the identity of a live process. `None` when the process is
    /// gone or `/proc` cannot name it.
    pub fn of_process(uid: u32, pid: u32) -> Option<Self> {
        let start_time_ticks = crate::proc::read_start_time_ticks_pub(pid);
        start_time_ticks?;
        Some(Self {
            uid,
            pid,
            start_time_ticks,
            unit: read_unit(pid),
        })
    }

    /// Is the bound process still the one the grant was issued to?
    pub fn is_live(&self) -> bool {
        crate::proc::is_alive_with_start_time(self.pid, self.start_time_ticks)
    }
}

#[cfg(target_os = "linux")]
fn read_unit(pid: u32) -> Option<String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let line = raw.lines().next()?.trim().to_string();
    (!line.is_empty() && line.len() <= 512).then_some(line)
}

#[cfg(not(target_os = "linux"))]
fn read_unit(_pid: u32) -> Option<String> {
    None
}

/// Who or what the grant acts for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Subject {
    pub session_id: Option<String>,
    pub app_id: Option<String>,
    pub task_id: Option<String>,
}

impl Subject {
    pub fn session(session_id: impl Into<String>) -> Self {
        Self {
            session_id: Some(session_id.into()),
            ..Self::default()
        }
    }

    pub fn with_app(mut self, app_id: Option<String>) -> Self {
        self.app_id = app_id;
        self
    }

    pub fn with_task(mut self, task_id: Option<String>) -> Self {
        self.task_id = task_id;
        self
    }
}

/// Where a grant came from. Recorded for audit and for the migration
/// rule that refuses authority with no provenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Issuer {
    /// Minted for a launcher registering an App/MCP session.
    AppSessionAuthority,
    /// Minted by the MCP App Gateway for one target call after the
    /// caller and manifest-derived target capabilities are authorized.
    AppGateway,
    /// Minted for a session the daemon itself entered (scheduler
    /// delegation, system Agent task).
    TrustedSession,
    /// Minted when the owner approved an exact capability.
    Approval,
    /// Minted for a `clawd`-run scheduler command.
    Scheduler,
}

impl Issuer {
    pub fn as_str(self) -> &'static str {
        match self {
            Issuer::AppSessionAuthority => "app-session",
            Issuer::AppGateway => "app-gateway",
            Issuer::TrustedSession => "trusted-session",
            Issuer::Approval => "approval",
            Issuer::Scheduler => "scheduler",
        }
    }
}

/// Remaining uses of a grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uses {
    /// Bounded only by expiry and revocation. Used for the session-long
    /// authority a running App holds over its own provider routes.
    Unbounded,
    /// Exactly `n` more uses. Reaching zero retires the grant in the
    /// same transaction that spent it, so a one-shot cannot be spent
    /// twice by two concurrent callers.
    Budget(u32),
}

impl Uses {
    pub fn remaining(self) -> Option<u32> {
        match self {
            Uses::Unbounded => None,
            Uses::Budget(remaining) => Some(remaining),
        }
    }

    pub fn is_exhausted(self) -> bool {
        matches!(self, Uses::Budget(0))
    }

    /// May `self` be handed to a child? A child never gets more than
    /// the parent has left, and an unbounded parent may still bound its
    /// child.
    fn covers(self, requested: Uses) -> bool {
        match (self, requested) {
            (Uses::Unbounded, _) => true,
            (Uses::Budget(_), Uses::Unbounded) => false,
            (Uses::Budget(have), Uses::Budget(want)) => want <= have,
        }
    }
}

/// One grant the authority holds.
#[derive(Debug)]
pub struct Grant {
    pub(crate) id: GrantId,
    pub(crate) key: HandleKey,
    pub(crate) issuer: Issuer,
    pub(crate) principal: Principal,
    pub(crate) binding: Binding,
    pub(crate) subject: Subject,
    pub(crate) audience: AudienceSet,
    pub(crate) caps: CapSet,
    pub(crate) issued_at: Instant,
    pub(crate) expires_at: Instant,
    pub(crate) uses: Uses,
    pub(crate) revoked: bool,
    /// Bumped by every revocation pass that touches this grant, so a
    /// decision taken against generation `n` can be recognised as stale
    /// if it is ever replayed.
    pub(crate) generation: u64,
    pub(crate) parent: Option<GrantId>,
    pub(crate) depth: u16,
    pub(crate) children: u32,
}

impl Grant {
    pub fn id(&self) -> GrantId {
        self.id
    }

    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }

    /// A grant is usable when it is not revoked, not expired, not
    /// exhausted, and its process is still the one it was bound to.
    pub fn is_live(&self, now: Instant) -> bool {
        !self.revoked
            && !self.is_expired(now)
            && !self.uses.is_exhausted()
            && self.principal.is_live()
    }
}

/// A request to issue a grant. Every field is daemon-derived; nothing
/// here may be copied out of a request body.
#[derive(Debug)]
pub struct Issuance {
    pub issuer: Issuer,
    pub principal: Principal,
    pub binding: Binding,
    pub subject: Subject,
    pub audience: AudienceSet,
    pub caps: CapSet,
    pub lifetime: Duration,
    pub uses: Uses,
    /// Claim the session index, so a request that names this session
    /// resolves to this grant. Exactly one grant per session claims it;
    /// a second claim is refused rather than replacing the first, so a
    /// re-registration cannot re-point a live session id at wider
    /// authority.
    pub index_session: bool,
}

/// A request to derive a child grant from a parent.
#[derive(Debug)]
pub struct Attenuation {
    pub issuer: Issuer,
    pub principal: Principal,
    pub binding: Binding,
    pub subject: Subject,
    pub audience: AudienceSet,
    pub caps: CapSet,
    pub lifetime: Duration,
    pub uses: Uses,
    pub index_session: bool,
}

/// Why an attenuation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttenuationError {
    /// The child asked for a capability the parent does not cover.
    CapabilityWiden { verb: &'static str },
    /// The child asked for `Scope::Wild` on a verb that addresses a
    /// real resource namespace, or on a verb where the parent does not
    /// already hold `Wild`.
    WildIntroduced { verb: &'static str },
    /// The child asked for an audience the parent does not hold.
    AudienceWiden,
    /// The child asked to outlive its parent.
    LifetimeExtended,
    /// The child asked for more uses than the parent has left.
    UseBudgetIncreased,
    /// The child would sit deeper than [`MAX_LINEAGE_DEPTH`].
    DepthExceeded,
    /// The parent already has [`MAX_CHILDREN`] outstanding.
    TooManyChildren,
    /// The parent is revoked, expired, exhausted or its process is
    /// gone.
    ParentNotLive,
    /// The child would act for a different owner.
    OwnerChanged,
}

impl std::fmt::Display for AttenuationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttenuationError::CapabilityWiden { verb } => {
                write!(f, "child grant would widen `{verb}` beyond its parent")
            }
            AttenuationError::WildIntroduced { verb } => {
                write!(f, "child grant would introduce an unbounded `{verb}` scope")
            }
            AttenuationError::AudienceWiden => {
                f.write_str("child grant would reach an audience its parent cannot")
            }
            AttenuationError::LifetimeExtended => {
                f.write_str("child grant would outlive its parent")
            }
            AttenuationError::UseBudgetIncreased => {
                f.write_str("child grant would carry more uses than its parent has left")
            }
            AttenuationError::DepthExceeded => f.write_str("grant lineage is too deep"),
            AttenuationError::TooManyChildren => {
                f.write_str("grant already has the maximum number of children")
            }
            AttenuationError::ParentNotLive => {
                f.write_str("parent grant is revoked, expired, exhausted or orphaned")
            }
            AttenuationError::OwnerChanged => {
                f.write_str("child grant would act for a different owner")
            }
        }
    }
}

impl Attenuation {
    /// Check every monotonic property against `parent`.
    ///
    /// Deliberately total: each dimension is refused with its own
    /// reason rather than being clamped, so a caller that asked for
    /// more than it may have gets an error instead of a silently
    /// narrowed grant it will later be surprised by.
    pub fn check(&self, parent: &Grant, now: Instant) -> Result<Instant, AttenuationError> {
        if !parent.is_live(now) {
            return Err(AttenuationError::ParentNotLive);
        }
        if parent.depth + 1 > MAX_LINEAGE_DEPTH {
            return Err(AttenuationError::DepthExceeded);
        }
        if parent.children >= MAX_CHILDREN {
            return Err(AttenuationError::TooManyChildren);
        }
        if self.principal.uid != parent.principal.uid {
            return Err(AttenuationError::OwnerChanged);
        }
        if !self.audience.is_subset_of(parent.audience) {
            return Err(AttenuationError::AudienceWiden);
        }
        for cap in self.caps.iter() {
            if matches!(cap.scope, Scope::Wild) && !wild_is_canonical(cap.verb, &parent.caps) {
                return Err(AttenuationError::WildIntroduced {
                    verb: cap.verb.as_str(),
                });
            }
            if !parent.caps.covers(cap) {
                return Err(AttenuationError::CapabilityWiden {
                    verb: cap.verb.as_str(),
                });
            }
        }
        if !parent.uses.covers(self.uses) {
            return Err(AttenuationError::UseBudgetIncreased);
        }
        let expires_at = now + self.lifetime;
        if expires_at > parent.expires_at {
            return Err(AttenuationError::LifetimeExtended);
        }
        Ok(expires_at)
    }
}

/// May a child carry `Scope::Wild` for this verb?
///
/// Only when the parent already holds `Wild` for it *and* the catalog
/// says the verb has no narrower representable scope — a
/// self-referential verb, or one that carries no resource at all.
/// `Wild` on a Path/Host/Name verb covers every scope of every kind, so
/// introducing it there is indistinguishable from `fs.read:/**` and is
/// refused even when the parent holds it.
fn wild_is_canonical(verb: Verb, parent: &CapSet) -> bool {
    let canonical = matches!(
        crate::caps::lookup_meta(verb).map(|meta| meta.scope_kind),
        Some(ScopeKind::SelfRef) | Some(ScopeKind::Wild) | Some(ScopeKind::None)
    );
    canonical
        && parent
            .iter()
            .any(|held| held.verb == verb && matches!(held.scope, Scope::Wild))
}

/// The capability a route requires, resolved from its typed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requirement {
    /// The route needs no capability: identity and audience are the
    /// whole decision (daemon health, the caller's own tasks, the
    /// consent surface).
    None,
    /// The exact capabilities this request needs, canonicalized by the
    /// owning route from its validated body.
    Exact(Vec<Cap>),
    /// The owning route derives the exact capability from state only it
    /// can canonicalize, and must exercise
    /// [`super::Decision::require_all`] before it may answer. The
    /// middleware refuses the response if it did not.
    RouteDerived,
}

impl Requirement {
    pub fn exact(caps: impl IntoIterator<Item = Cap>) -> Self {
        Requirement::Exact(caps.into_iter().collect())
    }

    pub fn is_route_derived(&self) -> bool {
        matches!(self, Requirement::RouteDerived)
    }
}
