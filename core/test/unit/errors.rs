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
        "file not found: /home/cos/missing.txt",
        "Check the path exists",
        &["cos app fs ls /home/cos"],
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
