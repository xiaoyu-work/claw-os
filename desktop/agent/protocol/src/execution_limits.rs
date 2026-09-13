//! Explicit execution constraints, never capabilities or approval grants.

use std::time::SystemTime;

use chrono::DateTime;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLimitsDraft {
    pub max_attempts: u32,
    pub max_turns_per_attempt: u32,
    pub expires_at: String,
}

impl ExecutionLimitsDraft {
    /// Future-expiry admission belongs to the backend transaction, not the UI clock.
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        if !(1..=1000).contains(&self.max_attempts) {
            return Err("Maximum attempts must be between 1 and 1000");
        }
        if !(1..=100).contains(&self.max_turns_per_attempt) {
            return Err("Maximum turns per attempt must be between 1 and 100");
        }
        if DateTime::parse_from_rfc3339(&self.expires_at).is_err() {
            return Err("Execution limits expiry must be an RFC3339 timestamp");
        }
        Ok(())
    }

    pub fn matches_normalized(&self, submitted: &Self) -> bool {
        self.max_attempts == submitted.max_attempts
            && self.max_turns_per_attempt == submitted.max_turns_per_attempt
            && same_instant(&self.expires_at, &submitted.expires_at)
    }

    pub fn is_expired_at(&self, now: SystemTime) -> Result<bool, &'static str> {
        let expiry = DateTime::parse_from_rfc3339(&self.expires_at)
            .map_err(|_| "Execution limits expiry must be an RFC3339 timestamp")?;
        Ok(SystemTime::from(expiry) <= now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityExecutionLimits {
    pub activity_id: String,
    pub owner_uid: u32,
    pub revision: u64,
    pub enabled: bool,
    pub limits: ExecutionLimitsDraft,
    pub used_attempts: u32,
    pub created_at: String,
    pub updated_at: String,
}

impl ActivityExecutionLimits {
    pub fn matches_activity(&self, activity_id: &str) -> bool {
        same_activity(&self.activity_id, activity_id)
            && self.revision > 0
            && self.limits.validate_shape().is_ok()
            && DateTime::parse_from_rfc3339(&self.created_at).is_ok()
            && DateTime::parse_from_rfc3339(&self.updated_at).is_ok()
    }

    pub fn matches_owner(&self, activity_id: &str, owner_uid: u32) -> bool {
        self.matches_activity(activity_id) && self.owner_uid == owner_uid
    }

    pub fn matches_set(
        &self,
        activity_id: &str,
        request: &ActivityExecutionLimitsSetRequest,
    ) -> bool {
        self.matches_activity(activity_id)
            && request.validate_shape().is_ok()
            && next_revision(request.expected_revision) == Some(self.revision)
            && self.limits.matches_normalized(&request.limits)
            && (request.expected_revision.is_some() || self.enabled)
    }

    pub fn matches_enabled(
        &self,
        activity_id: &str,
        request: &ActivityExecutionLimitsEnabledRequest,
    ) -> bool {
        self.matches_activity(activity_id)
            && request.validate_shape().is_ok()
            && next_revision(Some(request.expected_revision)) == Some(self.revision)
            && self.enabled == request.enabled
    }

    pub fn preserves_lifetime(&self, previous: &Self) -> bool {
        self.matches_owner(&previous.activity_id, previous.owner_uid)
            && self.used_attempts >= previous.used_attempts
            && same_instant(&self.created_at, &previous.created_at)
    }

    pub fn remaining_attempts(&self) -> u32 {
        self.limits.max_attempts.saturating_sub(self.used_attempts)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityExecutionLimitsResponse {
    pub schema: u32,
    pub activity_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub execution_limits: Option<ActivityExecutionLimits>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityExecutionLimitsQuery {}

impl ActivityExecutionLimitsResponse {
    pub fn matches_activity(&self, activity_id: &str) -> bool {
        self.schema == 1
            && same_activity(&self.activity_id, activity_id)
            && self
                .execution_limits
                .as_ref()
                .is_none_or(|limits| limits.matches_activity(activity_id))
    }

    pub fn matches_owner(&self, activity_id: &str, owner_uid: u32) -> bool {
        self.matches_activity(activity_id)
            && self
                .execution_limits
                .as_ref()
                .is_none_or(|limits| limits.owner_uid == owner_uid)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityExecutionLimitsSetRequest {
    #[serde(deserialize_with = "required_nullable")]
    pub expected_revision: Option<u64>,
    pub limits: ExecutionLimitsDraft,
}

impl ActivityExecutionLimitsSetRequest {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        validate_revision(self.expected_revision)?;
        self.limits.validate_shape()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityExecutionLimitsEnabledRequest {
    pub expected_revision: u64,
    pub enabled: bool,
}

impl ActivityExecutionLimitsEnabledRequest {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        validate_revision(Some(self.expected_revision))
    }
}

fn validate_revision(revision: Option<u64>) -> Result<(), &'static str> {
    if revision == Some(0) || next_revision(revision).is_none() {
        return Err("Expected revision must be positive and incrementable, or null for creation");
    }
    Ok(())
}

fn next_revision(revision: Option<u64>) -> Option<u64> {
    revision.unwrap_or(0).checked_add(1)
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

// Missing data must not be interpreted as "no limits" or initial configuration.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/execution_limits.rs"
    ));
}
