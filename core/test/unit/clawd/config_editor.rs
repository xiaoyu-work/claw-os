use super::*;

#[test]
fn sysctl_validator_rejects_missing_assignment() {
    assert!(!validate_sysctl(b"net.ipv4.ip_forward\n").valid);
    assert!(validate_sysctl(b"net.ipv4.ip_forward = 1\n").valid);
}

#[test]
fn validator_selection_is_fail_closed() {
    assert!(matches!(
        validator_for(Path::new("/etc/fstab")).unwrap(),
        Validator::Fstab
    ));
    assert!(validator_for(Path::new("/etc/unknown.conf")).is_err());
}
