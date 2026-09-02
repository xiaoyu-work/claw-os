//! The provenance capability ceiling.
//!
//! A package's trust tier is not a label — it is an upper bound on what
//! the package may ever be granted, applied wherever authority is
//! derived. Signed publisher content and root-owned vendor content keep
//! the full manifest-declared surface. Unsigned content running under a
//! [`TrustTier::Developer`] grant is deliberately crippled: the
//! operator agreed to run code nobody signed, so it gets the smallest
//! set that still allows a developer to iterate.
//!
//! ## What Developer may hold
//!
//! Only verbs on [`DEVELOPER_ALLOWED`]: reading and writing the
//! package's own data, its own agent memory, and harmless output
//! (`ui.notify`, log writes). Everything else is denied, including
//! every `sys.*`, `secret.*`, `net.*`, process control, `fs.exec`,
//! agent spawn/delegate, device, clipboard, UI input and browser verb.
//!
//! The allow-list is closed on purpose: a verb added to
//! [`crate::caps::verb::ALL_VERBS`] tomorrow is denied to developer
//! content until somebody deliberately adds it here, and a test asserts
//! exactly that.
//!
//! ## What Developer may never do regardless of verb
//!
//! * hold [`Scope::Wild`] — an unsigned package cannot get a wildcard
//!   over any resource kind;
//! * reach a privileged broker route ([`Ceiling::allows_audience`]);
//! * invoke a different App's identity;
//! * mount any host path beyond its own package (read-only) and its own
//!   App data partition.

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::trust::TrustTier;

/// Does this verb name a resource namespace a wildcard could widen?
///
/// Fails closed: a verb missing from the catalog is treated as
/// resource-addressing, so an unsigned package cannot reach it with
/// [`Scope::Wild`].
pub fn verb_addresses_a_resource(verb: Verb) -> bool {
    use crate::caps::ScopeKind;
    match crate::caps::lookup_meta(verb).map(|meta| meta.scope_kind) {
        Some(ScopeKind::Path | ScopeKind::Host | ScopeKind::Name) => true,
        Some(_) => false,
        None => true,
    }
}

/// Verbs an unsigned, developer-trusted package may hold.
///
/// Chosen so a developer can build and exercise an App's own logic:
/// its manifest, its data partition, its own memory and a notification.
/// Nothing here reaches another App, the system, the network, a secret
/// or a device.
pub const DEVELOPER_ALLOWED: &[Verb] = &[
    // The package's own data partition. `fs.read`/`fs.write` are still
    // scope-checked; the worker policy separately refuses to mount
    // anything outside the package and its own data dir.
    Verb::FS_READ,
    Verb::FS_WRITE,
    Verb::FS_META,
    // Own key/value and log rows, scoped by the manifest as usual.
    Verb::DATA_KV_READ,
    Verb::DATA_KV_WRITE,
    Verb::DATA_LOG_READ,
    Verb::DATA_LOG_WRITE,
    // Own agent memory.
    Verb::MEMORY_READ,
    Verb::MEMORY_WRITE,
    // Harmless output.
    Verb::UI_NOTIFY,
];

/// Broker audiences a tier may present a grant for.
///
/// `AppRelay`, `SystemService` and `Credential` are the three that
/// carry real authority off the package: relaying an App session grant,
/// driving a privileged `system.*` route, and refreshing a credential.
/// Developer content is refused all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    AgentWorker,
    AppLaunch,
    AppRelay,
    SystemService,
    Credential,
    Scheduler,
    Permission,
    Transaction,
    Context,
    Notification,
    Task,
    Daemon,
}

impl Audience {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentWorker => "agent-worker",
            Self::AppLaunch => "app-launch",
            Self::AppRelay => "app-relay",
            Self::SystemService => "system-service",
            Self::Credential => "credential",
            Self::Scheduler => "scheduler",
            Self::Permission => "permission",
            Self::Transaction => "transaction",
            Self::Context => "context",
            Self::Notification => "notification",
            Self::Task => "task",
            Self::Daemon => "daemon",
        }
    }
}

/// The ceiling implied by one package's trust tier and identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ceiling {
    tier: TrustTier,
    /// The package's own id, when known. Only used to allow a developer
    /// package to invoke *itself*; every other App identity is refused.
    own_id: Option<String>,
}

impl Ceiling {
    /// A tier-only ceiling. Use [`Ceiling::for_package`] wherever the
    /// package id is known: without it a developer package cannot even
    /// invoke itself.
    pub fn for_tier(tier: TrustTier) -> Self {
        Self { tier, own_id: None }
    }

    pub fn for_package(tier: TrustTier, own_id: impl Into<String>) -> Self {
        Self {
            tier,
            own_id: Some(own_id.into()),
        }
    }

    pub fn tier(&self) -> TrustTier {
        self.tier
    }

    pub fn own_id(&self) -> Option<&str> {
        self.own_id.as_deref()
    }

    /// True when the tier is the restricted developer tier.
    pub fn is_developer(&self) -> bool {
        matches!(self.tier, TrustTier::Developer)
    }

