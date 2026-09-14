//! Owner-configured model-turn accounting, not provider billing or authority.

use chrono::DateTime;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

pub const MAX_TOTAL_MICROUSD: u64 = 1_000_000_000_000;
pub const MAX_RATE_MICROUSD_PER_MILLION_TOKENS: u64 = 1_000_000_000_000;
pub const MAX_OUTPUT_TOKENS_PER_TURN: u32 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonetaryCurrency {
    #[serde(rename = "USD")]
    Usd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryBudgetDraft {
    pub currency: MonetaryCurrency,
    pub max_total_microusd: u64,
    pub input_microusd_per_million_tokens: u64,
    pub output_microusd_per_million_tokens: u64,
    pub max_output_tokens_per_turn: u32,
}

impl MonetaryBudgetDraft {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        if !(1..=MAX_TOTAL_MICROUSD).contains(&self.max_total_microusd) {
            return Err("Maximum configured total must be between 1 and 1000000000000 micro-USD");
        }
        if !(1..=MAX_RATE_MICROUSD_PER_MILLION_TOKENS)
            .contains(&self.input_microusd_per_million_tokens)
            || !(1..=MAX_RATE_MICROUSD_PER_MILLION_TOKENS)
                .contains(&self.output_microusd_per_million_tokens)
        {
            return Err(
                "Configured accounting rates must be between 1 and 1000000000000 micro-USD per million tokens",
            );
        }
        if !(1..=MAX_OUTPUT_TOKENS_PER_TURN).contains(&self.max_output_tokens_per_turn) {
            return Err("Maximum output tokens per turn must be between 1 and 1000000");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityMonetaryBudget {
    pub activity_id: String,
    pub owner_uid: u32,
    pub revision: u64,
    pub enabled: bool,
    pub spent_microusd: u64,
    pub reserved_microusd: u64,
    pub budget: MonetaryBudgetDraft,
    pub created_at: String,
    pub updated_at: String,
}

impl ActivityMonetaryBudget {
    pub fn matches_activity(&self, activity_id: &str) -> bool {
        same_activity(&self.activity_id, activity_id)
            && self.revision > 0
            && self.budget.validate_shape().is_ok()
            && DateTime::parse_from_rfc3339(&self.created_at).is_ok()
            && DateTime::parse_from_rfc3339(&self.updated_at).is_ok()
    }

    pub fn matches_owner(&self, activity_id: &str, owner_uid: u32) -> bool {
        self.matches_activity(activity_id) && self.owner_uid == owner_uid
    }

    pub fn matches_set(
        &self,
        activity_id: &str,
        request: &ActivityMonetaryBudgetSetRequest,
    ) -> bool {
        self.matches_activity(activity_id)
            && request.validate_shape().is_ok()
            && next_revision(request.expected_revision) == Some(self.revision)
            && self.budget == request.budget
            && (request.expected_revision.is_some() || self.enabled)
    }

    pub fn matches_enabled(
        &self,
        activity_id: &str,
        request: &ActivityMonetaryBudgetEnabledRequest,
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

    pub fn remaining_microusd(&self) -> u64 {
        self.budget
            .max_total_microusd
            .saturating_sub(self.spent_microusd.saturating_add(self.reserved_microusd))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityMonetaryBudgetResponse {
    pub schema: u32,
    pub activity_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub monetary_budget: Option<ActivityMonetaryBudget>,
}

impl ActivityMonetaryBudgetResponse {
    pub fn matches_activity(&self, activity_id: &str) -> bool {
        self.schema == 1
            && same_activity(&self.activity_id, activity_id)
            && self
                .monetary_budget
                .as_ref()
                .is_none_or(|budget| budget.matches_activity(activity_id))
    }

    pub fn matches_owner(&self, activity_id: &str, owner_uid: u32) -> bool {
        self.matches_activity(activity_id)
            && self
                .monetary_budget
                .as_ref()
                .is_none_or(|budget| budget.owner_uid == owner_uid)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityMonetaryBudgetQuery {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityMonetaryBudgetSetRequest {
    #[serde(deserialize_with = "required_nullable")]
    pub expected_revision: Option<u64>,
    pub budget: MonetaryBudgetDraft,
}

impl ActivityMonetaryBudgetSetRequest {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        validate_revision(self.expected_revision)?;
        self.budget.validate_shape()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityMonetaryBudgetEnabledRequest {
    pub expected_revision: u64,
    pub enabled: bool,
}

impl ActivityMonetaryBudgetEnabledRequest {
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
        "/test/unit/monetary_budget.rs"
    ));
}
