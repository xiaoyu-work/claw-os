use super::*;

#[test]
fn desktop_action_validation_is_strict() {
    validate_action("focus", Some("window-1"), None).unwrap();
    validate_action("restart", Some("window-1"), Some("com.example.App")).unwrap();
    assert!(validate_action("restart", Some("window-1"), Some("*")).is_err());
    assert!(validate_action("list", Some("window-1"), None).is_err());
}

#[test]
fn display_validation_rejects_paths_and_options() {
    assert!(valid_wayland_display("wayland-0"));
    assert!(!valid_wayland_display("../wayland-0"));
    assert!(valid_display(":0"));
    assert!(!valid_display("-display"));
}
