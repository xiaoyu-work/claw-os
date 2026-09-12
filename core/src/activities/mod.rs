//! Persistent user goals shared by headless and graphical presentations.
//!
//! The daemon owns persistence and supplies the authenticated owner UID to
//! [`ActivityService`]. Activities grant no authority and never infer goal
//! completion from an execution result.

mod execution_limits;
mod object_state;
mod receipts;
mod sqlite;

use serde::{Deserialize, Serialize};

pub use execution_limits::{
    ActivityExecutionLimits, ExecutionBlockedReason, ExecutionLimitsDraft, ExecutionReservation,
};
pub use object_state::{
    ObjectRelationKind, ObjectStateContent, ObjectStateDraft, ObjectStateEntry, ObjectStateSource,
    ObjectStateValidity,
};
pub use receipts::{
    ActivityReceipt, ReceiptDeclaration, ReceiptEffect, ReceiptOutcome, ReceiptReport,
    ReceiptSource, ResultKind, ResultSummary,
};
pub use sqlite::SqliteActivityService;

/// Public Activity wire-compatibility version; the broker versions responses independently.
pub const SCHEMA_VERSION: u32 = 1;
/// SQLite user_version; never emitted as an Activity response schema.
pub const DATABASE_SCHEMA_VERSION: u32 = 4;
pub const DEFAULT_LIST_LIMIT: usize = 50;
pub const MAX_LIST_LIMIT: usize = 100;

const MAX_TITLE_BYTES: usize = 240;
const MAX_GOAL_BYTES: usize = 16 * 1024;
const MAX_PLANNING_BYTES: usize = 8 * 1024;
const MAX_COMPLETION_NOTE_BYTES: usize = 8 * 1024;
const MAX_RESOURCES: usize = 32;
const MAX_RESOURCE_LABEL_BYTES: usize = 240;
const MAX_RESOURCE_REFERENCE_BYTES: usize = 4096;
const MAX_ACTIVITIES_PER_OWNER: i64 = 1000;

#[derive(Debug, thiserror::Error)]
pub enum ActivityError {
    #[error("invalid activity: {0}")]
    Invalid(String),
    #[error("activity not found")]
    NotFound,
    #[error("activity conflict: {0}")]
    Conflict(String),
    #[error("activity, receipt, or object-state limit reached")]
    LimitReached,
    #[error("activity execution blocked: {0}")]
    ExecutionBlocked(ExecutionBlockedReason),
    #[error("activity database is unavailable: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("activity storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("activity serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("activity database lock is poisoned")]
    Poisoned,
    #[error("unsupported activity schema version {found}; supported version is {supported}")]
    SchemaVersion { found: i64, supported: u32 },
    #[error("activity database is corrupt: {0}")]
    Corrupt(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    Active,
    Paused,
    Completed,
    Cancelled,
}

impl ActivityState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ActivityError> {
        match value {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(ActivityError::Invalid(
                "state must be active, paused, completed, or cancelled".to_string(),
            )),
        }
    }

    pub fn allows_work(self) -> bool {
        self == Self::Active
    }

    fn allows_transition(self, next: Self) -> bool {
        match self {
            Self::Active | Self::Paused => self != next,
            Self::Completed | Self::Cancelled => next == Self::Active,
        }
    }
}

/// Display-only references; providers must never resolve or open them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityResource {
    pub label: String,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityDraft {
    pub title: String,
    pub goal: String,
    #[serde(default)]
    pub completion_criteria: String,
    #[serde(default)]
    pub boundaries: String,
    #[serde(default)]
    pub resources: Vec<ActivityResource>,
}

impl ActivityDraft {
    pub fn validate(&self) -> Result<(), ActivityError> {
        validate_planning(
            &self.title,
            &self.goal,
            &self.completion_criteria,
            &self.boundaries,
            &self.resources,
        )
    }

