use super::*;

#[test]
fn protocol_refusals_advertise_current_version() {
    let response = protocol_error(
        ErrorCode::IncompatibleProtocolVersion,
        "incompatible protocol",
    );
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    assert_eq!(
        response.headers()[PROTOCOL_VERSION_HEADER],
        CURRENT_PROTOCOL_VERSION_HEADER_VALUE
    );
}

#[test]
fn constant_time_comparison_preserves_auth_contract() {
    assert!(constant_time_eq(b"same", b"same"));
    assert!(!constant_time_eq(b"same", b"diff"));
    assert!(!constant_time_eq(b"short", b"longer"));
}
