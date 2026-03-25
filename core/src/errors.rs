/// Standard error codes for Claw OS.
///
/// Agents can match on `code` field instead of parsing error messages.
/// Code format: <category>.<specific> (e.g., "auth.tier_denied")
use serde_json::{json, Value};

// ---- Category: auth (permission/credential issues) ----
pub const AUTH_TIER_DENIED: &str = "auth.tier_denied";
pub const AUTH_SCOPE_VIOLATION: &str = "auth.scope_violation";
pub const AUTH_CREDENTIAL_NOT_FOUND: &str = "auth.credential_not_found";
pub const AUTH_CREDENTIAL_EXPIRED: &str = "auth.credential_expired";
pub const AUTH_REFRESH_FAILED: &str = "auth.refresh_failed";

// ---- Category: resource (not found, already exists) ----
pub const RESOURCE_NOT_FOUND: &str = "resource.not_found";
pub const RESOURCE_ALREADY_EXISTS: &str = "resource.already_exists";
pub const RESOURCE_BUSY: &str = "resource.busy";

// ---- Category: input (bad arguments, validation) ----
pub const INPUT_MISSING_REQUIRED: &str = "input.missing_required";
pub const INPUT_INVALID_VALUE: &str = "input.invalid_value";
pub const INPUT_UNKNOWN_COMMAND: &str = "input.unknown_command";

// ---- Category: limit (quota, rate, timeout) ----
pub const LIMIT_RATE_EXCEEDED: &str = "limit.rate_exceeded";
pub const LIMIT_QUOTA_EXCEEDED: &str = "limit.quota_exceeded";
pub const LIMIT_TIMEOUT: &str = "limit.timeout";
pub const LIMIT_OOM: &str = "limit.out_of_memory";

// ---- Category: io (filesystem, network) ----
pub const IO_FILE_NOT_FOUND: &str = "io.file_not_found";
pub const IO_PERMISSION_DENIED: &str = "io.permission_denied";
pub const IO_DISK_FULL: &str = "io.disk_full";
pub const IO_NETWORK_ERROR: &str = "io.network_error";
pub const IO_CONNECTION_REFUSED: &str = "io.connection_refused";

// ---- Category: provider (external service failures) ----
pub const PROVIDER_NOT_CONFIGURED: &str = "provider.not_configured";
pub const PROVIDER_API_ERROR: &str = "provider.api_error";
pub const PROVIDER_UNAVAILABLE: &str = "provider.unavailable";

// ---- Category: system (internal errors) ----
pub const SYSTEM_INTERNAL: &str = "system.internal";
pub const SYSTEM_NOT_SUPPORTED: &str = "system.not_supported";

/// Build a structured error response with an error code.
///
/// Example output:
/// ```json
/// {
///   "error": "credential not found: OPENAI_KEY",
///   "code": "auth.credential_not_found"
/// }
/// ```
pub fn error(code: &str, message: &str) -> Value {
    json!({
        "error": message,
        "code": code,
    })
}

/// Build a structured error response with recovery guidance.
///
/// Example output:
/// ```json
/// {
///   "error": "credential not found: OPENAI_KEY",
///   "code": "auth.credential_not_found",
///   "recovery": {
///     "hint": "Store the credential first",
///     "try": ["cos credential store OPENAI_KEY <value> --tier 0"]
///   }
/// }
/// ```
pub fn error_with_recovery(code: &str, message: &str, hint: &str, try_cmds: &[&str]) -> Value {
    let try_arr: Vec<Value> = try_cmds.iter().map(|s| json!(s)).collect();
    json!({
        "error": message,
        "code": code,
        "recovery": {
            "hint": hint,
            "try": try_arr,
        }
    })
}

/// Build a structured error response with arbitrary details.
///
/// Example output:
/// ```json
/// {
///   "error": "rate limit exceeded",
///   "code": "limit.rate_exceeded",
///   "details": {"retry_after_secs": 12}
/// }
/// ```
pub fn error_with_details(code: &str, message: &str, details: Value) -> Value {
    json!({
        "error": message,
        "code": code,
        "details": details,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_basic() {
        let e = error(
            AUTH_CREDENTIAL_NOT_FOUND,
            "credential not found: OPENAI_KEY",
        );
        assert_eq!(e["error"], "credential not found: OPENAI_KEY");
        assert_eq!(e["code"], "auth.credential_not_found");
    }

    #[test]
    fn error_with_recovery_includes_all_fields() {
        let e = error_with_recovery(
            IO_FILE_NOT_FOUND,
            "file not found: /den/missing.txt",
            "Check the path exists",
            &["cos app fs ls /den"],
        );
        assert_eq!(e["code"], "io.file_not_found");
        assert!(e["recovery"]["hint"].is_string());
        assert!(e["recovery"]["try"].is_array());
    }

    #[test]
    fn error_with_details_includes_details() {
        let e = error_with_details(
            LIMIT_RATE_EXCEEDED,
            "rate limit exceeded",
            json!({"retry_after_secs": 12}),
        );
        assert_eq!(e["code"], "limit.rate_exceeded");
        assert_eq!(e["details"]["retry_after_secs"], 12);
    }

    #[test]
    fn error_codes_are_dot_separated() {
        let codes = [
            AUTH_TIER_DENIED,
            AUTH_SCOPE_VIOLATION,
            AUTH_CREDENTIAL_NOT_FOUND,
            RESOURCE_NOT_FOUND,
            RESOURCE_ALREADY_EXISTS,
            INPUT_MISSING_REQUIRED,
            INPUT_INVALID_VALUE,
            INPUT_UNKNOWN_COMMAND,
            LIMIT_RATE_EXCEEDED,
            LIMIT_TIMEOUT,
            IO_FILE_NOT_FOUND,
            IO_PERMISSION_DENIED,
            PROVIDER_NOT_CONFIGURED,
            PROVIDER_API_ERROR,
            SYSTEM_INTERNAL,
        ];
        for code in &codes {
            assert!(code.contains('.'), "code should be dot-separated: {code}");
            let parts: Vec<&str> = code.split('.').collect();
            assert_eq!(parts.len(), 2, "code should have exactly 2 parts: {code}");
        }
    }
}
