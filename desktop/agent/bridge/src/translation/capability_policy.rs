//! Closed capability-policy projections with bridge-owned identity checks.

use cos_agent_protocol::{ActivityCapabilityPolicy, ActivityCapabilityPolicyResponse};
use serde_json::Value;

pub fn get(
    value: Value,
    activity_id: &str,
    owner_uid: u32,
) -> Result<ActivityCapabilityPolicyResponse, String> {
    let response: ActivityCapabilityPolicyResponse = serde_json::from_value(value)
        .map_err(|error| format!("invalid activity.capability_policy.get result: {error}"))?;
    if !response.matches_owner(activity_id, owner_uid) {
        return Err(
            "Capability policy returned an invalid schema, owner, Activity or rules".into(),
        );
    }
    Ok(response)
}

pub fn policy(
    value: Value,
    activity_id: &str,
    owner_uid: u32,
) -> Result<ActivityCapabilityPolicy, String> {
    let policy: ActivityCapabilityPolicy = serde_json::from_value(value)
        .map_err(|error| format!("invalid Activity capability policy result: {error}"))?;
    if !policy.matches_owner(activity_id, owner_uid) {
        return Err(
            "Capability policy acknowledgement returned an invalid owner, Activity or rules".into(),
        );
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/translation/capability_policy.rs"
    ));
}
