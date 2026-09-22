use super::*;

#[test]
fn workspace_is_canonical_and_bounded_to_the_verified_owner_home() {
    let uid = unsafe { libc::geteuid() } as u32;
    if uid == 0 {
        return;
    }
    let home = crate::paths::verified_home_for_uid(uid).unwrap();
    assert_eq!(resolve(uid, None).unwrap(), home);
    assert!(resolve(uid, Some("")).is_err());
    assert!(resolve(uid, Some("/")).is_err());
}
