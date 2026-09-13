//! Bounded caller reports, kept separate from verified manifest declarations.
//! Neither a report nor an App declaration confirms effects or grants authority.

use serde::{Deserialize, Serialize};

use crate::caps::manifest::{EffectKind, EffectRecovery};

use super::{parse_id, ActivityError};

const MAX_RESULT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REPORT_TEXT_BYTES: usize = 2048;
const MAX_LABEL_BYTES: usize = 512;
const MAX_NAME_BYTES: usize = 128;
const MAX_EFFECTS: usize = 16;
const SESSION_TOOL_PREFIX: &str = "session:";
const EMPTY_SHA256: &str =
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOutcome {
    Returned,
    ReportedError,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    Json,
    Text,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultSummary {
    pub kind: ResultKind,
    pub sha256: String,
    pub bytes: u64,
    pub preview: String,
    pub preview_truncated: bool,
}

impl ResultSummary {
    fn validate(&self) -> Result<(), ActivityError> {
        validate_digest("result sha256", &self.sha256)?;
        if self.bytes > MAX_RESULT_BYTES {
            return invalid("result bytes exceeds 16 MiB");
        }
        bounded_text("result preview", &self.preview, MAX_REPORT_TEXT_BYTES)?;
        if self
            .preview
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
        {
            return invalid("result preview contains unsupported control characters");
        }
        match self.kind {
            ResultKind::Empty
                if self.bytes != 0
                    || !self.preview.is_empty()
                    || self.preview_truncated
                    || self.sha256 != EMPTY_SHA256 =>
            {
                invalid("an empty result requires zero bytes, the empty digest, and no preview")
            }
            ResultKind::Json | ResultKind::Text if self.bytes == 0 => {
                invalid("JSON and text results require a nonzero byte count")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptReport {
    pub id: String,
    pub app_id: String,
    pub operation: String,
    pub package_digest: String,
    pub outcome: ReceiptOutcome,
    pub result: Option<ResultSummary>,
    pub error: Option<String>,
}

impl ReceiptReport {
    pub fn session_operation(tool: &str) -> String {
        format!("{SESSION_TOOL_PREFIX}{tool}")
    }

    pub fn session_tool_name(&self) -> Option<&str> {
        self.operation.strip_prefix(SESSION_TOOL_PREFIX)
    }

    pub fn validate(&self) -> Result<(), ActivityError> {
        parse_id(&self.id)?;
        if !name(&self.app_id, false) {
            return invalid(
                "receipt app_id must be a lowercase App identifier of at most 128 bytes",
            );
        }
        if let Some(tool) = self.session_tool_name() {
            if !session_tool_name(tool) {
                return invalid(
                    "receipt session tool must match [a-z][a-z0-9._-]* and fit 128 bytes",
                );
            }
        } else if !name(&self.operation, true) {
            return invalid("receipt operation must be a lowercase operation name of at most 128 bytes without '..', or session:<tool>");
        }
        validate_digest("receipt package_digest", &self.package_digest)?;
        if let Some(result) = &self.result {
            result.validate()?;
        }
        if let Some(error) = &self.error {
            required_error("receipt error", error)?;
        }
        match self.outcome {
            ReceiptOutcome::Returned if self.result.is_some() && self.error.is_none() => Ok(()),
            ReceiptOutcome::ReportedError if self.result.is_some() && self.error.is_some() => {
                Ok(())
            }
            ReceiptOutcome::Indeterminate if self.result.is_none() && self.error.is_some() => {
                Ok(())
            }
            _ => invalid("receipt outcome does not match its result and error fields"),
        }
    }

    pub(super) fn canonicalized(mut self) -> Result<Self, ActivityError> {
        self.validate()?;
        self.id = parse_id(&self.id)?;
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptEffect {
    pub kind: EffectKind,
    pub label: String,
    pub recovery: EffectRecovery,
    pub target_arg: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptDeclaration {
    pub app_version: String,
    pub operation_label: String,
    pub effects: Vec<ReceiptEffect>,
}

impl ReceiptDeclaration {
    /// Check metadata bounds; package authentication and digest matching remain
    /// the broker's responsibility.
    pub fn validate(&self) -> Result<(), ActivityError> {
        bounded_text("declaration app_version", &self.app_version, MAX_NAME_BYTES)?;
        bounded_text(
            "declaration operation_label",
            &self.operation_label,
            MAX_LABEL_BYTES,
        )?;
        if self.effects.len() > MAX_EFFECTS {
            return invalid("receipt declaration exceeds 16 effects");
        }
        for effect in &self.effects {
            bounded_text("declaration effect label", &effect.label, MAX_LABEL_BYTES)?;
            if let Some(target) = &effect.target_arg {
                bounded_text("declaration effect target_arg", target, MAX_NAME_BYTES)?;
                if target.is_empty() || target.chars().any(char::is_control) {
                    return invalid(
                        "declaration effect target_arg must be nonempty and contain no controls",
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptSource {
    CallerReported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityReceipt {
    pub id: String,
    pub activity_id: String,
    pub owner_uid: u32,
    pub received_at: String,
    pub source: ReceiptSource,
    pub report: ReceiptReport,
    pub declaration: Option<ReceiptDeclaration>,
    pub declaration_error: Option<String>,
}

pub(super) fn validate_declaration(
    declaration: Option<&ReceiptDeclaration>,
    error: Option<&str>,
) -> Result<(), ActivityError> {
    match (declaration, error) {
        (Some(declaration), None) => declaration.validate(),
        (None, Some(error)) => required_error("receipt declaration_error", error),
        _ => invalid("a receipt requires exactly one declaration or declaration_error"),
    }
}

fn validate_digest(field: &str, value: &str) -> Result<(), ActivityError> {
    if !crate::provenance::envelope::is_sha256_ref(value) {
        return invalid(format!(
            "{field} must be sha256: followed by 64 lowercase hex digits"
        ));
    }
    Ok(())
}

fn name(value: &str, operation: bool) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NAME_BYTES
        && value.as_bytes()[0].is_ascii_lowercase()
        && (!operation || !value.contains(".."))
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || byte == b'_'
                || byte == if operation { b'.' } else { b'-' }
        })
}

fn session_tool_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NAME_BYTES
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
}

fn bounded_text(field: &str, value: &str, limit: usize) -> Result<(), ActivityError> {
    if value.len() > limit {
        return invalid(format!("{field} exceeds {limit} UTF-8 bytes"));
    }
    Ok(())
}

fn required_error(field: &str, value: &str) -> Result<(), ActivityError> {
    bounded_text(field, value, MAX_REPORT_TEXT_BYTES)?;
    if value.is_empty() {
        return invalid(format!("{field} must not be empty"));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> Result<(), ActivityError> {
    Err(ActivityError::Invalid(message.into()))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/receipts.rs"
    ));
}
