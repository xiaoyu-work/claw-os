//! Common envelope handling for wire v1 replies.
//!
//! Today the kernel emits flat per-command shapes — policy checks return
//! `{ "decision": "...", "verb": "..." }`, `cos ai chat`
//! returns `{ "text": "...", "model": "...", ... }`, and the few
//! errors that escape the app dispatcher are `{ "error": "...", "code": "..." }`.
//!
//! Wire v1's *target* shape wraps that in a uniform
//! `{ "ok": bool, "data": {...}, "error": "...", "code": "...", "wire_version": 1 }`
//! envelope. This module performs the adaptation in **both** directions
//! so callers can already program against v1 semantics:
//!
//! * [`Envelope::accept`] takes the kernel's current flat reply and
//!   normalises it into v1 shape.
//! * [`Envelope::data`] returns the inner payload as a
//!   [`serde_json::Value`] for ad-hoc inspection.
//!
//! Once the kernel migrates to emitting v1 envelopes natively, this
//! module's `accept` will become a passthrough and no caller will
//! notice. That's the point of the abstraction.

use serde::{Deserialize, Serialize};

/// Wire v1 reply envelope. See `wire/v1/envelope.schema.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub ok: bool,
    #[serde(default = "default_wire_version")]
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

fn default_wire_version() -> u32 {
    1
}

impl Envelope {
    /// Adapt a raw kernel reply (current flat shape or future native
    /// envelope) into the wire v1 [`Envelope`] surface.
    ///
    /// If the input already has an `ok` field we treat it as a
    /// native v1 envelope; otherwise we infer:
    ///   - an `error` (and optionally `code`) field ⇒ failure
    ///     envelope with `detail = entire input`
    ///   - anything else ⇒ success envelope with `data = entire input`
    pub fn accept(raw: serde_json::Value) -> Self {
        // If a `wire_version` field is present, reject anything we
        // don't speak before we trust the surrounding shape. A future
        // wire/v2 kernel must be matched by a wire/v2 SDK; silently
        // downgrading would let mismatched field semantics through.
        if let Some(v) = raw.get("wire_version") {
            // Schema pins this to const: 1; allow integer or string
            // (the JSON spec doesn't constrain ints) but require it
            // to equal the constant.
            let ok = match v {
                serde_json::Value::Number(n) => n.as_u64() == Some(1),
                _ => false,
            };
            if !ok {
                return Envelope::synthetic_error(&format!(
                    "unsupported wire_version: got {v}, expected 1",
                ));
            }
        }
        if raw.get("ok").is_some() {
            return serde_json::from_value(raw).unwrap_or_else(|_| Envelope::synthetic_error(
                "envelope had `ok` but didn't deserialise",
            ));
        }
        if let Some(err) = raw.get("error").and_then(|v| v.as_str()) {
            return Envelope {
                ok: false,
                wire_version: 1,
                audit_id: raw
                    .get("audit_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                data: None,
                error: Some(err.to_string()),
                code: raw
                    .get("code")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                detail: Some(raw),
            };
        }
        Envelope {
            ok: true,
            wire_version: 1,
            audit_id: raw
                .get("audit_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            data: Some(raw),
            error: None,
            code: None,
            detail: None,
        }
    }

    /// Convenience constructor for an internally-generated error
    /// envelope (e.g. when the SDK couldn't even contact the kernel).
    pub fn synthetic_error(message: &str) -> Self {
        Envelope {
            ok: false,
            wire_version: 1,
            audit_id: None,
            data: None,
            error: Some(message.to_string()),
            code: Some("KERNEL_UNAVAILABLE".to_string()),
            detail: None,
        }
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
    use super::*;
    use serde_json::json;

    #[test]
    fn accept_flat_success() {
        let env = Envelope::accept(json!({"decision": "allow", "verb": "fs.read"}));
        assert!(env.ok);
        assert_eq!(env.wire_version, 1);
        assert_eq!(env.data.unwrap()["decision"], "allow");
    }

    #[test]
    fn accept_flat_error() {
        let env = Envelope::accept(json!({"error": "nope", "code": "PERMISSION_DENIED"}));
        assert!(!env.ok);
        assert_eq!(env.error.as_deref(), Some("nope"));
        assert_eq!(env.code.as_deref(), Some("PERMISSION_DENIED"));
    }

    #[test]
    fn accept_native_v1() {
        let env = Envelope::accept(json!({
            "ok": true,
            "wire_version": 1,
            "data": {"verb": "fs.read"}
        }));
        assert!(env.ok);
    }

    #[test]
    fn error_code_roundtrip() {
        for code in ["PERMISSION_DENIED", "BUDGET_EXCEEDED", "SOMETHING_NEW"] {
            assert_eq!(ErrorCode::from_str_lossy(code).as_str(), code);
        }
    }
}
