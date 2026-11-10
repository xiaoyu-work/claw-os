use super::{Scope, Verb};
use crate::caps::args::PathContext;

pub(super) fn validate(verb: Verb, path: &str) -> Result<(), String> {
    if crate::caps::lookup_meta(verb).map(|meta| meta.scope_kind) != Some(crate::caps::ScopeKind::Path) {
        return Err("owner-path requires a Path-kind capability verb".into());
    }
    claw_os_sdk::manifest::validate_owner_path(verb.as_str(), path)
}

/// Resolve against a Root-derived owner context, never environment or argv.
/// Normal filesystem/protected-resource and launch pinning checks still apply.
pub fn resolve_owner_path(verb: Verb, path: &str, paths: &PathContext) -> Result<Scope, String> {
    validate(verb, path)?;
    let home = paths.home.to_str().ok_or("authenticated owner home is not UTF-8")?;
    let resolved = claw_os_sdk::manifest::resolve_owner_path(verb.as_str(), path, home)?;
    let bound = Scope::path(format!("{home}/**")).canonicalized();
    let scope = Scope::path(resolved).canonicalized();
    if bound.is_wildcard() || scope.is_wildcard() || !bound.covers(&scope) {
        return Err("owner-path escapes the authenticated owner home".into());
    }
    Ok(scope)
}
