//! Durable, caller-reported work handoffs. Neither progress nor enablement is authority.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{parse_id, validate_text, ActivityError};

pub const MAX_WORK_HISTORY: usize = 1000;
pub const MAX_WORK_CHECKPOINT_BYTES: usize = 32 * 1024;
pub const MAX_STAGNANT_WORK_SEGMENTS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityWorkEvent {
    pub source: String,
    pub event_type: String,
}

impl ActivityWorkEvent {
    pub fn validate(&self) -> Result<(), ActivityError> {
        for (name, value) in [
            ("event source", &self.source),
            ("event type", &self.event_type),
        ] {
            validate_text(name, value, 128, false, false)?;
            if value.trim() != value {
                return Err(ActivityError::Invalid(format!("{name} must be trimmed")));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityWorkDraft {
    pub task: String,
    pub check_interval_secs: u32,
    pub wake_event: Option<ActivityWorkEvent>,
}

impl ActivityWorkDraft {
    pub fn validate(&self) -> Result<(), ActivityError> {
        validate_text("work task", &self.task, 8192, false, true)?;
        if !(60..=86400).contains(&self.check_interval_secs) {
            return Err(ActivityError::Invalid(
                "work check interval must be between 60 and 86400 seconds".into(),
            ));
        }
        if let Some(event) = &self.wake_event {
            event.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActivityWorkNext {
    Continue { task: String },
    Wait { task: String, reason: String },
    Review { reason: String },
    Blocked { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityWorkCheckpoint {
    pub summary: String,
    pub made_progress: bool,
    pub completed_steps: Vec<String>,
    pub artifacts: Vec<String>,
    pub next: ActivityWorkNext,
}

impl ActivityWorkCheckpoint {
    pub fn validate(&self) -> Result<(), ActivityError> {
        validate_text("work summary", &self.summary, 8192, false, true)?;
        if self.completed_steps.len() > 32 || self.artifacts.len() > 32 {
            return Err(ActivityError::Invalid(
                "a work handoff allows at most 32 completed steps and 32 artifact references"
                    .into(),
            ));
        }
        for step in &self.completed_steps {
            validate_text("completed step", step, 512, false, false)?;
        }
        let mut references = std::collections::BTreeSet::new();
        for reference in &self.artifacts {
            let parsed = crate::objects::parse_reference(reference)
                .map_err(|error| ActivityError::Invalid(error.to_string()))?;
            if crate::objects::format_reference(&parsed)
                .map_err(|error| ActivityError::Invalid(error.to_string()))?
                != *reference
                || !references.insert(reference)
            {
                return Err(ActivityError::Invalid(
                    "artifact references must be canonical and unique".into(),
                ));
            }
        }
        match &self.next {
            ActivityWorkNext::Continue { task } => {
                validate_text("next work task", task, 8192, false, true)?;
            }
            ActivityWorkNext::Wait { task, reason } => {
                validate_text("next work task", task, 8192, false, true)?;
                validate_text("wait reason", reason, 2048, false, true)?;
            }
            ActivityWorkNext::Review { reason } | ActivityWorkNext::Blocked { reason } => {
                validate_text("work stop reason", reason, 2048, false, true)?;
            }
        }
        if serde_json::to_vec(self)?.len() > MAX_WORK_CHECKPOINT_BYTES {
            return Err(ActivityError::Invalid("work handoff exceeds 32 KiB".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityWorkPhase {
    Ready,
    Running,
    Waiting,
    Review,
    Held,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityWorkSegment {
    pub job_id: String,
    pub session_id: String,
    pub generation: String,
    pub prepared_at: String,
    pub checkpoint: Option<ActivityWorkCheckpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityWorkRecord {
    pub job_id: String,
    pub session_id: String,
    pub recorded_at: String,
    pub checkpoint: Option<ActivityWorkCheckpoint>,
    pub outcome: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityWork {
    pub activity_id: String,
    pub owner_uid: u32,
    pub revision: u64,
    pub config: ActivityWorkDraft,
    pub enabled: bool,
    pub generation: Option<String>,
    pub phase: ActivityWorkPhase,
    pub next_task: String,
    pub next_wake_at: Option<String>,
    pub event_after: u64,
    pub current: Option<ActivityWorkSegment>,
    pub last_checkpoint: Option<ActivityWorkRecord>,
    pub stagnant_segments: u32,
    pub message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ActivityWork {
    pub fn due_at(&self, now: DateTime<Utc>) -> Result<bool, ActivityError> {
        if !self.enabled || self.current.is_some() {
            return Ok(false);
        }
        if !matches!(
            self.phase,
            ActivityWorkPhase::Ready | ActivityWorkPhase::Waiting
        ) {
            return Ok(false);
        }
        self.next_wake_at
            .as_deref()
            .map(|value| work_time(value).map(|at| at <= now))
            .transpose()
            .map(|due| due.unwrap_or(self.phase == ActivityWorkPhase::Ready))
    }

    pub fn validate(&self) -> Result<(), ActivityError> {
        if parse_id(&self.activity_id)? != self.activity_id
            || self.revision == 0
            || self.revision > i64::MAX as u64
        {
            return Err(ActivityError::Invalid(
                "invalid work identity or revision".into(),
            ));
        }
        self.config.validate()?;
        validate_text("next work task", &self.next_task, 8192, false, true)?;
        work_time(&self.created_at)?;
        if work_time(&self.updated_at)? < work_time(&self.created_at)? {
            return Err(ActivityError::Invalid(
                "work timestamps are reversed".into(),
            ));
        }
        if let Some(at) = &self.next_wake_at {
            work_time(at)?;
        }
        if let Some(generation) = &self.generation {
            if parse_id(generation)? != *generation {
                return Err(ActivityError::Invalid("invalid work generation".into()));
            }
        }
        if self.enabled && self.generation.is_none() {
            return Err(ActivityError::Invalid(
                "enabled work has no scheduler binding".into(),
            ));
        }
        if (self.phase == ActivityWorkPhase::Running) != self.current.is_some() {
            return Err(ActivityError::Invalid(
                "work phase and current Job disagree".into(),
            ));
        }
        if let Some(segment) = &self.current {
            super::execution_limits::validate_job_id(&segment.job_id)?;
            validate_text("work session", &segment.session_id, 128, false, false)?;
            if self.generation.as_deref() != Some(segment.generation.as_str()) {
                return Err(ActivityError::Invalid(
                    "work segment generation differs".into(),
                ));
            }
            work_time(&segment.prepared_at)?;
            if let Some(checkpoint) = &segment.checkpoint {
                checkpoint.validate()?;
            }
        }
        if let Some(record) = &self.last_checkpoint {
            record.validate()?;
        }
        if let Some(message) = &self.message {
            validate_text("work message", message, 2048, false, true)?;
        }
        if self.stagnant_segments > MAX_STAGNANT_WORK_SEGMENTS {
            return Err(ActivityError::Invalid(
                "invalid work stagnation counter".into(),
            ));
        }
        Ok(())
    }
}

impl ActivityWorkRecord {
    pub fn validate(&self) -> Result<(), ActivityError> {
        super::execution_limits::validate_job_id(&self.job_id)?;
        validate_text("work session", &self.session_id, 128, false, false)?;
        work_time(&self.recorded_at)?;
        if !matches!(
            self.outcome.as_str(),
            "reported" | "failed" | "indeterminate" | "cancelled"
        ) {
            return Err(ActivityError::Invalid(
                "invalid work segment outcome".into(),
            ));
        }
        if let Some(checkpoint) = &self.checkpoint {
            checkpoint.validate()?;
        }
        if let Some(message) = &self.message {
            validate_text("work message", message, 2048, false, true)?;
        }
        Ok(())
    }
}

/// Trusted consumer operations, deliberately not a wire-deserializable enum.
#[derive(Debug, Clone)]
pub enum ActivityWorkChange {
    Configure(ActivityWorkDraft),
    Enable {
        generation: String,
        next_task: Option<String>,
        review_note: Option<String>,
    },
    Disable,
    Reserve {
        generation: String,
        job_id: String,
        session_id: String,
    },
    Stage {
        job_id: String,
        checkpoint: ActivityWorkCheckpoint,
    },
    Finish {
        job_id: String,
        outcome: ActivityWorkOutcome,
        event_after: u64,
    },
    Wake {
        generation: String,
        event_index: u64,
    },
    Hold(String),
}

#[derive(Debug, Clone)]
pub enum ActivityWorkOutcome {
    Reported,
    Failed(String),
    Indeterminate(String),
    Cancelled,
}

pub(crate) fn work_time(value: &str) -> Result<DateTime<Utc>, ActivityError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| ActivityError::Invalid("work timestamp must be RFC3339".into()))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/work.rs"
    ));
}
