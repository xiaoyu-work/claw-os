use super::*;
use serde_json::json;

#[test]
fn a_request_id_is_bounded_and_charset_checked() {
    assert!(RequestId::parse("").is_err());
    assert!(RequestId::parse(&"a".repeat(MAX_REQUEST_ID_BYTES)).is_ok());
    assert!(RequestId::parse(&"a".repeat(MAX_REQUEST_ID_BYTES + 1)).is_err());
    assert!(RequestId::parse("has space").is_err());
    assert!(RequestId::parse("../../etc/passwd").is_err());
    assert_ne!(RequestId::generate(), RequestId::generate());
}

#[test]
fn an_envelope_is_closed() {
    let ok = json!({"v": PROTOCOL_VERSION, "id": "r1", "command": "daemon.health", "params": {}});
    let envelope: InboundRequest = serde_json::from_value(ok).unwrap();
    assert_eq!(envelope.v, PROTOCOL_VERSION);
    assert_eq!(envelope.command.as_str(), "daemon.health");

    // A field the envelope never declared is a decode failure, not a
    // field the daemon ignores.
    let extra = json!({
        "v": PROTOCOL_VERSION,
        "id": "r1",
        "command": "daemon.health",
        "params": {},
        "impersonate_uid": 0,
    });
    assert!(serde_json::from_value::<InboundRequest>(extra).is_err());

    // The pre-v1 shape has no version and carries an untyped id.
    let legacy = json!({"id": 1, "command": "daemon.health", "params": {}});
    assert!(serde_json::from_value::<InboundRequest>(legacy).is_err());
}

#[test]
fn params_default_to_null_but_the_command_is_required() {
    let envelope: InboundRequest =
        serde_json::from_value(json!({"v": PROTOCOL_VERSION, "id": "r1", "command": "task.count"}))
            .unwrap();
    assert!(envelope.params.is_null());
    assert!(
        serde_json::from_value::<InboundRequest>(json!({"v": PROTOCOL_VERSION, "id": "r1"}))
            .is_err()
    );
}

#[test]
fn an_in_repo_request_names_a_route_the_registry_knows() {
    let request = Request::new(crate::clawd::routes::Command::DaemonHealth, Value::Null);
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(encoded["v"], json!(PROTOCOL_VERSION));
    assert_eq!(encoded["command"], json!("daemon.health"));
    let decoded: InboundRequest = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.id, request.id);
}

#[test]
fn a_command_that_does_not_exist_does_not_deserialize() {
    assert!(serde_json::from_value::<Request>(json!({
        "v": PROTOCOL_VERSION,
        "id": "r1",
        "command": "vendor.debug.dump",
        "params": {},
    }))
    .is_err());
}

#[test]
fn every_fault_carries_a_static_class_and_message() {
    let faults = [
        Fault::UnsupportedFrame,
        Fault::FrameTooLarge,
        Fault::TruncatedFrame,
        Fault::MalformedBody,
        Fault::ExtraFrame,
        Fault::InvalidEnvelope,
        Fault::UnsupportedVersion,
        Fault::UnknownCommand,
        Fault::InvalidParams,
        Fault::MissingCredentials,
        Fault::CredentialsChanged,
        Fault::DescriptorPassing,
        Fault::PeerUnverified,
        Fault::ReadTimeout,
        Fault::WriteTimeout,
        Fault::ResponseTooLarge,
        Fault::TooManyConnections,
        Fault::TooManyRequests,
        Fault::RouteBusy,
        Fault::DuplicateRequest,
        Fault::NotAuthorized,
        Fault::RouteTimeout,
    ];
    let mut classes = std::collections::BTreeSet::new();
    for fault in faults {
        assert!(!fault.class().is_empty());
        assert!(!fault.message().is_empty());
        assert!(crate::audit_policy::is_token(fault.class()), "{fault:?}");
        assert!(classes.insert(fault.class()), "duplicate class {fault:?}");
    }
}

#[test]
fn public_failure_categories_are_stable_and_distinct() {
    assert_eq!(Fault::MalformedBody.code(), "invalid_json");
    assert_eq!(Fault::InvalidEnvelope.code(), "invalid_request");
    assert_eq!(Fault::InvalidParams.code(), "invalid_request");
    assert_eq!(Fault::UnknownCommand.code(), "unknown_command");
    assert_eq!(Fault::NotAuthorized.code(), "not_authorized");
    assert_eq!(Fault::RouteBusy.code(), "unavailable");
    assert_eq!(Fault::RouteTimeout.code(), "unavailable");
    assert_eq!(Fault::UnsupportedVersion.code(), "protocol_error");
}

#[test]
fn the_legacy_notice_is_one_json_line_and_names_no_caller_input() {
    let notice = legacy_upgrade_notice();
    assert!(notice.ends_with(b"\n"));
    let parsed: Value = serde_json::from_slice(&notice[..notice.len() - 1]).unwrap();
    assert_eq!(parsed["ok"], json!(false));
    assert_eq!(
        parsed["error"]["message"],
        json!(Fault::UnsupportedFrame.message())
    );
}

#[test]
fn legacy_detection_only_matches_a_json_object_opener() {
    assert!(looks_like_legacy_request(b"{\"command\""));
    assert!(looks_like_legacy_request(b"   {"));
    assert!(!looks_like_legacy_request(&MAGIC));
    assert!(!looks_like_legacy_request(b""));
    assert!(!looks_like_legacy_request(b"GET / HTTP/1.1"));
}
