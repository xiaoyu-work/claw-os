use crate::caps::{Cap, CapSet, Risk, Scope, ScopeKind, Verb};
use std::path::Path;

/// Default capabilities for the first-party system agent.
///
/// The capability catalog is the policy source of truth: low- and
/// medium-risk actions run directly, while high- and critical-risk actions
/// are deliberately absent and flow through the approval queue on demand.
pub fn system_agent_caps(owner_home: Option<&Path>) -> CapSet {
    let mut caps = CapSet::new();

    for meta in crate::caps::catalog::CATALOG {
        if meta.risk > Risk::Medium {
            continue;
        }
        let scope = match meta.scope_kind {
            ScopeKind::Path if meta.risk == Risk::Medium => {
                let Some(home) = owner_home else {
                    continue;
                };
                Scope::path(format!("{}/**", home.display()))
            }
            ScopeKind::Path => Scope::path("/**"),
            ScopeKind::Host => Scope::host("**"),
            ScopeKind::Name => Scope::name("**"),
            ScopeKind::SelfRef | ScopeKind::Wild | ScopeKind::None => Scope::Wild,
        };
        caps.insert(Cap::new(meta.verb, scope));
    }

    caps
}

/// Authority that must never be delegated to a launcher the daemon
/// could not tie to a registered session, regardless of catalog risk.
///
/// Every verb here mutates machine-wide state or hands out credentials,
/// which is exactly the escalation an unauthenticated local process
/// would gain by asking for an App that declares it. Obtaining one of
/// these requires an authenticated parent session or an approved
/// permission grant.
const LOCAL_LAUNCH_DENIED_VERBS: &[Verb] = &[
    Verb::SYS_CONFIG,
    Verb::SYS_PACKAGE,
    Verb::SYS_IDENTITY,
    Verb::SYS_SERVICE,
    Verb::SYS_STORAGE,
    Verb::SYS_MOUNT,
    Verb::SYS_SNAPSHOT,
    Verb::SYS_SECURITY,
    Verb::SYS_POWER,
    Verb::NET_FIREWALL,
    Verb::NET_MANAGE,
    Verb::DATA_BACKUP,
    Verb::SECRET_READ,
    Verb::SECRET_WRITE,
    Verb::SECRET_GRANT,
];

/// Ceiling `clawd` delegates to a launcher it could not tie to a
/// registered session — the interactive `cos` CLI and the desktop
/// launcher.
///
/// This is a *policy* value owned by the daemon, not something a caller
/// may assert, and it is deliberately unprivileged: low/medium-risk
/// verbs only, never the machine-mutating set above, and never global
/// filesystem authority. Path scopes are bounded to the caller's own
/// home so an App launched without an authenticated parent cannot read
/// or write outside it. Anything beyond this ceiling has to come from
/// an authenticated parent session or an approved permission grant.
pub fn local_launcher_ceiling(owner_home: &Path) -> CapSet {
    let mut caps = CapSet::new();

    for meta in crate::caps::catalog::CATALOG {
        if meta.risk > Risk::Medium || LOCAL_LAUNCH_DENIED_VERBS.contains(&meta.verb) {
            continue;
        }
        let scope = match meta.scope_kind {
            ScopeKind::Path => Scope::path(format!("{}/**", owner_home.display())),
            ScopeKind::Host => Scope::host("**"),
            ScopeKind::Name => Scope::name("**"),
            // `SelfRef` addresses the session's own children and has no
            // narrower representable wildcard; `Wild`/`None` verbs carry
            // no resource at all.
            ScopeKind::SelfRef | ScopeKind::Wild | ScopeKind::None => Scope::Wild,
        };
        caps.insert(Cap::new(meta.verb, scope));
    }

    caps
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/system_caps.rs"
    ));
}