    pub fn label(&self) -> &'static str {
        self.tier.as_str()
    }

    /// May a package at this tier inherit a manifest `wild` need?
    ///
    /// A `wild` scope binding asks the kernel to hand the package
    /// whatever the launching session holds for that verb. Nobody
    /// vouched for developer content, so it never borrows the
    /// launcher's reach over a real resource; resourceless verbs, whose
    /// only representable scope *is* wild, are unaffected.
    pub fn allows_wild_binding(&self, verb: Verb) -> bool {
        if !self.is_developer() {
            return true;
        }
        !verb_addresses_a_resource(verb)
    }

    /// May a package at this tier hold `verb` at all?
    ///
    /// `agent.invoke` is the one verb whose answer depends on scope, so
    /// it is deliberately *not* allowed here — see [`Self::allows_cap`].
    pub fn allows_verb(&self, verb: Verb) -> bool {
        if !self.is_developer() {
            return true;
        }
        DEVELOPER_ALLOWED.contains(&verb)
    }

    /// May a package at this tier hold this exact capability?
    ///
    /// Developer content is additionally refused a wildcard scope over
    /// any real resource namespace: an unsigned package that declared
    /// `ScopeBinding::Wild` would otherwise inherit whatever the
    /// launching session happens to hold. Verbs that address no
    /// resource at all (`ui.notify`, `time.delay`) have no narrower
    /// form than [`Scope::Wild`], so for them it is the canonical
    /// scope rather than unbounded authority.
    ///
    /// `agent.invoke` is permitted only at the package's own id — an
    /// unsigned App may re-enter itself, never another App.
    pub fn allows_cap(&self, cap: &Cap) -> bool {
        if !self.is_developer() {
            return true;
        }
        if cap.scope.is_wildcard() && verb_addresses_a_resource(cap.verb) {
            return false;
        }
        if cap.verb == Verb::AGENT_INVOKE {
            return match (&self.own_id, &cap.scope) {
                (Some(own), Scope::Name(name)) => own == name,
                _ => false,
            };
        }
        DEVELOPER_ALLOWED.contains(&cap.verb)
    }

    /// May a package at this tier present a grant for `audience`?
    pub fn allows_audience(&self, audience: Audience) -> bool {
        if !self.is_developer() {
            return true;
        }
        matches!(audience, Audience::AppLaunch)
    }

    /// The audiences a grant for this package may carry, filtered from
    /// what the caller would otherwise request.
    ///
    /// Structural, not advisory: the daemon builds every launch,
    /// session and relay grant through this, so a developer package
    /// cannot be handed `AppRelay`, `SystemService` or `Credential`
    /// even by a caller that asked for them.
    pub fn permitted_audiences(&self, requested: &[Audience]) -> Vec<Audience> {
        requested
            .iter()
            .copied()
            .filter(|audience| self.allows_audience(*audience))
            .collect()
    }

    /// May a package at this tier act as an MCP server?
    ///
    /// A developer-trusted MCP package would be unsigned third-party
    /// code holding a live broker endpoint, so it is refused outright
    /// rather than attached with an empty capability set — a running
    /// server is a standing attack surface even with no authority.
    pub fn allows_mcp_attach(&self) -> bool {
        !self.is_developer()
    }

    /// May the launcher be given a relay grant for this package?
    ///
    /// The relay grant is the worker's only route to a privileged
    /// broker call. Developer content gets none, so the broker endpoint
    /// holds no handle and every relayed route is refused rather than a
    /// dead handle sitting in the slot.
    pub fn allows_relay(&self) -> bool {
        self.allows_audience(Audience::AppRelay)
    }

    /// May host paths granted by capabilities be mounted into the
    /// sandbox?
    ///
    /// Developer content sees only its own package (read-only) and its
    /// own App data partition; a granted path outside those is dropped
    /// by the worker policy even if the capability survived.
    pub fn allows_granted_path_mounts(&self) -> bool {
        !self.is_developer()
    }

    /// Does content from this tier count as untrusted model input?
    ///
    /// Skill text is always wrapped, but developer content additionally
    /// never claims a vendor or publisher source label.
    pub fn model_content_is_untrusted(&self) -> bool {
        !matches!(self.tier, TrustTier::Vendor)
    }

    /// Apply the ceiling to a resolved capability set, returning the
    /// capabilities that survive plus the ones that were dropped.
    ///
    /// Callers use the dropped list for the audit record: a developer
    /// package silently losing half its manifest would be baffling.
    pub fn clamp(&self, caps: &CapSet) -> (CapSet, Vec<Cap>) {
        let mut kept = CapSet::new();
        let mut dropped = Vec::new();
        for cap in caps.iter() {
            if self.allows_cap(cap) {
                kept.insert(cap.clone());
            } else {
                dropped.push(cap.clone());
            }
        }
        (kept, dropped)
    }

    /// Refuse an App identity that is not the package's own id.
    pub fn allows_app_identity(&self, package_id: &str, requested: &str) -> bool {
        if !self.is_developer() {
            return true;
        }
        package_id == requested
    }

    /// Drop from `plan` every capability this ceiling forbids, keeping
    /// the removed ones for the audit record.
    ///
    /// Used by the daemon before a plan reaches the approvals store, so
    /// a developer package can never consume an approval for a
    /// capability it could not hold anyway.
    pub fn clamp_vec(&self, caps: &[Cap]) -> (Vec<Cap>, Vec<Cap>) {
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for cap in caps {
            if self.allows_cap(cap) {
                kept.push(cap.clone());
            } else {
                dropped.push(cap.clone());
            }
        }
        (kept, dropped)
    }

    /// Scopes a developer package may be granted for its own data.
    pub fn developer_self_scope(package_id: &str) -> Scope {
        Scope::name(package_id)
    }

    /// Audit-safe projection.
    pub fn facts(&self) -> serde_json::Value {
        serde_json::json!({
            "tier": self.tier.as_str(),
            "developer_restricted": self.is_developer(),
            "allowed_verbs": if self.is_developer() {
                Some(
                    DEVELOPER_ALLOWED
                        .iter()
                        .map(|v| v.as_str())
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            },
        })
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/ceiling.rs"
    ));
}
