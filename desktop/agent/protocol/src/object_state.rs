//! Bounded caller-reported Activity annotations, never object truth or authority.

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ActivityReceiptReport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStateSource {
    CallerReported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStateRelation {
    RelatedTo,
    DependsOn,
    DerivedFrom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStateValidity {
    Unknown,
    NotYetApplicable,
    WithinReportedWindow,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectStateContent {
    UserStatement {
        text: String,
    },
    AgentInference {
        text: String,
    },
    AppReport {
        receipt_id: String,
    },
    Relation {
        relation: ObjectStateRelation,
        target: String,
        note: String,
    },
    Retracted {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectStateDraft {
    pub id: String,
    pub reference: String,
    pub content: ObjectStateContent,
    #[serde(default)]
    pub observed_at: Option<String>,
    #[serde(default)]
    pub valid_until: Option<String>,
    #[serde(default)]
    pub supersedes: Option<String>,
}

impl ObjectStateDraft {
    /// Structural bounds only. The broker owns UUID/RFC3339 parsing, time
    /// ordering, canonical references, attachment/receipt binding and conflicts.
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        if self.id.trim().is_empty() || self.reference.trim().is_empty() {
            return Err("Object state requires an entry id and an attached App reference");
        }
        if self
            .supersedes
            .as_deref()
            .is_some_and(|id| id.trim().is_empty() || id == self.id)
        {
            return Err("A correction must name a different, nonempty predecessor id");
        }
        match (&self.observed_at, &self.valid_until) {
            (None, None) => {}
            (Some(start), Some(end)) if !start.trim().is_empty() && !end.trim().is_empty() => {}
            _ => return Err("A reported window requires both start and end, or neither"),
        }
        if matches!(
            self.content,
            ObjectStateContent::Relation { .. } | ObjectStateContent::Retracted { .. }
        ) && self.observed_at.is_some()
        {
            return Err("Planning relations and retractions cannot have a reported time window");
        }
        match &self.content {
            ObjectStateContent::UserStatement { text }
            | ObjectStateContent::AgentInference { text } => {
                if text.trim().is_empty() || text.len() > 4096 {
                    return Err("Object state text must be nonempty and at most 4096 UTF-8 bytes");
                }
            }
            ObjectStateContent::AppReport { receipt_id } => {
                if receipt_id.trim().is_empty() {
                    return Err("An App report requires an existing receipt id");
                }
            }
            ObjectStateContent::Relation { target, note, .. } => {
                if target.trim().is_empty() {
                    return Err("A planning relation requires an attached target App reference");
                }
                if note.len() > 2048 {
                    return Err("A planning relation note must be at most 2048 UTF-8 bytes");
                }
            }
            ObjectStateContent::Retracted { reason } => {
                if self.supersedes.is_none() {
                    return Err("A retraction must name the entry it supersedes");
                }
                if reason.trim().is_empty() || reason.len() > 4096 {
                    return Err(
                        "A retraction reason must be nonempty and at most 4096 UTF-8 bytes",
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectStateEntry {
    pub id: String,
    pub activity_id: String,
    pub owner_uid: u32,
    pub recorded_at: String,
    pub source: ObjectStateSource,
    pub draft: ObjectStateDraft,
    #[serde(default)]
    pub receipt: Option<ActivityReceiptReport>,
    #[serde(default)]
    pub superseded_by: Option<String>,
    pub validity: ObjectStateValidity,
}

impl ObjectStateEntry {
    pub fn matches_activity(&self, id: &str) -> bool {
        !id.trim().is_empty()
            && self.activity_id == id
            && self.id == self.draft.id
            && !self.recorded_at.trim().is_empty()
            && self.draft.validate_shape().is_ok()
            && self
                .superseded_by
                .as_deref()
                .is_none_or(|next| !next.trim().is_empty() && next != self.id)
            && (self.validity == ObjectStateValidity::Unknown) == self.draft.observed_at.is_none()
            && match (&self.draft.content, &self.receipt) {
                (ObjectStateContent::AppReport { .. }, Some(receipt)) => {
                    !receipt.id.trim().is_empty()
                        && !receipt.app_id.trim().is_empty()
                        && !receipt.operation.trim().is_empty()
                }
                (ObjectStateContent::AppReport { .. }, None) | (_, Some(_)) => false,
                (_, None) => true,
            }
    }

    /// Compare the submitted intent after the broker's documented normalization,
    /// without interpreting App URIs or changing the retry payload.
    pub fn matches_submission(&self, activity_id: &str, draft: &ObjectStateDraft) -> bool {
        self.matches_activity(&self.activity_id)
            && same_uuid(&self.activity_id, activity_id)
            && same_uuid(&self.id, &draft.id)
            && self.draft.reference == draft.reference
            && same_optional_uuid(
                self.draft.supersedes.as_deref(),
                draft.supersedes.as_deref(),
            )
            && normalized_content_matches(&self.draft.content, &draft.content)
            && same_reported_window(&self.draft, draft)
    }
}

fn same_uuid(left: &str, right: &str) -> bool {
    match (Uuid::parse_str(left), Uuid::parse_str(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn same_optional_uuid(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => same_uuid(left, right),
        _ => false,
    }
}

fn normalized_content_matches(
    recorded: &ObjectStateContent,
    submitted: &ObjectStateContent,
) -> bool {
    match (recorded, submitted) {
        (
            ObjectStateContent::UserStatement { text: recorded },
            ObjectStateContent::UserStatement { text: submitted },
        )
        | (
            ObjectStateContent::AgentInference { text: recorded },
            ObjectStateContent::AgentInference { text: submitted },
        ) => recorded == submitted.trim(),
        (
            ObjectStateContent::AppReport {
                receipt_id: recorded,
            },
            ObjectStateContent::AppReport {
                receipt_id: submitted,
            },
        ) => same_uuid(recorded, submitted),
        (
            ObjectStateContent::Relation {
                relation: recorded_relation,
                target: recorded_target,
                note: recorded_note,
            },
            ObjectStateContent::Relation {
                relation: submitted_relation,
                target: submitted_target,
                note: submitted_note,
            },
        ) => {
            recorded_relation == submitted_relation
                && recorded_target == submitted_target
                && recorded_note == submitted_note.trim()
        }
        (
            ObjectStateContent::Retracted { reason: recorded },
            ObjectStateContent::Retracted { reason: submitted },
        ) => recorded == submitted.trim(),
        _ => false,
    }
}

fn same_reported_window(recorded: &ObjectStateDraft, submitted: &ObjectStateDraft) -> bool {
    match (
        recorded.observed_at.as_deref(),
        recorded.valid_until.as_deref(),
        submitted.observed_at.as_deref(),
        submitted.valid_until.as_deref(),
    ) {
        (None, None, None, None) => true,
        (Some(recorded_start), Some(recorded_end), Some(submitted_start), Some(submitted_end)) => {
            let (Ok(recorded_start), Ok(recorded_end), Ok(submitted_start), Ok(submitted_end)) = (
                DateTime::parse_from_rfc3339(recorded_start),
                DateTime::parse_from_rfc3339(recorded_end),
                DateTime::parse_from_rfc3339(submitted_start),
                DateTime::parse_from_rfc3339(submitted_end),
            ) else {
                return false;
            };
            recorded_start == submitted_start
                && recorded_end == submitted_end
                && recorded_end > recorded_start
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityObjectStateResponse {
    pub schema: u32,
    pub activity_id: String,
    pub entries: Vec<ObjectStateEntry>,
}

impl ActivityObjectStateResponse {
    pub fn matches_activity(&self, id: &str) -> bool {
        let mut ids = std::collections::HashSet::new();
        self.schema == 1
            && !id.trim().is_empty()
            && self.activity_id == id
            && self.entries.len() <= 100
            && self
                .entries
                .iter()
                .all(|entry| entry.matches_activity(id) && ids.insert(entry.id.as_str()))
    }

    pub fn matches_query(&self, id: &str, query: &ActivityObjectStateQuery) -> bool {
        self.matches_activity(id)
            && query.validate_shape().is_ok()
            && self.entries.len() <= query.limit.unwrap_or(50) as usize
            && query.reference.as_deref().is_none_or(|reference| {
                self.entries
                    .iter()
                    .all(|entry| entry.draft.reference == reference)
            })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityObjectStateQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl ActivityObjectStateQuery {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        if self.limit.is_some_and(|limit| !(1..=100).contains(&limit)) {
            return Err("Object state limit must be between 1 and 100");
        }
        if self
            .reference
            .as_deref()
            .is_some_and(|reference| reference.trim().is_empty())
        {
            return Err("An object state reference filter cannot be empty");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityObjectStateRecordRequest {
    pub entry: ObjectStateDraft,
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/object_state.rs"
    ));
}
