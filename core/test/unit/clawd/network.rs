use super::*;

#[test]
fn terse_parser_handles_escaped_colons() {
    assert_eq!(
        split_terse(r"*:Cafe\:Guest:aa\:bb:11:54 Mbit/s:70:WPA2"),
        vec!["*", "Cafe:Guest", "aa:bb", "11", "54 Mbit/s", "70", "WPA2"]
    );
}

#[test]
fn action_validation_is_strict() {
    validate_action("wifi-connect", Some("Cafe"), None, Some("default/wifi_psk")).unwrap();
    validate_action("airplane", None, Some("on"), None).unwrap();
    assert!(validate_action("wifi-toggle", None, Some("maybe"), None).is_err());
    assert!(validate_action("vpn-up", Some("--help"), None, None).is_err());
}

#[test]
fn connection_categories_are_disjoint() {
    assert!(connection_type_matches("802-11-wireless", "wifi"));
    assert!(connection_type_matches("wifi", "wifi"));
    assert!(!connection_type_matches("802-3-ethernet", "wifi"));
    assert!(connection_type_matches("vpn", "vpn"));
    assert!(connection_type_matches("wireguard", "vpn"));
    assert!(!connection_type_matches("802-11-wireless", "vpn"));
}
