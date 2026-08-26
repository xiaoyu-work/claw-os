use super::*;

#[test]
fn confirmation_is_required_for_power_actions() {
    assert!(validate_action("reboot", false).is_err());
    validate_action("reboot", true).unwrap();
    assert!(validate_action("status", true).is_err());
}

#[test]
fn upower_percentages_are_numeric() {
    assert_eq!(parse_upower_value("72.5%"), json!(72.5));
    assert_eq!(parse_upower_value("yes"), json!(true));
}
