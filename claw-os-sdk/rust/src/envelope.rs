//! Strict decoding for replies requested with `cos --wire=1`.

use serde::{Deserialize, Serialize};

/// Wire v1 reply envelope. See `wire/v1/envelope.schema.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub ok: bool,
    pub wire_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl Envelope {
    pub fn decode(raw: serde_json::Value) -> Result<Self, String> {
        crate::generated::validate_envelope(&raw).map_err(|error| error.to_string())?;
        serde_json::from_value(raw).map_err(|error| format!("invalid wire envelope: {error}"))
    }
}

/// Stable error codes for wire v1. See `wire/v1/error_codes.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    PermissionDenied,
    BudgetExceeded,
    SafetyViolation,
    UnknownApp,
    UnknownVerb,
    InvalidArgs,
    KernelUnavailable,
    InternalError,
    /// Catch-all for codes the kernel may add in future versions.
    Other(String),
}

impl ErrorCode {
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "PERMISSION_DENIED" => ErrorCode::PermissionDenied,
            "BUDGET_EXCEEDED" => ErrorCode::BudgetExceeded,
            "SAFETY_VIOLATION" => ErrorCode::SafetyViolation,
            "UNKNOWN_APP" => ErrorCode::UnknownApp,
            "UNKNOWN_VERB" => ErrorCode::UnknownVerb,
            "INVALID_ARGS" => ErrorCode::InvalidArgs,
            "KERNEL_UNAVAILABLE" => ErrorCode::KernelUnavailable,
            "INTERNAL_ERROR" => ErrorCode::InternalError,
            other => ErrorCode::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            ErrorCode::PermissionDenied => "PERMISSION_DENIED",
            ErrorCode::BudgetExceeded => "BUDGET_EXCEEDED",
            ErrorCode::SafetyViolation => "SAFETY_VIOLATION",
            ErrorCode::UnknownApp => "UNKNOWN_APP",
            ErrorCode::UnknownVerb => "UNKNOWN_VERB",
            ErrorCode::InvalidArgs => "INVALID_ARGS",
            ErrorCode::KernelUnavailable => "KERNEL_UNAVAILABLE",
            ErrorCode::InternalError => "INTERNAL_ERROR",
            ErrorCode::Other(s) => s.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/envelope.rs"
    ));
}
