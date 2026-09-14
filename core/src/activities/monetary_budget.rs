//! Owner-defined USD accounting policy for Activity model turns.
//!
//! Amounts are integer micro-USD. Rates are policy inputs, not provider
//! prices, and accounting is a conservative upper bound rather than an invoice.

use serde::{Deserialize, Serialize};

use super::ActivityError;

pub const CURRENCY_USD: &str = "USD";
pub const MAX_TOTAL_MICROUSD: u64 = 1_000_000_000_000;
pub const MAX_RATE_MICROUSD_PER_MILLION_TOKENS: u64 = 1_000_000_000_000;
pub const MAX_OUTPUT_TOKENS_PER_TURN: u32 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryBudgetDraft {
    pub currency: String,
    pub max_total_microusd: u64,
    pub input_microusd_per_million_tokens: u64,
    pub output_microusd_per_million_tokens: u64,
    pub max_output_tokens_per_turn: u32,
}

impl MonetaryBudgetDraft {
    pub fn validate(&self) -> Result<(), ActivityError> {
        if self.currency != CURRENCY_USD {
            return Err(ActivityError::Invalid(
                "monetary budget currency must be USD".into(),
            ));
        }
        if !(1..=MAX_TOTAL_MICROUSD).contains(&self.max_total_microusd) {
            return Err(ActivityError::Invalid(format!(
                "max_total_microusd must be between 1 and {MAX_TOTAL_MICROUSD}"
            )));
        }
        for (field, value) in [
            (
                "input_microusd_per_million_tokens",
                self.input_microusd_per_million_tokens,
            ),
            (
                "output_microusd_per_million_tokens",
                self.output_microusd_per_million_tokens,
            ),
        ] {
            if !(1..=MAX_RATE_MICROUSD_PER_MILLION_TOKENS).contains(&value) {
                return Err(ActivityError::Invalid(format!(
                    "{field} must be between 1 and {MAX_RATE_MICROUSD_PER_MILLION_TOKENS}"
                )));
            }
        }
        if !(1..=MAX_OUTPUT_TOKENS_PER_TURN).contains(&self.max_output_tokens_per_turn) {
            return Err(ActivityError::Invalid(format!(
                "max_output_tokens_per_turn must be between 1 and {MAX_OUTPUT_TOKENS_PER_TURN}"
            )));
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryReservationRequest {
    pub call_id: String,
    pub job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub turn_index: u32,
    pub input_upper_bound_tokens: u64,
    pub requested_max_output_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryReservation {
    pub call_id: String,
    pub activity_id: String,
    pub owner_uid: u32,
    pub job_id: String,
    pub session_id: Option<String>,
    pub turn_index: u32,
    pub policy_revision: u64,
    pub reserved_microusd: u64,
    pub input_upper_bound_tokens: u64,
    pub requested_max_output_tokens: u32,
    pub policy_max_output_tokens_per_turn: u32,
    pub max_output_tokens: u32,
    pub input_microusd_per_million_tokens: u64,
    pub output_microusd_per_million_tokens: u64,
    pub reserved_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonetarySettlement {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_read_tokens: Option<u32>,
    pub cache_write_tokens: Option<u32>,
    pub provider: String,
    pub model: String,
    pub conservative: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum MonetaryBlockedReason {
    #[error("Activity is not active for monetary budget use")]
    Inactive,
    #[error("monetary budget is disabled")]
    Disabled,
    #[error("monetary budget reservation is stale")]
    Stale,
    #[error("monetary budget is exhausted")]
    Exhausted,
}

pub fn accounted_cost(
    tokens: u64,
    microusd_per_million_tokens: u64,
) -> Result<u64, ActivityError> {
    let numerator = u128::from(tokens)
        .checked_mul(u128::from(microusd_per_million_tokens))
        .ok_or_else(|| ActivityError::Invalid("monetary accounting overflow".into()))?;
    let rounded = numerator
        .checked_add(999_999)
        .ok_or_else(|| ActivityError::Invalid("monetary accounting overflow".into()))?
        / 1_000_000;
    u64::try_from(rounded)
        .map_err(|_| ActivityError::Invalid("monetary accounting is not representable".into()))
}

pub(super) fn validate_runtime_identity(
    request: &MonetaryReservationRequest,
) -> Result<(), ActivityError> {
    uuid::Uuid::parse_str(&request.call_id)
        .map_err(|_| ActivityError::Invalid("monetary call_id must be a UUID".into()))?;
    super::execution_limits::validate_job_id(&request.job_id)?;
    if let Some(session_id) = &request.session_id {
        if session_id.is_empty() || session_id.len() > 128 || session_id.chars().any(char::is_control)
        {
            return Err(ActivityError::Invalid(
                "monetary session_id must contain 1..=128 non-control characters".into(),
            ));
        }
    }
    if request.requested_max_output_tokens == 0 {
        return Err(ActivityError::Invalid(
            "requested_max_output_tokens must be positive".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/monetary_budget.rs"
    ));
}
