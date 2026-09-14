//! Owner-selected admission priority for queued Activity work.
//!
//! Scheduling metadata changes only pending Job ordering. It is not authority,
//! consent, budget, completion, execution evidence, or permission to preempt.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivitySchedulingPriority {
    Foreground,
    Standard,
    Background,
}

impl ActivitySchedulingPriority {
    pub(crate) fn admission_rank(self) -> u8 {
        match self {
            Self::Foreground => 0,
            Self::Standard => 1,
            Self::Background => 2,
        }
    }
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

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/scheduling_policy.rs"
    ));
}
