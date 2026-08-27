use super::*;

#[test]
fn launch_handles_are_never_written_to_the_audit_trail() {
    let params = json!({
        "session_id": "app-1",
        "handle": "d34db33f",
        "pid": 4242
    });
    let redacted = redact_params(&params).expect("handle must be masked");
    assert_eq!(redacted["handle"], json!("<redacted>"));
    assert_eq!(redacted["session_id"], json!("app-1"));
    assert_eq!(redacted["pid"], json!(4242));
    assert!(
        !serde_json::to_string(&redacted).unwrap().contains("d34db33f"),
        "the bearer value must not survive serialization"
    );
}

#[test]
fn only_bearer_fields_are_masked_in_the_audit_trail() {
    // Nothing secret rides the App-session protocol any more; the only
    // masked field is the launch handle, and everything else is
    // recorded so a launch stays reconstructable.
    let params = json!({
        "app_id": "user-manager",
        "kind": "operation",
        "operation": "create-user"
    });
    assert!(redact_params(&params).is_none());
}

#[test]
fn error_data_never_reaches_the_audit_record() {
    let response = Response::error_with_data(
        None,
        "request_failed",
        crate::clawd::protocol::BrokerError::with_data(
            "launcher cannot delegate sys.identity:name:accounts; awaiting approval",
            json!({"status": "approval_required", "approval_requests": ["ap-1"]}),
        ),
    );
    let audit = RequestAudit {
        ts: Utc::now(),
        event: "clawd.request",
        command: "app_session.register",
        ok: response.ok,
        duration_ms: 1,
        params: &json!({}),
        client: &ClientIdentity::unknown(),
        error_code: response.error.as_ref().map(|err| err.code.as_str()),
        error_message: response.error.as_ref().map(|err| err.message.as_str()),
    };
    let rendered = serde_json::to_string(&audit).expect("serialize");
    assert!(
        !rendered.contains("approval_requests"),
        "structured denial data is for the peer only, never the audit trail"
    );
    assert!(rendered.contains("awaiting approval"), "the reason is still recorded");
}

#[test]
fn requests_without_bearer_fields_are_recorded_verbatim() {
    assert!(redact_params(&json!({"session_id": "app-1"})).is_none());
    assert!(redact_params(&Value::Null).is_none());
}
