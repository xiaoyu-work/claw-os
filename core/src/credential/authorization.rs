use super::*;

pub(super) fn validate_credential_component(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!(
            "{kind} must be alphanumeric (hyphens/underscores allowed)"
        ));
    }
    Ok(())
}

pub(super) fn credential_scope(namespace: &str, name: &str) -> Result<Scope, String> {
    let id = CredentialId::parse(namespace, name).map_err(|error| error.to_string())?;
    Ok(Scope::name(format!("{}/{}", id.namespace(), id.name())))
}

pub(super) fn namespace_scope(namespace: &str) -> Result<Scope, String> {
    let namespace = NamespaceId::parse(namespace).map_err(|error| error.to_string())?;
    Ok(Scope::name(format!("{}/*", namespace.as_str())))
}

pub(super) fn bundle_scope(namespace: &str, bundle: &str) -> Result<Scope, String> {
    validate_credential_component("namespace", namespace)?;
    validate_credential_component("bundle name", bundle)?;
    Ok(Scope::name(format!("{namespace}/bundles/{bundle}")))
}

pub(super) fn require_secret(verb: Verb, scope: Scope) -> Result<(), String> {
    require_or_json(verb, scope).map_err(|value| value.to_string())
}

// ===========================================================================
// Expiry helpers
// ===========================================================================

/// Check whether a credential with the given `expires_at` has expired.
pub(super) fn is_expired(expires_at: &Option<String>) -> bool {
    if let Some(exp) = expires_at {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(exp, "%Y-%m-%dT%H:%M:%SZ") {
            return chrono::Utc::now().naive_utc() > dt;
        }
    }
    false
}

// ===========================================================================
// Tier comparison
// ===========================================================================

/// Returns `true` iff a session running at `session_tier` has *enough*
/// privilege to access a credential whose minimum tier is `min_tier`.
///
/// **Tier semantics, easy to misread:** lower number = MORE privileged.
///   * 0 = ROOT      (strongest)
///   * 1 = OPERATE
///   * 2 = APP
///   * 3 = SANDBOX   (weakest)
///
/// Therefore a session is "strong enough" exactly when its number is
/// less-than-or-equal-to the credential's `min_tier`.
pub(super) fn tier_grants_access(session_tier: u8, min_tier: u8) -> bool {
    session_tier <= min_tier
}

pub(super) fn require_credential_access(
    cred: &StoredCredential,
    namespace: &str,
    name: &str,
    current_tier: u8,
) -> Result<(), String> {
    if cred.name != name || cred.namespace != namespace {
        return Err("credential metadata does not match its storage path".to_string());
    }
    if !tier_grants_access(current_tier, cred.min_tier) {
        return Err(format!(
            "insufficient tier: credential '{}' requires tier {} or stronger (lower number), current session has tier {}",
            name, cred.min_tier, current_tier
        ));
    }
    Ok(())
}

/// Resolve the *effective* tier for the current request, fail-closed.
///
/// Previous behaviour was `policy::current_tier().unwrap_or(0)` which silently
/// granted ROOT whenever the policy registry could not be loaded — a clear
/// fail-open default for a privilege check. We now distinguish:
///
///   * No `COS_SESSION` env var at all → direct interactive CLI, tier 0
///     (matches historical UX where a human at a shell is treated as root on
///     their own machine).
///   * `COS_SESSION` set but the registry lookup fails or returns no tier →
///     `u8::MAX`, i.e. the weakest possible tier. This causes
///     [`tier_grants_access`] to deny everything except `min_tier == u8::MAX`
///     credentials (none exist in practice), so a missing/corrupt registry can
///     never silently elevate.
pub(super) fn effective_session_tier() -> u8 {
    match crate::proc::current_session_id() {
        None => 0,
        Some(_) => policy::current_tier().unwrap_or(u8::MAX),
    }
}
