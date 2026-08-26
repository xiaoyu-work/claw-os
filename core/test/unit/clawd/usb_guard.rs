use super::*;

#[test]
fn usb_sysfs_names_are_strict() {
    assert!(valid_device_name("1-2.3"));
    assert!(!valid_device_name("usb1"));
    assert!(!valid_device_name("../1-2"));
}

#[test]
fn serials_are_safe_for_udev_rules() {
    validate_serial("ABC-123").unwrap();
    assert!(validate_serial("bad\"serial").is_err());
}
