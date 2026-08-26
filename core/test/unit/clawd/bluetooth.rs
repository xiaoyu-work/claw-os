use super::*;

#[test]
fn bluetooth_addresses_are_normalized() {
    assert_eq!(
        normalize_address("aa:bb:cc:dd:ee:ff").unwrap(),
        "AA:BB:CC:DD:EE:FF"
    );
    assert!(normalize_address("not-an-address").is_err());
}

#[test]
fn ansi_sequences_are_removed() {
    assert_eq!(
        strip_ansi("\u{1b}[0;91mDevice AA:BB Test\u{1b}[0m"),
        "Device AA:BB Test"
    );
}

#[test]
fn scan_duration_is_bounded() {
    validate_action(
        "scan",
        Some("AA:BB:CC:DD:EE:FF"),
        None,
        None,
        None,
        None,
        Some(10),
    )
    .unwrap();
    assert!(validate_action(
        "scan",
        Some("AA:BB:CC:DD:EE:FF"),
        None,
        None,
        None,
        None,
        Some(61),
    )
    .is_err());
}
