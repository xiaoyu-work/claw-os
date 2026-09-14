//! Closed scheduling-priority translation with authenticated owner checks.

use cos_agent_protocol::{ActivitySchedulingPolicy, ActivitySchedulingPriorityResponse};
use serde_json::Value;

pub fn get(
    value: Value,
    activity_id: &str,
    owner_uid: u32,
) -> Result<ActivitySchedulingPriorityResponse, String> {
    let response: ActivitySchedulingPriorityResponse = serde_json::from_value(value)
        .map_err(|error| format!("invalid activity.scheduling_policy.get result: {error}"))?;
    if !response.matches_owner(activity_id, owner_uid) {
        return Err(
            "Scheduling priority returned an invalid schema, owner, Activity, or policy".into(),
        );
    }
    Ok(response)
}

pub fn policy(
    value: Value,
    activity_id: &str,
    owner_uid: u32,
) -> Result<ActivitySchedulingPolicy, String> {
    let policy: ActivitySchedulingPolicy = serde_json::from_value(value)
        .map_err(|error| format!("invalid Activity scheduling-priority result: {error}"))?;
    if !policy.matches_owner(activity_id, owner_uid) {
        return Err(
            "Scheduling-priority acknowledgement returned an invalid owner, Activity, or policy"
                .into(),
        );
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/translation/scheduling_priority.rs"
    ));
}
