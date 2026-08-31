use super::*;

#[test]
fn packaged_uid_range_is_outside_normal_login_allocation() {
    assert!(DEFAULT_UID_MIN > 60_000);
    assert_eq!(DEFAULT_UID_COUNT, 64);
    assert!(DEFAULT_UID_MIN.checked_add(DEFAULT_UID_COUNT - 1).is_some());
}

#[test]
fn mapped_host_accounts_are_detected() {
    let uid = unsafe { libc::geteuid() } as u32;
    assert!(passwd_name(uid).unwrap().is_some());
}

#[test]
fn retained_identity_is_not_released_until_cleanup() {
    let lock_dir = tempfile::tempdir().unwrap();
    let lock = std::fs::File::create(lock_dir.path().join("lock")).unwrap();
    let pool = Arc::new(ExtensionIdentityPool {
        identities: Vec::new(),
        in_use: Mutex::new(HashSet::from([61_184])),
        retained_locks: Mutex::new(HashMap::new()),
    });
    let mut lease = ExtensionIdentityLease {
        pool: pool.clone(),
        identity: ExtensionIdentity {
            uid: 61_184,
            gid: 61_183,
            username: "cos-extension-61184".to_string(),
        },
        lock: Some(lock),
        release_on_drop: true,
    };
    lease.retain_until_cleanup();
    drop(lease);
    assert!(pool.in_use.lock().unwrap().contains(&61_184));
    assert!(pool.retained_locks.lock().unwrap().contains_key(&61_184));
}
