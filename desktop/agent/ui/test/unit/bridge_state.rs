use super::*;
use cos_agent_protocol::ProtocolVersion;

fn endpoint() -> BridgeEndpoint {
    BridgeEndpoint {
        port: 43123,
        token: "0123456789abcdef0123456789abcdef".into(),
        protocol_version: ProtocolVersion(1),
        min_protocol_version: ProtocolVersion(1),
    }
}

#[test]
fn bridge_failure_and_reconnect_are_explicit_transitions() {
    let mut state = BridgeState::connecting();
    state.connection_failed("offline".into());
    assert_eq!(state.error(), Some("offline"));
    assert!(state.endpoint().is_none());
    assert!(state.begin_connect());
    assert!(!state.begin_connect());

    state.connected(endpoint());
    assert!(!state.is_connecting());
    assert!(state.endpoint().is_some());
    assert!(state.error().is_none());
}
