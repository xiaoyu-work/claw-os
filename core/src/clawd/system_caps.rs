use crate::caps::{Cap, CapSet, Risk, Scope, ScopeKind};
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

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/system_caps.rs"
    ));
}
