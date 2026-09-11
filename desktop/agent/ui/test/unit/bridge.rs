use super::*;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn endpoint(minimum: u16, current: u16) -> BridgeEndpoint {
    BridgeEndpoint {
        port: 43123,
        token: TOKEN.into(),
        min_protocol_version: ProtocolVersion(minimum),
        protocol_version: ProtocolVersion(current),
    }
}

#[test]
fn legacy_discovery_fixture_requires_one_restart_cycle() {
    let state = decode_bridge_discovery(
        br#"{"port":43123,"token":"0123456789abcdef0123456789abcdef"}"#,
    )
    .unwrap();
    assert!(matches!(state, DiscoveryState::UpgradeRequired));
    assert_eq!(
        service_action(&state, HealthStatus::NegotiationFailed),
        Some(ServiceAction::Restart)
    );
}

#[test]
fn old_health_fixture_without_echo_requires_restart() {
    let selected = ProtocolVersion(1);
    let old_health_headers = HeaderMap::new();
    assert!(validate_response_protocol_headers(&old_health_headers, selected).is_err());

    let state = DiscoveryState::Ready(endpoint(1, 1));
    assert_eq!(
        service_action(&state, HealthStatus::NegotiationFailed),
        Some(ServiceAction::Restart)
    );
}

#[test]
fn future_bridge_negotiates_highest_client_overlap() {
    let state = decode_bridge_discovery(
        br#"{"port":43123,"token":"0123456789abcdef0123456789abcdef","protocol_version":2,"min_protocol_version":1}"#,
    )
    .unwrap();
    let DiscoveryState::Ready(future) = state else {
        panic!("future bridge with a v1 overlap must be usable");
    };
    assert_eq!(
        selected_protocol_version(&future).unwrap(),
        ProtocolVersion(1)
    );

    let mut echoed = HeaderMap::new();
    echoed.insert(PROTOCOL_VERSION_HEADER, "1".parse().unwrap());
    assert!(validate_response_protocol_headers(&echoed, ProtocolVersion(1)).is_ok());
    assert!(validate_response_protocol_headers(&echoed, ProtocolVersion(2)).is_err());
}

#[test]
fn discovery_without_overlap_requires_upgrade() {
    let state = decode_bridge_discovery(
        br#"{"port":43123,"token":"0123456789abcdef0123456789abcdef","protocol_version":3,"min_protocol_version":2}"#,
    )
    .unwrap();
    assert!(matches!(state, DiscoveryState::UpgradeRequired));
    assert_eq!(
        service_action(&state, HealthStatus::NegotiationFailed),
        Some(ServiceAction::Restart)
    );
    assert!(selected_protocol_version(&endpoint(2, 3)).is_err());
}

#[test]
fn healthy_manual_bridge_is_left_running() {
    let state = DiscoveryState::Ready(endpoint(1, 1));
    assert_eq!(service_action(&state, HealthStatus::Healthy), None);
    assert_eq!(
        service_action(&state, HealthStatus::Unavailable),
        Some(ServiceAction::Start)
    );
}

#[test]
fn upgrade_restart_can_only_be_claimed_once() {
    let attempted = AtomicBool::new(false);
    assert!(claim_upgrade_restart(&attempted));
    assert!(!claim_upgrade_restart(&attempted));
}

#[test]
fn activity_requests_authenticate_negotiate_and_escape_resource_identity() {
    let endpoint = endpoint(1, 2);
    let (request, selected) = activity_request(
        &endpoint,
        reqwest::Method::POST,
        &["activities", "id/with?query#fragment", "run"],
    ).unwrap();
    let request = request.json(&ActivityRunRequest {
        prompt: Some("Continue".into()),
        session_id: Some("session-1".into()),
        ..ActivityRunRequest::default()
    }).build().unwrap();
    assert_eq!(selected, ProtocolVersion(1));
    assert_eq!(request.headers()[PROTOCOL_VERSION_HEADER], "1");
    assert_eq!(request.headers()["authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(request.url().host_str(), Some("127.0.0.1"));
    assert_eq!(request.url().path_segments().unwrap().count(), 4);
    assert!(request.url().query().is_none());
    assert!(request.url().fragment().is_none());
    let body: ActivityRunRequest =
        serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
    assert_eq!(body.session_id.as_deref(), Some("session-1"));
}

#[test]
fn activity_object_transport_uses_the_versioned_endpoint_and_typed_components() {
    let endpoint = endpoint(1, 1);
    let body = ActivityObjectAttachRequest {
        label: "Release status".into(),
        object: cos_agent_protocol::AppObjectReference {
            app_id: "kv".into(),
            object_type: "entry".into(),
            object_id: " a/b?x=1&y=2 ".into(),
            revision: Some("v1/#? ".into()),
        },
    };
    let (request, selected) = activity_request(
        &endpoint,
        reqwest::Method::POST,
        &["activities", "activity-1", "objects"],
    ).unwrap();
    let request = request.json(&body).build().unwrap();
    assert_eq!(selected, ProtocolVersion(1));
    assert_eq!(request.headers()[PROTOCOL_VERSION_HEADER], "1");
    assert!(request.headers().contains_key(reqwest::header::AUTHORIZATION));
    assert_eq!(request.url().path(), "/api/activities/activity-1/objects");
    let sent: ActivityObjectAttachRequest =
        serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
    assert_eq!(sent, body);
}
