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
