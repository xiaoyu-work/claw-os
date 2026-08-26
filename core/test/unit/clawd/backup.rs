use super::*;

#[test]
fn mountinfo_parser_handles_escaped_mountpoints() {
    let info =
        parse_mountinfo("36 25 8:1 / /media/My\\040Disk rw,relatime - ext4 /dev/sdb1 rw")
            .unwrap();
    assert_eq!(info.mountpoint, "/media/My Disk");
    assert_eq!(info.source, "/dev/sdb1");
}

#[test]
fn retention_is_bounded() {
    validate_action(
        "retention",
        None,
        None,
        None,
        None,
        Some(7),
        Some(4),
        Some(12),
        true,
    )
    .unwrap();
    assert!(validate_action(
        "retention",
        None,
        None,
        None,
        None,
        Some(366),
        Some(4),
        Some(12),
        true,
    )
    .is_err());
}
