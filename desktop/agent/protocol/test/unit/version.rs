use super::*;

#[test]
fn compatibility_range_is_explicit_and_closed() {
    assert_eq!(
        CURRENT_PROTOCOL_VERSION_HEADER_VALUE.parse::<u16>().unwrap(),
        CURRENT_PROTOCOL_VERSION
    );
    assert!(ProtocolVersion::CURRENT.is_supported());
    assert!(!ProtocolVersion(MIN_SUPPORTED_PROTOCOL_VERSION - 1).is_supported());
    assert!(!ProtocolVersion(CURRENT_PROTOCOL_VERSION + 1).is_supported());
}

#[test]
fn metadata_round_trips() {
    let json = serde_json::to_string(&ProtocolMetadata::CURRENT).unwrap();
    assert_eq!(
        serde_json::from_str::<ProtocolMetadata>(&json).unwrap(),
        ProtocolMetadata::CURRENT
    );
}

#[test]
fn negotiation_selects_highest_overlap() {
    let v1_client = ProtocolMetadata::CURRENT;
    let future_bridge = ProtocolMetadata {
        min_protocol_version: ProtocolVersion(1),
        protocol_version: ProtocolVersion(2),
    };
    assert_eq!(
        v1_client.negotiate_highest(future_bridge),
        Some(ProtocolVersion(1))
    );

    let v2_only_bridge = ProtocolMetadata {
        min_protocol_version: ProtocolVersion(2),
        protocol_version: ProtocolVersion(3),
    };
    assert_eq!(v1_client.negotiate_highest(v2_only_bridge), None);
    assert!(!ProtocolMetadata {
        min_protocol_version: ProtocolVersion(3),
        protocol_version: ProtocolVersion(2),
    }
    .is_valid());
}
