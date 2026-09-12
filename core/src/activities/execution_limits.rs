//! Finite Activity execution constraints and accounting, never grants or budgets
//! for capabilities, approvals, tokens, or cost.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use super::ActivityError;

pub(super) const MAX_ATTEMPTS: u32 = 1000;
pub(super) const MAX_TURNS_PER_ATTEMPT: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLimitsDraft {
    pub max_attempts: u32,
    pub max_turns_per_attempt: u32,
    pub expires_at: String,
}

impl ExecutionLimitsDraft {
    /// Validate shape and bounds without storage. The provider rechecks that
    /// expiry is in the future inside each configuration transaction.
    pub fn validate(&self) -> Result<(), ActivityError> {
        if !(1..=MAX_ATTEMPTS).contains(&self.max_attempts) {
            return Err(ActivityError::Invalid(
                "max_attempts must be between 1 and 1000".into(),
            ));
        }
        if !(1..=MAX_TURNS_PER_ATTEMPT).contains(&self.max_turns_per_attempt) {
            return Err(ActivityError::Invalid(
                "max_turns_per_attempt must be between 1 and 100".into(),
            ));
        }
        self.expiry().map(|_| ())
    }

    pub(super) fn canonicalized(mut self) -> Result<Self, ActivityError> {
        self.validate()?;
        self.expires_at = self.expiry()?.to_rfc3339_opts(SecondsFormat::Nanos, true);
        Ok(self)
    }

    pub(super) fn expiry(&self) -> Result<DateTime<Utc>, ActivityError> {
        DateTime::parse_from_rfc3339(&self.expires_at)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|_| {
                ActivityError::Invalid("execution expires_at must be an RFC3339 timestamp".into())
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionReservation {
    pub id: String,
    pub activity_id: String,
    pub owner_uid: u32,
    pub job_id: String,
    pub policy_revision: u64,
    pub max_turns: u32,
    pub expires_at: String,
    pub reserved_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBlockedReason {
    #[error("Activity is not active for this operation")]
    Inactive,
    #[error("execution limits are disabled")]
    Disabled,
    #[error("execution limits have expired")]
    Expired,
    #[error("execution policy revision changed")]
    StaleRevision,
    #[error("execution attempt limit reached")]
    AttemptLimit,
}

pub(super) fn validate_job_id(id: &str) -> Result<(), ActivityError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ActivityError::Invalid(
            "execution job_id must contain 1..=128 ASCII letters, digits, '-' or '_'".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_requested_turns(turns: Option<u32>) -> Result<(), ActivityError> {
    if turns == Some(0) {
        return Err(ActivityError::Invalid(
            "requested_max_turns must be positive when supplied".into(),
        ));
    }
    Ok(())
}

pub(super) fn revision_sql(revision: u64) -> Result<i64, ActivityError> {
    if revision == 0 {
        return Err(ActivityError::Invalid(
            "execution policy revision must be positive".into(),
        ));
    }
    i64::try_from(revision).map_err(|_| {
        ActivityError::Invalid("execution policy revision is not representable".into())
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/execution_limits.rs"
    ));
}
