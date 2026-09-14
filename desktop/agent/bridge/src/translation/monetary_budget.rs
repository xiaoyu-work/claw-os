//! Closed monetary-accounting DTO translation with authenticated owner checks.

use cos_agent_protocol::{ActivityMonetaryBudget, ActivityMonetaryBudgetResponse};
use serde_json::Value;

pub fn get(
    value: Value,
    activity_id: &str,
    owner_uid: u32,
) -> Result<ActivityMonetaryBudgetResponse, String> {
    let response: ActivityMonetaryBudgetResponse = serde_json::from_value(value)
        .map_err(|error| format!("invalid activity.monetary_budget.get result: {error}"))?;
    if !response.matches_owner(activity_id, owner_uid) {
        return Err(
            "Monetary budget returned an invalid schema, owner, Activity, or accounting policy"
                .into(),
        );
    }
    Ok(response)
}

pub fn policy(
    value: Value,
    activity_id: &str,
    owner_uid: u32,
) -> Result<ActivityMonetaryBudget, String> {
    let budget: ActivityMonetaryBudget = serde_json::from_value(value)
        .map_err(|error| format!("invalid Activity monetary-budget result: {error}"))?;
    if !budget.matches_owner(activity_id, owner_uid) {
        return Err(
            "Monetary-budget acknowledgement returned an invalid owner, Activity, or policy".into(),
        );
    }
    Ok(budget)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/translation/monetary_budget.rs"
    ));
}
