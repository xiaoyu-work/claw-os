use super::*;

#[test]
fn smart_exit_status_is_decoded() {
    assert_eq!(
        smart_exit_flags((1 << 3) | (1 << 6)),
        vec!["disk-failing", "error-log"]
    );
}

#[test]
fn lsblk_tree_is_flattened_with_mountpoints() {
    let values = parse_lsblk(
        r#"{"blockdevices":[{"path":"/dev/example","name":"/dev/example","kname":"example","type":"disk","mountpoints":[null],"children":[{"path":"/dev/example1","name":"/dev/example1","kname":"example1","type":"part","pkname":"example","fstype":"ext4","mountpoints":["/mnt/example"]}]}]}"#,
    )
    .unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[1]["mountpoints"][0], "/mnt/example");
}

#[test]
fn storage_actions_require_expected_device_shape() {
    assert!(validate_action("status", None).is_ok());
    assert!(validate_action("status", Some("/dev/sda")).is_err());
    assert!(validate_action("mount", Some("/dev/sda1")).is_ok());
    assert!(validate_action("eject", None).is_err());
}
