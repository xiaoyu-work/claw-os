use super::*;

#[test]
fn mime_validation_is_strict() {
    validate_action("read", Some("text/plain"), None, false, false).unwrap();
    assert!(validate_action("read", Some("--help"), None, false, false).is_err());
}

#[test]
fn wayland_display_rejects_paths() {
    assert!(valid_wayland_display("wayland-0"));
    assert!(!valid_wayland_display("../wayland-0"));
}
