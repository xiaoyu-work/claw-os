//! Activity capability constraints. Policy data never supplies permission.

use std::collections::HashSet;

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_CAPABILITY_POLICY_RULES: usize = 64;
pub const MAX_CAPABILITY_POLICY_SCOPES: usize = 32;
pub const MAX_CAPABILITY_POLICY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityPolicyMode {
    Normal,
    RequireApproval,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CapabilityPolicyScope {
    Path { value: String },
    Host { value: String },
    Name { value: String },
    SelfRef { value: String },
    Wild {},
}

impl CapabilityPolicyScope {
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Path { value }
            | Self::Host { value }
            | Self::Name { value }
            | Self::SelfRef { value } => Some(value),
            Self::Wild {} => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPolicyRule {
    pub verb: String,
    pub mode: CapabilityPolicyMode,
    pub scopes: Vec<CapabilityPolicyScope>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPolicyDraft {
    pub rules: Vec<CapabilityPolicyRule>,
}

impl CapabilityPolicyDraft {
    /// The broker validates catalogue membership and compatible scope kinds.
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        validate_rules(&self.rules)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityCapabilityPolicy {
    pub activity_id: String,
    pub owner_uid: u32,
    pub revision: u64,
    pub enabled: bool,
    pub rules: Vec<CapabilityPolicyRule>,
    pub created_at: String,
    pub updated_at: String,
}

impl ActivityCapabilityPolicy {
    pub fn matches_activity(&self, activity_id: &str) -> bool {
        same_activity(&self.activity_id, activity_id)
            && self.revision > 0
            && validate_rules(&self.rules).is_ok()
            && DateTime::parse_from_rfc3339(&self.created_at).is_ok()
            && DateTime::parse_from_rfc3339(&self.updated_at).is_ok()
    }

    pub fn matches_owner(&self, activity_id: &str, owner_uid: u32) -> bool {
        self.matches_activity(activity_id) && self.owner_uid == owner_uid
    }

    pub fn matches_set(
        &self,
        activity_id: &str,
        request: &ActivityCapabilityPolicySetRequest,
    ) -> bool {
        self.matches_activity(activity_id)
            && request.validate_shape().is_ok()
            && next_revision(request.expected_revision) == Some(self.revision)
            && self.rules_match(&request.policy.rules)
            && (request.expected_revision.is_some() || self.enabled)
    }

    pub fn matches_enabled(
        &self,
        activity_id: &str,
        request: &ActivityCapabilityPolicyEnabledRequest,
    ) -> bool {
        self.matches_activity(activity_id)
            && request.validate_shape().is_ok()
            && next_revision(Some(request.expected_revision)) == Some(self.revision)
            && self.enabled == request.enabled
    }

    pub fn preserves_identity(&self, previous: &Self) -> bool {
        self.matches_owner(&previous.activity_id, previous.owner_uid)
            && same_instant(&self.created_at, &previous.created_at)
    }

    pub fn rules_match(&self, submitted: &[CapabilityPolicyRule]) -> bool {
        canonical_rules(&self.rules) == canonical_rules(submitted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityCapabilityPolicyResponse {
    pub schema: u32,
    pub activity_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub capability_policy: Option<ActivityCapabilityPolicy>,
}

impl ActivityCapabilityPolicyResponse {
    pub fn matches_activity(&self, activity_id: &str) -> bool {
        self.schema == 1
            && same_activity(&self.activity_id, activity_id)
            && self
                .capability_policy
                .as_ref()
                .is_none_or(|policy| policy.matches_activity(activity_id))
    }

    pub fn matches_owner(&self, activity_id: &str, owner_uid: u32) -> bool {
        self.matches_activity(activity_id)
            && self
                .capability_policy
                .as_ref()
                .is_none_or(|policy| policy.owner_uid == owner_uid)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCapabilityPolicyQuery {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCapabilityPolicySetRequest {
    #[serde(deserialize_with = "Option::deserialize")]
    pub expected_revision: Option<u64>,
    pub policy: CapabilityPolicyDraft,
}

impl ActivityCapabilityPolicySetRequest {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        validate_revision(self.expected_revision)?;
        self.policy.validate_shape()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCapabilityPolicyEnabledRequest {
    pub expected_revision: u64,
    pub enabled: bool,
}

impl ActivityCapabilityPolicyEnabledRequest {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        validate_revision(Some(self.expected_revision))
    }
}

fn validate_rules(rules: &[CapabilityPolicyRule]) -> Result<(), &'static str> {
    if rules.len() > MAX_CAPABILITY_POLICY_RULES {
        return Err("A capability policy may contain at most 64 rules");
    }
    let mut verbs = HashSet::new();
    for rule in rules {
        if rule.verb.trim().is_empty() {
            return Err("Each capability rule requires a catalogue verb");
        }
        if !verbs.insert(rule.verb.trim()) {
            return Err("A capability policy may contain only one rule per verb");
        }
        if rule.mode == CapabilityPolicyMode::Deny {
            if !rule.scopes.is_empty() {
                return Err("Deny rules block the whole verb and must have no scopes");
            }
        } else if !(1..=MAX_CAPABILITY_POLICY_SCOPES).contains(&rule.scopes.len()) {
            return Err("Normal and approval rules require between 1 and 32 compatible scopes");
        }
        if rule.scopes.iter().any(|scope| {
            scope
                .value()
                .is_some_and(|value| value.is_empty() || value.chars().any(char::is_control))
        }) {
            return Err("Scope values must be nonempty and contain no control characters");
        }
    }
    #[derive(Serialize)]
    struct Draft<'a> {
        rules: &'a [CapabilityPolicyRule],
    }
    let bytes = serde_json::to_vec(&Draft { rules })
        .map_err(|_| "Could not encode capability policy draft")?;
    if bytes.len() > MAX_CAPABILITY_POLICY_BYTES {
        return Err("The complete capability policy draft must be at most 16 KiB of JSON");
    }
    Ok(())
}

// Storage lowercases ASCII host text, sorts rules/scopes and removes duplicate
// scopes. Paths, names, self-references and verb spelling are not normalized.
fn canonical_rules(rules: &[CapabilityPolicyRule]) -> Vec<CapabilityPolicyRule> {
    let mut ordered = rules.to_vec();
    for rule in &mut ordered {
        for scope in &mut rule.scopes {
            if let CapabilityPolicyScope::Host { value } = scope {
                value.make_ascii_lowercase();
            }
        }
        rule.scopes.sort();
        rule.scopes.dedup();
    }
    ordered.sort();
    ordered
}

fn next_revision(revision: Option<u64>) -> Option<u64> {
    revision.unwrap_or(0).checked_add(1)
}

fn validate_revision(revision: Option<u64>) -> Result<(), &'static str> {
    if revision == Some(0) || next_revision(revision).is_none() {
        return Err("Expected revision must be positive and incrementable, or null for creation");
    }
    Ok(())
}

fn same_activity(left: &str, right: &str) -> bool {
    match (Uuid::parse_str(left), Uuid::parse_str(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn same_instant(left: &str, right: &str) -> bool {
    match (
        DateTime::parse_from_rfc3339(left),
        DateTime::parse_from_rfc3339(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/capability_policy.rs"
    ));
}
