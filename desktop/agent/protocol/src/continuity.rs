//! Closed portable Activity continuity presentation.
//!
//! This duplicates the published wire shape without importing core models.
//! Documents carry intent, App references and safe rules only.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{ActivitySchedulingPriority, ActivityState};

pub const ACTIVITY_CONTINUITY_KIND: &str = "claw_os.activity_continuity";
pub const ACTIVITY_CONTINUITY_SCHEMA_VERSION: u32 = 1;
pub const MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES: usize = 192 * 1024;

const MAX_JSON_DEPTH: usize = 8;
const MAX_JSON_CONTAINERS: usize = 128;
const MAX_RAW_JSON_STRING_BYTES: usize = 128 * 1024;
const MAX_TITLE_BYTES: usize = 240;
const MAX_GOAL_BYTES: usize = 16 * 1024;
const MAX_PLANNING_BYTES: usize = 8 * 1024;
const MAX_REFERENCES: usize = 32;
const MAX_REFERENCE_LABEL_BYTES: usize = 240;
const MAX_REFERENCE_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityExecutionPlacement {
    Local,
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

impl ActivityContinuityDocument {
    pub fn from_json(data: &[u8]) -> Result<Self, String> {
        validate_json_envelope(data)?;
        let document: Self = serde_json::from_slice(data)
            .map_err(|error| format!("Invalid Activity continuity JSON: {error}"))?;
        document.validate()?;
        Ok(document)
    }

    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        let data = serde_json::to_vec(self)
            .map_err(|error| format!("Could not encode Activity continuity JSON: {error}"))?;
        if data.len() > MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES {
            return Err(format!(
                "Activity continuity document exceeds {MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES} bytes"
            ));
        }
        String::from_utf8(data).map_err(|_| "Activity continuity JSON was not UTF-8".into())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.kind != ACTIVITY_CONTINUITY_KIND {
            return Err(format!(
                "Activity continuity kind must be {ACTIVITY_CONTINUITY_KIND}"
            ));
        }
        if self.schema_version != ACTIVITY_CONTINUITY_SCHEMA_VERSION {
            return Err(format!(
                "Unsupported Activity continuity schema version {}; supported version is {}",
                self.schema_version, ACTIVITY_CONTINUITY_SCHEMA_VERSION
            ));
        }
        validate_canonical_uuid("Continuity lineage id", &self.lineage.id)?;
        if self.lineage.revision == 0 || self.lineage.revision > i64::MAX as u64 {
            return Err("Continuity revision must be a representable positive integer".into());
        }
        validate_canonical_text("Title", &self.intent.title, MAX_TITLE_BYTES, false, false)?;
        validate_canonical_text("Goal", &self.intent.goal, MAX_GOAL_BYTES, false, true)?;
        validate_canonical_text(
            "Completion criteria",
            &self.intent.completion_criteria,
            MAX_PLANNING_BYTES,
            true,
            true,
        )?;
        validate_canonical_text(
            "Boundaries",
            &self.intent.boundaries,
            MAX_PLANNING_BYTES,
            true,
            true,
        )?;
        if self.references.len() > MAX_REFERENCES {
            return Err(format!(
                "Activity continuity references exceed the maximum of {MAX_REFERENCES}"
            ));
        }
        for reference in &self.references {
            validate_canonical_text(
                "Continuity reference label",
                &reference.label,
                MAX_REFERENCE_LABEL_BYTES,
                false,
                false,
            )?;
            validate_app_reference(&reference.reference)?;
        }
        if let Some(limits) = &self.rules.execution_limits {
            if !(1..=1000).contains(&limits.max_attempts) {
                return Err("Continuity max_attempts must be between 1 and 1000".into());
            }
            if !(1..=100).contains(&limits.max_turns_per_attempt) {
                return Err("Continuity max_turns_per_attempt must be between 1 and 100".into());
            }
            let expiry = DateTime::parse_from_rfc3339(&limits.expires_at)
                .map_err(|_| "Continuity execution expiry must be RFC3339")?
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Nanos, true);
            if expiry != limits.expires_at {
                return Err("Continuity execution expiry must be canonical UTC RFC3339".into());
            }
        }
        let expected = self.snapshot_digest()?;
        if self.snapshot != expected {
            return Err("Activity continuity snapshot digest does not match the document".into());
        }
        Ok(())
    }

    fn snapshot_digest(&self) -> Result<String, String> {
        #[derive(Serialize)]
        struct SnapshotMaterial<'a> {
            kind: &'a str,
            schema_version: u32,
            lineage: &'a ActivityContinuityLineage,
            intent: &'a PortableActivityIntent,
            references: &'a [PortableActivityReference],
            rules: &'a PortableActivityRules,
        }
        let data = serde_json::to_vec(&SnapshotMaterial {
            kind: &self.kind,
            schema_version: self.schema_version,
            lineage: &self.lineage,
            intent: &self.intent,
            references: &self.references,
            rules: &self.rules,
        })
        .map_err(|error| format!("Could not encode continuity snapshot: {error}"))?;
        Ok(format!("sha256:{:x}", Sha256::digest(data)))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityContinuityExportQuery {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityContinuityImportRequest {
    pub placement: ActivityExecutionPlacement,
    pub document: String,
}

impl ActivityContinuityImportRequest {
    pub fn validated_document(&self) -> Result<ActivityContinuityDocument, String> {
        ActivityContinuityDocument::from_json(self.document.as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedActivity {
    pub id: String,
    pub title: String,
    pub goal: String,
    pub completion_criteria: String,
    pub boundaries: String,
    pub resources: Vec<ImportedActivityResource>,
    pub state: ActivityState,
    pub completion_note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedActivityResource {
    pub label: String,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityContinuityImportAcknowledgement {
    pub activity: ImportedActivity,
    pub continuity_id: String,
    pub continuity_revision: u64,
    pub placement: ActivityExecutionPlacement,
}

impl ActivityContinuityImportAcknowledgement {
    pub fn matches(
        &self,
        document: &ActivityContinuityDocument,
        placement: ActivityExecutionPlacement,
    ) -> bool {
        self.placement == placement
            && placement == ActivityExecutionPlacement::Local
            && self.continuity_id == document.lineage.id
            && self.continuity_revision == document.lineage.revision
            && validate_canonical_uuid("Imported Activity id", &self.activity.id).is_ok()
            && self.activity.id != document.lineage.id
            && self.activity.state == ActivityState::Paused
            && self.activity.completion_note.is_none()
            && self.activity.title == document.intent.title
            && self.activity.goal == document.intent.goal
            && self.activity.completion_criteria == document.intent.completion_criteria
            && self.activity.boundaries == document.intent.boundaries
            && self.activity.resources
                == document
                    .references
                    .iter()
                    .map(|reference| ImportedActivityResource {
                        label: reference.label.clone(),
                        reference: reference.reference.clone(),
                    })
                    .collect::<Vec<_>>()
            && DateTime::parse_from_rfc3339(&self.activity.created_at).is_ok()
            && DateTime::parse_from_rfc3339(&self.activity.updated_at).is_ok()
    }
}

fn validate_canonical_text(
    field: &str,
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
    multiline: bool,
) -> Result<(), String> {
    let trimmed = value.trim();
    if value != trimmed {
        return Err(format!("{field} must be canonically trimmed"));
    }
    if (!allow_empty && value.is_empty()) || value.len() > max_bytes {
        return Err(format!("{field} is outside its UTF-8 byte bound"));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() && !(multiline && matches!(ch, '\n' | '\r' | '\t')))
    {
        return Err(format!("{field} contains unsupported control characters"));
    }
    Ok(())
}

fn validate_canonical_uuid(field: &str, value: &str) -> Result<(), String> {
    let id = Uuid::parse_str(value).map_err(|_| format!("{field} must be a UUID"))?;
    if id.to_string() != value {
        return Err(format!("{field} must be a canonical UUID"));
    }
    Ok(())
}

fn validate_app_reference(value: &str) -> Result<(), String> {
    if value.len() > MAX_REFERENCE_BYTES {
        return Err("Activity continuity App reference exceeds 4096 UTF-8 bytes".into());
    }
    let rest = value
        .strip_prefix("app://")
        .ok_or("Activity continuity references must use the exact app:// scheme")?;
    let (address, query) = rest
        .split_once('?')
        .ok_or("Activity continuity App reference requires an id query parameter")?;
    let (app_id, object_type) = address
        .split_once('/')
        .ok_or("Activity continuity App reference requires one App and object type")?;
    if !component(app_id, 128) || !component(object_type, 64) {
        return Err("Activity continuity App reference has an invalid component".into());
    }
    let mut object_id = None;
    let mut revision = None;
    for parameter in query.split('&') {
        let (key, encoded) = parameter
            .split_once('=')
            .ok_or("Activity continuity App reference has an invalid query parameter")?;
        match key {
            "id" if object_id.is_none() => object_id = Some(decode(encoded)?),
            "revision" if revision.is_none() => revision = Some(decode(encoded)?),
            _ => {
                return Err(
                    "Activity continuity App reference has an unknown or repeated parameter".into(),
                );
            }
        }
    }
    let object_id =
        object_id.ok_or("Activity continuity App reference requires an id query parameter")?;
    if !opaque(&object_id, 1024) || revision.as_ref().is_some_and(|value| !opaque(value, 128)) {
        return Err("Activity continuity App reference has an invalid opaque value".into());
    }
    let mut canonical = format!("app://{app_id}/{object_type}?id={}", encode(&object_id));
    if let Some(revision) = revision {
        canonical.push_str("&revision=");
        canonical.push_str(&encode(&revision));
    }
    if canonical != value {
        return Err("Activity continuity App reference is not canonically spelled".into());
    }
    Ok(())
}

fn component(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn opaque(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if unreserved(byte) {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    output
}

fn decode(value: &str) -> Result<String, String> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if unreserved(byte) {
            decoded.push(byte);
        } else if byte == b'%' {
            let high = bytes.next().and_then(hex);
            let low = bytes.next().and_then(hex);
            match (high, low) {
                (Some(high), Some(low)) => decoded.push((high << 4) | low),
                _ => return Err("Activity continuity App reference has an invalid escape".into()),
            }
        } else {
            return Err("Activity continuity App reference query must use percent encoding".into());
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| "Activity continuity App reference is not valid UTF-8".into())
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn validate_json_envelope(data: &[u8]) -> Result<(), String> {
    if data.is_empty() {
        return Err("Activity continuity document is empty".into());
    }
    if data.len() > MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES {
        return Err(format!(
            "Activity continuity document exceeds {MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES} bytes"
        ));
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
                return Err("Activity continuity JSON string is too large".into());
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
                    return Err("Activity continuity JSON is too deeply nested".into());
                }
                if containers > MAX_JSON_CONTAINERS {
                    return Err("Activity continuity JSON has too many containers".into());
                }
            }
            b'}' | b']' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("Activity continuity JSON is unbalanced")?;
            }
            _ => {}
        }
    }
    if in_string || depth != 0 {
        return Err("Activity continuity JSON is incomplete".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/continuity.rs"
    ));
}
