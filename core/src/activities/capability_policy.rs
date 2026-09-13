//! Pure Activity capability constraints. Normal means ordinary authorization,
//! not permission; neither a policy nor its decision is authority.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::caps::{lookup_meta, Cap, Scope, ScopeKind, Verb};

use super::ActivityError;

pub(super) const MAX_POLICY_BYTES: usize = 16 * 1024;
const MAX_RULES: usize = 64;
const MAX_SCOPES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityRuleMode {
    Normal,
    RequireApproval,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCapabilityRule {
    pub verb: Verb,
    pub mode: CapabilityRuleMode,
    pub scopes: Vec<Scope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityBoundaryDecision {
    Normal,
    RequireApproval,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPolicyDraft {
    pub rules: Vec<ActivityCapabilityRule>,
}

impl CapabilityPolicyDraft {
    pub fn validate(&self) -> Result<(), ActivityError> {
        if self.rules.len() > MAX_RULES {
            return invalid("capability policy exceeds 64 rules");
        }
        let mut verbs = BTreeSet::new();
        let mut text_bytes = 0;
        for rule in &self.rules {
            if lookup_meta(rule.verb).is_none() {
                return invalid("capability policy verb is missing from the catalog");
            }
            if !verbs.insert(rule.verb.as_str()) {
                return invalid("capability policy must have only one rule per verb");
            }
            if rule.mode == CapabilityRuleMode::Deny {
                if !rule.scopes.is_empty() {
                    return invalid("a deny rule must have no scopes and denies the entire verb");
                }
            } else if rule.scopes.is_empty() || rule.scopes.len() > MAX_SCOPES {
                return invalid("normal and require_approval rules require 1..=32 scopes");
            }
            for scope in &rule.scopes {
                validate_scope(rule.verb, scope)?;
                text_bytes += scope_text(scope).map_or(0, str::len);
                if text_bytes > MAX_POLICY_BYTES {
                    return invalid("capability policy exceeds 16 KiB");
                }
            }
        }
        if serde_json::to_vec(self)?.len() > MAX_POLICY_BYTES {
            return invalid("serialized capability policy exceeds 16 KiB");
        }
        Ok(())
    }

    pub(super) fn canonicalized(mut self) -> Result<Self, ActivityError> {
        self.validate()?;
        for rule in &mut self.rules {
            for scope in &mut rule.scopes {
                if let Scope::Host(host) = scope {
                    host.make_ascii_lowercase();
                }
            }
            rule.scopes.sort_by_cached_key(ToString::to_string);
            rule.scopes.dedup();
        }
        self.rules.sort_by_key(|rule| rule.verb.as_str());
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCapabilityPolicy {
    pub activity_id: String,
    pub owner_uid: u32,
    pub revision: u64,
    pub enabled: bool,
    pub rules: Vec<ActivityCapabilityRule>,
    pub created_at: String,
    pub updated_at: String,
}

impl ActivityCapabilityPolicy {
    /// Evaluate already validated canonical capability requests without I/O.
    /// The caller must still perform ordinary authorization and any required
    /// exact confirmation; this method never creates or consumes either.
    pub fn decision(&self, requested: &Cap) -> CapabilityBoundaryDecision {
        if !self.enabled {
            return CapabilityBoundaryDecision::Deny;
        }
        let mut matching = self.rules.iter().filter(|rule| rule.verb == requested.verb);
        let Some(rule) = matching.next() else {
            return CapabilityBoundaryDecision::Normal;
        };
        if matching.next().is_some()
            || rule.mode == CapabilityRuleMode::Deny
            || !rule
                .scopes
                .iter()
                .any(|scope| super::capability_scope_covers(rule.verb, scope, &requested.scope))
        {
            return CapabilityBoundaryDecision::Deny;
        }
        match rule.mode {
            CapabilityRuleMode::Normal => CapabilityBoundaryDecision::Normal,
            CapabilityRuleMode::RequireApproval => CapabilityBoundaryDecision::RequireApproval,
            CapabilityRuleMode::Deny => CapabilityBoundaryDecision::Deny,
        }
    }
}

fn validate_scope(verb: Verb, scope: &Scope) -> Result<(), ActivityError> {
    let expected = lookup_meta(verb)
        .ok_or_else(|| {
            ActivityError::Invalid("capability verb is missing from the catalog".into())
        })?
        .scope_kind;
    if matches!(scope, Scope::Wild) {
        if crate::provenance::ceiling::verb_addresses_a_resource(verb) {
            return invalid(
                "resource-addressing verbs require an explicit same-kind scope, not raw Wild",
            );
        }
        return Ok(());
    }
    if scope.kind() != expected || matches!(expected, ScopeKind::None | ScopeKind::Wild) {
        return invalid("capability policy scope kind does not match its catalog verb");
    }
    if let Some(text) = scope_text(scope) {
        if text.is_empty() || text.len() > MAX_POLICY_BYTES || text.chars().any(char::is_control) {
            return invalid("capability scopes must be nonempty, bounded and contain no controls");
        }
    }
    if let Scope::Path(value) = scope {
        let path = Path::new(value);
        let canonical: PathBuf = path.components().collect();
        if !value.starts_with('/')
            || !path.is_absolute()
            || value.contains('$')
            || canonical.as_os_str() != path.as_os_str()
            || path
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        {
            return invalid("capability path scopes require canonical absolute paths without placeholders or dot segments");
        }
    }
    Ok(())
}

fn scope_text(scope: &Scope) -> Option<&str> {
    match scope {
        Scope::Path(value) | Scope::Host(value) | Scope::Name(value) | Scope::SelfRef(value) => {
            Some(value)
        }
        Scope::Wild => None,
    }
}

/// Pure, conservative containment for scopes of one catalog verb.
/// Callers comparing Caps must first require equal verbs. Invalid or
/// noncanonical scopes are not covered; no resource or authority is resolved.
pub(crate) fn capability_scope_covers(verb: Verb, boundary: &Scope, requested: &Scope) -> bool {
    if validate_scope(verb, boundary).is_err() || validate_scope(verb, requested).is_err() {
        return false;
    }
    if matches!(boundary, Scope::Wild) {
        // Validation only admits Wild for catalog-canonical self/unscoped verbs.
        return boundary.covers(requested);
    }
    if boundary.kind() != requested.kind() {
        return false;
    }
    if !symbolic_request_is_covered(boundary, requested) {
        return false;
    }
    match (boundary, requested) {
        (Scope::Path(boundary), Scope::Path(requested)) => {
            // Scope::covers resolves Path values. Canonical absolute paths
            // instead reuse the same pure slash-segment glob engine via Name.
            // A shared leading segment preserves root and single-star semantics.
            let lexical = |path: &str| Scope::name(format!("path{}", path.trim_end_matches('/')));
            lexical(boundary).covers(&lexical(requested))
        }
        _ => boundary.covers(requested),
    }
}

fn symbolic_request_is_covered(boundary: &Scope, requested: &Scope) -> bool {
    let (pattern, request) = match (boundary, requested) {
        (Scope::Path(pattern), Scope::Path(request))
        | (Scope::Name(pattern), Scope::Name(request)) => (pattern, request),
        (Scope::Host(pattern), Scope::Host(request)) => {
            return !request.contains('*')
                || pattern.eq_ignore_ascii_case(request)
                || pattern == "**"
                || pattern.strip_prefix("**:").is_some_and(|port| {
                    !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
                });
        }
        _ => return true,
    };
    if !request.contains('*') || pattern == request {
        return true;
    }
    if (matches!(boundary, Scope::Path(_)) && pattern == "/**")
        || (matches!(boundary, Scope::Name(_)) && pattern == "**")
    {
        return true;
    }
    // covers() matches a literal target, not arbitrary glob-language inclusion.
    // A literal subtree ending in /** also proves containment of child patterns;
    // otherwise require an equivalent pattern rather than accept a partial cover.
    pattern.strip_suffix("/**").is_some_and(|prefix| {
        !prefix.contains('*')
            && request
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with('/'))
    })
}

fn invalid<T>(message: impl Into<String>) -> Result<T, ActivityError> {
    Err(ActivityError::Invalid(message.into()))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/capability_policy.rs"
    ));
}
