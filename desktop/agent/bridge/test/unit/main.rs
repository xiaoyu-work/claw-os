use super::*;
use axum::{
    body::Body,
    http::Request as HttpRequest,
    routing::get,
};
use tower::ServiceExt as _;

#[test]
fn protocol_refusals_advertise_current_version() {
    let response = protocol_error(
        ProtocolMetadata::CURRENT,
        ErrorCode::IncompatibleProtocolVersion,
        "incompatible protocol",
    );
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    assert_eq!(
        response.headers()[PROTOCOL_VERSION_HEADER],
        cos_agent_protocol::CURRENT_PROTOCOL_VERSION_HEADER_VALUE
    );
    assert_eq!(
        response.headers()[PROTOCOL_MIN_VERSION_HEADER],
        cos_agent_protocol::MIN_SUPPORTED_PROTOCOL_VERSION_HEADER_VALUE
    );
}

#[test]
fn selected_version_must_be_inside_bridge_range() {
    let future_bridge = ProtocolMetadata {
        min_protocol_version: ProtocolVersion(1),
        protocol_version: ProtocolVersion(2),
    };
    assert!(validate_selected_version(ProtocolVersion(1), future_bridge));
    assert!(validate_selected_version(ProtocolVersion(2), future_bridge));
    assert!(!validate_selected_version(ProtocolVersion(3), future_bridge));

    let v2_only_bridge = ProtocolMetadata {
        min_protocol_version: ProtocolVersion(2),
        protocol_version: ProtocolVersion(2),
    };
    assert!(!validate_selected_version(ProtocolVersion(1), v2_only_bridge));

    let refusal = protocol_error(
        v2_only_bridge,
        ErrorCode::IncompatibleProtocolVersion,
        "supported range is 2..=2",
    );
    assert_eq!(refusal.status(), StatusCode::UPGRADE_REQUIRED);
    assert_eq!(refusal.headers()[PROTOCOL_MIN_VERSION_HEADER], "2");
    assert_eq!(refusal.headers()[PROTOCOL_VERSION_HEADER], "2");
}

#[tokio::test]
async fn selected_version_is_echoed_on_success_and_api_error() {
    async fn success() -> &'static str {
        "ok"
    }
    async fn failure() -> Result<&'static str, crate::api_error::ApiError> {
        Err(crate::api_error::ApiError::bad_gateway("upstream failed"))
    }

    let app = Router::new()
        .route("/success", get(success))
        .route("/error", get(failure))
        .route_layer(middleware::from_fn(require_protocol_version));

    for path in ["/success", "/error"] {
        let request = HttpRequest::builder()
            .uri(path)
            .header(PROTOCOL_VERSION_HEADER, "1")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.headers()[PROTOCOL_VERSION_HEADER], "1");
    }
}

#[test]
fn constant_time_comparison_preserves_auth_contract() {
    assert!(constant_time_eq(b"same", b"same"));
    assert!(!constant_time_eq(b"same", b"diff"));
    assert!(!constant_time_eq(b"short", b"longer"));
}
