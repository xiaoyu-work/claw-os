//! Activity-local caller reports and planning links, not an App data store.
//! Content kinds classify statements; they do not authenticate their authors.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use super::{parse_id, validate_text, ActivityError, ReceiptReport};
use crate::objects::{format_reference, parse_reference};

const MAX_TEXT_BYTES: usize = 4096;
const MAX_RELATION_NOTE_BYTES: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectRelationKind {
    RelatedTo,
    DependsOn,
    DerivedFrom,
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
        relation: ObjectRelationKind,
        target: String,
        #[serde(default)]
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
    /// Validate untrusted input without opening storage or discovering Apps.
    pub fn validate(&self) -> Result<(), ActivityError> {
        let id = parse_id(&self.id)?;
        canonical_reference(&self.reference)?;
        if let Some(supersedes) = &self.supersedes {
            if parse_id(supersedes)? == id {
                return Err(ActivityError::Invalid(
                    "an object-state entry cannot supersede itself".into(),
                ));
            }
        }
        reported_window(self.observed_at.as_deref(), self.valid_until.as_deref())?;
        match &self.content {
            ObjectStateContent::UserStatement { text }
            | ObjectStateContent::AgentInference { text } => {
                validate_text("object-state text", text, MAX_TEXT_BYTES, false, true)?;
            }
            ObjectStateContent::AppReport { receipt_id } => {
                parse_id(receipt_id)?;
            }
            ObjectStateContent::Relation { target, note, .. } => {
                let target = canonical_reference(target)?;
                if target == self.reference {
                    return Err(ActivityError::Invalid(
                        "an object relation requires a different target reference".into(),
                    ));
                }
                validate_text(
                    "object relation note",
                    note,
                    MAX_RELATION_NOTE_BYTES,
                    true,
                    true,
                )?;
            }
            ObjectStateContent::Retracted { reason } => {
                validate_text(
                    "object-state retraction reason",
                    reason,
                    MAX_TEXT_BYTES,
                    false,
                    true,
                )?;
                if self.supersedes.is_none() {
                    return Err(ActivityError::Invalid(
                        "an object-state retraction requires supersedes".into(),
                    ));
                }
            }
        }
        if matches!(
            self.content,
            ObjectStateContent::Relation { .. } | ObjectStateContent::Retracted { .. }
        ) && self.observed_at.is_some()
        {
            return Err(ActivityError::Invalid(
                "relations and retractions cannot have reported validity windows".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn canonicalized(mut self) -> Result<Self, ActivityError> {
        self.validate()?;
        self.id = parse_id(&self.id)?;
        self.reference = canonical_reference(&self.reference)?;
        self.supersedes = self.supersedes.as_deref().map(parse_id).transpose()?;
        match &mut self.content {
            ObjectStateContent::UserStatement { text }
            | ObjectStateContent::AgentInference { text } => *text = text.trim().to_string(),
            ObjectStateContent::AppReport { receipt_id } => *receipt_id = parse_id(receipt_id)?,
            ObjectStateContent::Relation { target, note, .. } => {
                *target = canonical_reference(target)?;
                *note = note.trim().to_string();
            }
            ObjectStateContent::Retracted { reason } => *reason = reason.trim().to_string(),
        }
        if let Some((start, end)) =
            reported_window(self.observed_at.as_deref(), self.valid_until.as_deref())?
        {
            self.observed_at = Some(start.to_rfc3339_opts(SecondsFormat::Nanos, true));
            self.valid_until = Some(end.to_rfc3339_opts(SecondsFormat::Nanos, true));
        }
        Ok(self)
    }

    pub(super) fn validity_at(
        &self,
        now: DateTime<Utc>,
    ) -> Result<ObjectStateValidity, ActivityError> {
        Ok(
            match reported_window(self.observed_at.as_deref(), self.valid_until.as_deref())? {
                None => ObjectStateValidity::Unknown,
                Some((start, _)) if now < start => ObjectStateValidity::NotYetApplicable,
                Some((_, end)) if now >= end => ObjectStateValidity::Expired,
                Some(_) => ObjectStateValidity::WithinReportedWindow,
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStateSource {
    CallerReported,
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
#[serde(deny_unknown_fields)]
pub struct ObjectStateEntry {
    pub id: String,
    pub activity_id: String,
    pub owner_uid: u32,
    pub recorded_at: String,
    pub source: ObjectStateSource,
    pub draft: ObjectStateDraft,
    pub receipt: Option<ReceiptReport>,
    pub superseded_by: Option<String>,
    pub validity: ObjectStateValidity,
}

pub(super) fn canonical_reference(value: &str) -> Result<String, ActivityError> {
    parse_reference(value)
        .and_then(|object| format_reference(&object))
        .map_err(|error| ActivityError::Invalid(error.to_string()))
}

fn reported_window(
    observed_at: Option<&str>,
    valid_until: Option<&str>,
) -> Result<Option<(DateTime<Utc>, DateTime<Utc>)>, ActivityError> {
    match (observed_at, valid_until) {
        (None, None) => Ok(None),
        (Some(start), Some(end)) => {
            let parse = |value, field| {
                DateTime::parse_from_rfc3339(value)
                    .map(|time| time.with_timezone(&Utc))
                    .map_err(|_| {
                        ActivityError::Invalid(format!("{field} must be an RFC3339 timestamp"))
                    })
            };
            let start = parse(start, "observed_at")?;
            let end = parse(end, "valid_until")?;
            if end <= start {
                return Err(ActivityError::Invalid(
                    "valid_until must be strictly after observed_at".into(),
                ));
            }
            Ok(Some((start, end)))
        }
        _ => Err(ActivityError::Invalid(
            "observed_at and valid_until must either both be supplied or both be absent".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/object_state.rs"
    ));
}
