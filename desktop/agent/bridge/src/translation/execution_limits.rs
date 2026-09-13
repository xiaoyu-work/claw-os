//! Closed presentation DTOs; owner identity is supplied by the bridge, not HTTP input.

use cos_agent_protocol::{ActivityExecutionLimits, ActivityExecutionLimitsResponse};
use serde_json::Value;

pub fn get(
    value: Value,
    activity_id: &str,
    owner_uid: u32,
) -> Result<ActivityExecutionLimitsResponse, String> {
    let response: ActivityExecutionLimitsResponse = serde_json::from_value(value)
        .map_err(|error| format!("invalid activity.execution_limits.get result: {error}"))?;
    if !response.matches_owner(activity_id, owner_uid) {
        return Err(
            "Execution limits returned an invalid schema, owner, Activity or limits".into(),
        );
    }
    Ok(response)
}

pub fn policy(
    value: Value,
    activity_id: &str,
    owner_uid: u32,
) -> Result<ActivityExecutionLimits, String> {
    let limits: ActivityExecutionLimits = serde_json::from_value(value)
        .map_err(|error| format!("invalid Activity execution limits result: {error}"))?;
    if !limits.matches_owner(activity_id, owner_uid) {
        return Err(
            "Execution limits acknowledgement returned an invalid owner, Activity or limits".into(),
        );
    }
    Ok(limits)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/translation/execution_limits.rs"
    ));
}