    fn normalized(self) -> Result<Self, ActivityError> {
        self.validate()?;
        Ok(Self {
            title: self.title.trim().to_string(),
            goal: self.goal.trim().to_string(),
            completion_criteria: self.completion_criteria.trim().to_string(),
            boundaries: self.boundaries.trim().to_string(),
            resources: normalize_resources(self.resources),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_criteria: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundaries: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Vec<ActivityResource>>,
}

impl ActivityPatch {
    pub fn validate(&self) -> Result<(), ActivityError> {
        if self.title.is_none()
            && self.goal.is_none()
            && self.completion_criteria.is_none()
            && self.boundaries.is_none()
            && self.resources.is_none()
        {
            return Err(ActivityError::Invalid(
                "an update must supply at least one planning field".to_string(),
            ));
        }
        if let Some(title) = &self.title {
            validate_text("title", title, MAX_TITLE_BYTES, false, false)?;
        }
        if let Some(goal) = &self.goal {
            validate_text("goal", goal, MAX_GOAL_BYTES, false, true)?;
        }
        if let Some(criteria) = &self.completion_criteria {
            validate_text(
                "completion_criteria",
                criteria,
                MAX_PLANNING_BYTES,
                true,
                true,
            )?;
        }
        if let Some(boundaries) = &self.boundaries {
            validate_text("boundaries", boundaries, MAX_PLANNING_BYTES, true, true)?;
        }
        if let Some(resources) = &self.resources {
            validate_resources(resources)?;
        }
        Ok(())
    }

    fn apply(self, activity: &mut Activity) {
        if let Some(title) = self.title {
            activity.title = title.trim().to_string();
        }
        if let Some(goal) = self.goal {
            activity.goal = goal.trim().to_string();
        }
        if let Some(criteria) = self.completion_criteria {
            activity.completion_criteria = criteria.trim().to_string();
        }
        if let Some(boundaries) = self.boundaries {
            activity.boundaries = boundaries.trim().to_string();
        }
        if let Some(resources) = self.resources {
            activity.resources = normalize_resources(resources);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activity {
    pub id: String,
    pub owner_uid: u32,
    pub title: String,
    pub goal: String,
    pub completion_criteria: String,
    pub boundaries: String,
    pub resources: Vec<ActivityResource>,
    pub state: ActivityState,
    pub completion_note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub trait ActivityService: Send + Sync {
    fn create(&self, owner_uid: u32, draft: ActivityDraft) -> Result<Activity, ActivityError>;

    fn get(&self, owner_uid: u32, id: &str) -> Result<Activity, ActivityError>;

    /// Zero selects the default limit; larger limits are capped.
    fn list(
        &self,
        owner_uid: u32,
        state: Option<ActivityState>,
        limit: usize,
    ) -> Result<Vec<Activity>, ActivityError>;

    fn update(
        &self,
        owner_uid: u32,
        id: &str,
        patch: ActivityPatch,
    ) -> Result<Activity, ActivityError>;

    /// Atomically append a display reference, or update its label if already
    /// attached. This does not resolve the resource or grant access to it.
    fn add_resource(
        &self,
        owner_uid: u32,
        id: &str,
        resource: ActivityResource,
    ) -> Result<Activity, ActivityError>;

    /// Completion requires a user confirmation note. Other transitions reject
    /// notes, and reopening clears the previous completion confirmation.
    /// State gates future work; it never cancels an existing worker.
    fn transition(
        &self,
        owner_uid: u32,
        id: &str,
        state: ActivityState,
        completion_note: Option<String>,
    ) -> Result<Activity, ActivityError>;

    /// Append a caller report without executing work or changing Activity state.
    /// An exact report retry returns the original declaration and received time.
    fn record_receipt(
        &self,
        owner_uid: u32,
        activity_id: &str,
        report: ReceiptReport,
        declaration: Option<ReceiptDeclaration>,
        declaration_error: Option<String>,
    ) -> Result<ActivityReceipt, ActivityError>;

    /// Read immutable caller reports, including for paused or terminal Activities.
    fn receipts(
        &self,
        owner_uid: u32,
        activity_id: &str,
        limit: usize,
    ) -> Result<Vec<ActivityReceipt>, ActivityError>;

    /// Append caller-reported metadata for attached objects, without resolving
    /// App data, establishing authority, or changing Activity lifecycle state.
    fn record_object_state(
        &self,
        owner_uid: u32,
        activity_id: &str,
        draft: ObjectStateDraft,
    ) -> Result<ObjectStateEntry, ActivityError>;

    /// Read immutable history, including detached references and corrections.
    /// Validity describes only the caller's reported window, never freshness.
    fn object_state(
        &self,
        owner_uid: u32,
        activity_id: &str,
        reference: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ObjectStateEntry>, ActivityError>;

    /// Read optional execution constraints and durable attempt accounting.
    /// Absence preserves legacy behavior; limits never grant authority.
    fn execution_limits(
        &self,
        owner_uid: u32,
        activity_id: &str,
    ) -> Result<Option<ActivityExecutionLimits>, ActivityError>;

    /// Create only when absent, or update the exact expected policy revision.
    /// Updates preserve the attempt count and enabled state.
    fn set_execution_limits(
        &self,
        owner_uid: u32,
        activity_id: &str,
        expected_revision: Option<u64>,
        draft: ExecutionLimitsDraft,
    ) -> Result<ActivityExecutionLimits, ActivityError>;

    /// Explicitly enable or revoke constraints, advancing the policy revision.
    /// Disabling remains available in terminal Activities.
    fn set_execution_limits_enabled(
        &self,
        owner_uid: u32,
        activity_id: &str,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<ActivityExecutionLimits, ActivityError>;

    /// Durably charge a root-owned attempt before execution admission.
    /// A reservation is accounting, never authority or proof of execution.
    fn reserve_execution(
        &self,
        owner_uid: u32,
        activity_id: &str,
        attempt_id: &str,
        job_id: &str,
        requested_max_turns: Option<u32>,
    ) -> Result<Option<ExecutionReservation>, ActivityError>;
}

/// Daemon composition only. Direct clients use owner-scoped broker routes.
pub fn open_default() -> Result<SqliteActivityService, ActivityError> {
    SqliteActivityService::open(crate::paths::data_dir().join("activities.db"))
}

/// Validate a UUID without opening storage. Persist the canonical [`Activity::id`]
/// returned by a lookup when associating other records with an Activity.
pub fn validate_id(value: &str) -> Result<(), ActivityError> {
    parse_id(value).map(|_| ())
}

fn parse_id(value: &str) -> Result<String, ActivityError> {
    uuid::Uuid::parse_str(value)
        .map(|id| id.to_string())
        .map_err(|_| ActivityError::Invalid("id must be a UUID".to_string()))
}

fn validate_planning(
    title: &str,
    goal: &str,
    criteria: &str,
    boundaries: &str,
    resources: &[ActivityResource],
) -> Result<(), ActivityError> {
    validate_text("title", title, MAX_TITLE_BYTES, false, false)?;
    validate_text("goal", goal, MAX_GOAL_BYTES, false, true)?;
    validate_text(
        "completion_criteria",
        criteria,
        MAX_PLANNING_BYTES,
        true,
        true,
    )?;
    validate_text("boundaries", boundaries, MAX_PLANNING_BYTES, true, true)?;
    validate_resources(resources)
}

fn validate_resources(resources: &[ActivityResource]) -> Result<(), ActivityError> {
    if resources.len() > MAX_RESOURCES {
        return Err(ActivityError::Invalid(format!(
            "resources exceeds the maximum of {MAX_RESOURCES}"
        )));
    }
    for resource in resources {
        validate_text(
            "resource label",
            &resource.label,
            MAX_RESOURCE_LABEL_BYTES,
            false,
            false,
        )?;
        validate_text(
            "resource reference",
            &resource.reference,
            MAX_RESOURCE_REFERENCE_BYTES,
            false,
            false,
        )?;
    }
    Ok(())
}

fn validate_text(
    field: &str,
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
    multiline: bool,
) -> Result<(), ActivityError> {
    let trimmed = value.trim();
    if (!allow_empty && trimmed.is_empty()) || trimmed.len() > max_bytes {
        let minimum = usize::from(!allow_empty);
        return Err(ActivityError::Invalid(format!(
            "{field} must contain {minimum}..={max_bytes} UTF-8 bytes after trimming"
        )));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() && !(multiline && matches!(ch, '\n' | '\r' | '\t')))
    {
        return Err(ActivityError::Invalid(format!(
            "{field} contains unsupported control characters"
        )));
    }
    Ok(())
}

fn normalize_resources(resources: Vec<ActivityResource>) -> Vec<ActivityResource> {
    resources.into_iter().map(normalize_resource).collect()
}

fn normalize_resource(resource: ActivityResource) -> ActivityResource {
    ActivityResource {
        label: resource.label.trim().to_string(),
        reference: resource.reference.trim().to_string(),
    }
}

fn normalize_completion_note(
    state: ActivityState,
    note: Option<String>,
) -> Result<Option<String>, ActivityError> {
    match (state, note) {
        (ActivityState::Completed, Some(note)) => {
            validate_text(
                "completion_note",
                &note,
                MAX_COMPLETION_NOTE_BYTES,
                false,
                true,
            )?;
            Ok(Some(note.trim().to_string()))
        }
        (ActivityState::Completed, None) => Err(ActivityError::Invalid(
            "completion requires a user confirmation note".to_string(),
        )),
        (_, Some(_)) => Err(ActivityError::Invalid(
            "completion_note is only valid when completing an activity".to_string(),
        )),
        (_, None) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/mod.rs"
    ));
}
