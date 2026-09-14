//! Owner-selected pending admission metadata, not authority or provider QoS.

use chrono::DateTime;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivitySchedulingPriority {
    Foreground,
    Standard,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivitySchedulingPolicy {
    pub activity_id: String,
    pub owner_uid: u32,
    pub revision: u64,
    pub priority: ActivitySchedulingPriority,
    pub created_at: String,
    pub updated_at: String,
}

impl ActivitySchedulingPolicy {
    pub fn matches_activity(&self, activity_id: &str) -> bool {
        same_activity(&self.activity_id, activity_id)
            && self.revision > 0
            && DateTime::parse_from_rfc3339(&self.created_at).is_ok()
            && DateTime::parse_from_rfc3339(&self.updated_at).is_ok()
    }

    pub fn matches_owner(&self, activity_id: &str, owner_uid: u32) -> bool {
        self.matches_activity(activity_id) && self.owner_uid == owner_uid
    }

    pub fn matches_set(
        &self,
        activity_id: &str,
        request: &ActivitySchedulingPrioritySetRequest,
    ) -> bool {
        self.matches_activity(activity_id)
            && request.validate_shape().is_ok()
            && next_revision(request.expected_revision) == Some(self.revision)
            && self.priority == request.priority
    }

    pub fn preserves_identity(&self, previous: &Self) -> bool {
        self.matches_owner(&previous.activity_id, previous.owner_uid)
            && same_instant(&self.created_at, &previous.created_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivitySchedulingPriorityResponse {
    pub schema: u32,
    pub activity_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub scheduling_policy: Option<ActivitySchedulingPolicy>,
}

impl ActivitySchedulingPriorityResponse {
    pub fn matches_activity(&self, activity_id: &str) -> bool {
        self.schema == 1
            && same_activity(&self.activity_id, activity_id)
            && self
                .scheduling_policy
                .as_ref()
                .is_none_or(|policy| policy.matches_activity(activity_id))
    }

    pub fn matches_owner(&self, activity_id: &str, owner_uid: u32) -> bool {
        self.matches_activity(activity_id)
            && self
                .scheduling_policy
                .as_ref()
                .is_none_or(|policy| policy.owner_uid == owner_uid)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivitySchedulingPriorityQuery {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivitySchedulingPrioritySetRequest {
    #[serde(deserialize_with = "required_nullable")]
    pub expected_revision: Option<u64>,
    pub priority: ActivitySchedulingPriority,
}

impl ActivitySchedulingPrioritySetRequest {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        if self.expected_revision == Some(0) || next_revision(self.expected_revision).is_none() {
            return Err(
                "Expected revision must be positive and incrementable, or null for creation",
            );
        }
        Ok(())
    }
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
        "/test/unit/scheduling_priority.rs"
    ));
}
