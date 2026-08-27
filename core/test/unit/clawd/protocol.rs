use serde_json::json;

use super::*;
use crate::clawd::routes::Command;

#[test]
fn a_request_carries_the_protocol_version_and_a_fresh_id() {
    let request = Request::build(Command::DaemonHealth, Value::Null);
    assert_eq!(request.v, PROTOCOL_VERSION);
    assert_eq!(request.command, Command::DaemonHealth);
    assert_ne!(
        Request::build(Command::DaemonHealth, Value::Null).id,
        request.id,
        "each request must mint its own correlation id"
    );
}

#[test]
fn a_response_echoes_the_id_and_omits_the_unused_half() {
    let id = RequestId::parse("r-1").unwrap();
    let ok = serde_json::to_value(Response::ok(id.clone(), json!({"status": "ok"}))).unwrap();
    assert_eq!(ok["v"], json!(PROTOCOL_VERSION));
    assert_eq!(ok["id"], json!("r-1"));
    assert_eq!(ok["ok"], json!(true));
    assert!(ok.get("error").is_none());

    let failed =
        serde_json::to_value(Response::error(id, "bad_request", "missing command")).unwrap();
    assert_eq!(failed["ok"], json!(false));
    assert!(failed.get("result").is_none());
    assert_eq!(failed["error"]["code"], json!("bad_request"));
}

#[test]
fn a_fault_response_carries_only_static_text() {
    let response = Response::fault(RequestId::unknown(), Fault::InvalidParams);
    let error = response.error.as_ref().expect("error body");
    assert_eq!(error.code, Fault::InvalidParams.code());
    assert_eq!(error.message, Fault::InvalidParams.message());
    assert_eq!(error.audit_class, Some(Fault::InvalidParams.class()));
    assert!(error.data.is_none());
    assert_eq!(response.id.as_str(), "unknown");
}

#[test]
fn the_audit_projection_never_carries_the_handler_message_or_peer_payload() {
    let error = BrokerError::with_data(
        "credential ya29.oauth-access-token was rejected",
        json!({"approval_requests": ["req-1"]}),
    );
    let response = Response::handler_error(RequestId::unknown(), error);
    let facts = response.audit_facts();
    let rendered = serde_json::to_string(&facts).unwrap();
    assert!(!rendered.contains("ya29.oauth-access-token"), "{rendered}");
    assert!(!rendered.contains("approval_requests"), "{rendered}");
}

#[test]
fn handler_error_kinds_have_distinct_stable_codes() {
    let cases = [
        (
            BrokerError::from("provider failed".to_string()),
            "execution_failed",
        ),
        (
            BrokerError::unavailable("provider is offline"),
            "unavailable",
        ),
        (
            BrokerError::authorization_required(
                "approval required",
                json!({"approval_requests": ["req-1"]}),
            ),
            "not_authorized",
        ),
    ];
    for (error, code) in cases {
        let response = Response::handler_error(RequestId::unknown(), error);
        assert_eq!(response.error.expect("error body").code, code);
    }
}

#[test]
fn the_error_body_is_closed_against_unknown_fields() {
    let smuggled = json!({
        "code": "request_failed",
        "message": "no",
        "audit_class": "spoofed",
    });
    assert!(serde_json::from_value::<ErrorBody>(smuggled).is_err());
}

#[test]
fn a_response_encodes_to_the_body_the_transport_frames() {
    let response = Response::ok(RequestId::parse("r-1").unwrap(), json!({"n": 1}));
    let body = encode_response(&response).unwrap();
    let decoded: Response = serde_json::from_slice(&body).unwrap();
    assert_eq!(decoded, response);
}
