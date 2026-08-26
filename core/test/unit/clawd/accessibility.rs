use super::*;

#[test]
fn accessibility_actions_are_bounded() {
    validate_action("screen-reader", Some("on")).unwrap();
    validate_action("filter", Some("deuteranopia")).unwrap();
    assert!(validate_action("filter", Some("custom")).is_err());
}

#[test]
fn busctl_booleans_are_parsed() {
    assert_eq!(parse_busctl_bool("b true\n"), Some(true));
    assert_eq!(parse_busctl_bool("b false\n"), Some(false));
}
