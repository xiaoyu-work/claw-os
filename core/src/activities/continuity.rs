//! Portable continuity of Activity intent and semantic context.
//!
//! This document is deliberately not a database row, authority bundle,
//! execution record, or backup format.

use serde::{Deserialize, Serialize};

use super::{
    ActivityDraft, ActivityError, ActivityResource, ActivitySchedulingPriority,
    ExecutionLimitsDraft,
};

pub const CONTINUITY_KIND: &str = "claw_os.activity_continuity";
pub const CONTINUITY_SCHEMA_VERSION: u32 = 1;
pub const MAX_CONTINUITY_DOCUMENT_BYTES: usize = 192 * 1024;

const MAX_JSON_DEPTH: usize = 8;
const MAX_JSON_CONTAINERS: usize = 128;
const MAX_RAW_JSON_STRING_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityExecutionPlacement {
    Local,
}

impl ActivityExecutionPlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityContinuityLineage {
    pub id: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableActivityIntent {
    pub title: String,
    pub goal: String,
    pub completion_criteria: String,
    pub boundaries: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableActivityReference {
    pub label: String,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableExecutionLimits {
    pub enabled: bool,
    pub max_attempts: u32,
    pub max_turns_per_attempt: u32,
    pub expires_at: String,
}

impl PortableExecutionLimits {
    pub(crate) fn draft(&self) -> ExecutionLimitsDraft {
        ExecutionLimitsDraft {
            max_attempts: self.max_attempts,
            max_turns_per_attempt: self.max_turns_per_attempt,
            expires_at: self.expires_at.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableSchedulingPreference {
    pub priority: ActivitySchedulingPriority,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableActivityRules {
    pub execution_limits: Option<PortableExecutionLimits>,
    pub scheduling: Option<PortableSchedulingPreference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityContinuityDocument {
    pub kind: String,
    pub schema_version: u32,
    pub lineage: ActivityContinuityLineage,
    pub snapshot: String,
    pub intent: PortableActivityIntent,
    pub references: Vec<PortableActivityReference>,
    pub rules: PortableActivityRules,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityContinuityImport {
    pub activity: super::Activity,
    pub continuity_id: String,
    pub continuity_revision: u64,
    pub placement: ActivityExecutionPlacement,
}

impl ActivityContinuityDocument {
    pub fn from_json(data: &[u8]) -> Result<Self, ActivityError> {
        validate_json_envelope(data)?;
        let document: Self = serde_json::from_slice(data)?;
        document.validate()?;
        Ok(document)
    }

    pub fn to_json(&self) -> Result<Vec<u8>, ActivityError> {
        self.validate()?;
        let data = serde_json::to_vec(self)?;
        if data.len() > MAX_CONTINUITY_DOCUMENT_BYTES {
            return Err(ActivityError::Invalid(format!(
                "Activity continuity document exceeds {MAX_CONTINUITY_DOCUMENT_BYTES} bytes"
            )));
        }
        Ok(data)
    }

    pub fn validate(&self) -> Result<(), ActivityError> {
        if self.kind != CONTINUITY_KIND {
            return Err(ActivityError::Invalid(format!(
                "continuity kind must be {CONTINUITY_KIND}"
            )));
        }
        if self.schema_version != CONTINUITY_SCHEMA_VERSION {
            return Err(ActivityError::Invalid(format!(
                "unsupported Activity continuity schema version {}; supported version is {}",
                self.schema_version, CONTINUITY_SCHEMA_VERSION
            )));
        }
        let canonical_id = super::parse_id(&self.lineage.id)?;
        if canonical_id != self.lineage.id {
            return Err(ActivityError::Invalid(
                "continuity lineage id must be a canonical UUID".into(),
            ));
        }
        if self.lineage.revision == 0 || i64::try_from(self.lineage.revision).is_err() {
            return Err(ActivityError::Invalid(
                "continuity revision must be a representable positive integer".into(),
            ));
        }

        let resources = references_as_resources(&self.references)?;
        let draft = ActivityDraft {
            title: self.intent.title.clone(),
            goal: self.intent.goal.clone(),
            completion_criteria: self.intent.completion_criteria.clone(),
            boundaries: self.intent.boundaries.clone(),
            resources,
        };
        draft.validate()?;
        if draft.title.trim() != draft.title
            || draft.goal.trim() != draft.goal
            || draft.completion_criteria.trim() != draft.completion_criteria
            || draft.boundaries.trim() != draft.boundaries
        {
            return Err(ActivityError::Invalid(
                "continuity intent text must be canonically trimmed".into(),
            ));
        }

        if let Some(limits) = &self.rules.execution_limits {
            limits.draft().validate()?;
            let canonical_expiry = limits.draft().canonicalized()?.expires_at;
            if canonical_expiry != limits.expires_at {
                return Err(ActivityError::Invalid(
                    "continuity execution expiry must be canonical UTC RFC3339".into(),
                ));
            }
        }
        let expected = snapshot_digest(
            &self.kind,
            self.schema_version,
            &self.lineage,
            &self.intent,
            &self.references,
            &self.rules,
        )?;
        if self.snapshot != expected {
            return Err(ActivityError::Invalid(
                "continuity snapshot digest does not match the document".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn build(
        lineage: ActivityContinuityLineage,
        intent: PortableActivityIntent,
        references: Vec<PortableActivityReference>,
        rules: PortableActivityRules,
    ) -> Result<Self, ActivityError> {
        let snapshot = snapshot_digest(
            CONTINUITY_KIND,
            CONTINUITY_SCHEMA_VERSION,
            &lineage,
            &intent,
            &references,
            &rules,
        )?;
        let document = Self {
            kind: CONTINUITY_KIND.to_string(),
            schema_version: CONTINUITY_SCHEMA_VERSION,
            lineage,
            snapshot,
            intent,
            references,
            rules,
        };
        document.validate()?;
        Ok(document)
    }

    pub(crate) fn draft(&self) -> Result<ActivityDraft, ActivityError> {
        self.validate()?;
        Ok(ActivityDraft {
            title: self.intent.title.clone(),
            goal: self.intent.goal.clone(),
            completion_criteria: self.intent.completion_criteria.clone(),
            boundaries: self.intent.boundaries.clone(),
            resources: references_as_resources(&self.references)?,
        })
    }
}

pub(crate) fn portable_references(
    resources: &[ActivityResource],
) -> Result<Vec<PortableActivityReference>, ActivityError> {
    let mut references = Vec::new();
    for resource in resources {
        if !crate::objects::is_app_reference(&resource.reference) {
            continue;
        }
        if crate::objects::parse_reference(&resource.reference).is_err() {
            continue;
        }
        references.push(PortableActivityReference {
            label: resource.label.clone(),
            reference: resource.reference.clone(),
        });
    }
    validate_references(&references)?;
    Ok(references)
}

fn references_as_resources(
    references: &[PortableActivityReference],
) -> Result<Vec<ActivityResource>, ActivityError> {
    validate_references(references)?;
    Ok(references
        .iter()
        .map(|reference| ActivityResource {
            label: reference.label.clone(),
            reference: reference.reference.clone(),
        })
        .collect())
}

fn validate_references(references: &[PortableActivityReference]) -> Result<(), ActivityError> {
    if references.len() > super::MAX_RESOURCES {
        return Err(ActivityError::Invalid(format!(
            "continuity references exceed the maximum of {}",
            super::MAX_RESOURCES
        )));
    }
    for reference in references {
        super::validate_text(
            "continuity reference label",
            &reference.label,
            super::MAX_RESOURCE_LABEL_BYTES,
            false,
            false,
        )?;
        if reference.label.trim() != reference.label {
            return Err(ActivityError::Invalid(
                "continuity reference labels must be canonically trimmed".into(),
            ));
        }
        crate::objects::parse_reference(&reference.reference)
            .map_err(|error| ActivityError::Invalid(error.to_string()))?;
    }
    Ok(())
}

#[derive(Serialize)]
struct SnapshotMaterial<'a> {
    kind: &'a str,
    schema_version: u32,
    lineage: &'a ActivityContinuityLineage,
    intent: &'a PortableActivityIntent,
    references: &'a [PortableActivityReference],
    rules: &'a PortableActivityRules,
}

fn snapshot_digest(
    kind: &str,
    schema_version: u32,
    lineage: &ActivityContinuityLineage,
    intent: &PortableActivityIntent,
    references: &[PortableActivityReference],
    rules: &PortableActivityRules,
) -> Result<String, ActivityError> {
    let bytes = serde_json::to_vec(&SnapshotMaterial {
        kind,
        schema_version,
        lineage,
        intent,
        references,
        rules,
    })?;
    Ok(format!("sha256:{}", crate::crypto::sha256_hex(&bytes)))
}

fn validate_json_envelope(data: &[u8]) -> Result<(), ActivityError> {
    if data.is_empty() {
        return Err(ActivityError::Invalid(
            "Activity continuity document is empty".into(),
        ));
    }
    if data.len() > MAX_CONTINUITY_DOCUMENT_BYTES {
        return Err(ActivityError::Invalid(format!(
            "Activity continuity document exceeds {MAX_CONTINUITY_DOCUMENT_BYTES} bytes"
        )));
    }
    let mut depth = 0_usize;
    let mut containers = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut string_bytes = 0_usize;
    for byte in data {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
                string_bytes = 0;
                continue;
            }
            string_bytes += 1;
            if string_bytes > MAX_RAW_JSON_STRING_BYTES {
                return Err(ActivityError::Invalid(
                    "Activity continuity JSON string is too large".into(),
                ));
            }
            continue;
        }
        match *byte {
            b'"' => {
                in_string = true;
                string_bytes = 0;
            }
            b'{' | b'[' => {
                depth += 1;
                containers += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err(ActivityError::Invalid(
                        "Activity continuity JSON is too deeply nested".into(),
                    ));
                }
                if containers > MAX_JSON_CONTAINERS {
                    return Err(ActivityError::Invalid(
                        "Activity continuity JSON has too many containers".into(),
                    ));
                }
            }
            b'}' | b']' => {
                depth = depth.checked_sub(1).ok_or_else(|| {
                    ActivityError::Invalid("Activity continuity JSON is unbalanced".into())
                })?;
            }
            _ => {}
        }
    }
    if in_string || depth != 0 {
        return Err(ActivityError::Invalid(
            "Activity continuity JSON is incomplete".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/continuity.rs"
    ));
}
