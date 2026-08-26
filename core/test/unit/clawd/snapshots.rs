use super::*;

#[test]
fn snapshot_ids_are_strict() {
    validate_snapshot_id("snap_0123456789abcdef0123456789abcdef").unwrap();
    assert!(validate_snapshot_id("../snapshot").is_err());
    assert!(validate_snapshot_id("snap_not-hex").is_err());
}

#[test]
fn descriptions_strip_controls_and_limit_size() {
    let value = sanitize_description(&format!("hello\n{}", "x".repeat(300))).unwrap();
    assert!(!value.contains('\n'));
    assert_eq!(value.chars().count(), 200);
}

#[test]
fn lvm_paths_and_names_are_restricted() {
    ensure_lvm_snapshot_path("/dev/vg/cos_123").unwrap();
    assert!(ensure_lvm_snapshot_path("/tmp/x").is_err());
    assert!(ensure_lvm_snapshot_path("/dev/vg/../root").is_err());
    assert!(safe_lvm_name("ubuntu-vg"));
    assert!(!safe_lvm_name("../vg"));
}
