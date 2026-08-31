use super::*;

#[test]
fn packaged_uid_range_is_below_systemd_dynamic_users() {
    assert!(validate_fixed_range().is_ok());
    assert_eq!(GROUP_GID, 60_999);
    assert_eq!(IDENTITY_COUNT, 64);
    assert!(FIRST_UID + IDENTITY_COUNT - 1 < SYSTEMD_DYNAMIC_UID_MIN);
}

#[test]
fn nss_reverse_lookup_detects_mapped_accounts() {
    let uid = unsafe { libc::geteuid() } as u32;
    assert!(account_by_uid(uid).unwrap().is_some());
}

#[test]
fn manifest_contains_every_exact_identity() {
    let manifest = reservation_manifest(GROUP_GID);
    assert!(manifest.starts_with("version=1\ngroup=cos-extension:60999\n"));
    assert!(manifest.contains("identity=cos-ext-00:61000:60999:/nonexistent:/usr/sbin/nologin\n"));
    assert!(manifest.contains("identity=cos-ext-63:61063:60999:/nonexistent:/usr/sbin/nologin\n"));
    assert_eq!(manifest.lines().count(), 66);
}

#[test]
fn subordinate_id_ranges_are_checked_for_overlap() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("subuid");
    assert!(validate_subid_content(&path, "alice:100000:65536\n").is_ok());
    assert!(validate_subid_content(&path, "alice:60990:20\n")
        .unwrap_err()
        .contains("overlaps"));
    assert!(validate_subid_content(&path, "alice:61063:1\n")
        .unwrap_err()
        .contains("overlaps"));
    assert!(validate_subid_content(&path, "alice:4294967295:2\n")
        .unwrap_err()
        .contains("overflows"));
    assert!(validate_subid_content(&path, ":100000:1\n")
        .unwrap_err()
        .contains("empty owner"));
    let subgid = root.path().join("subgid");
    assert!(validate_subid_content(&subgid, "alice:60999:1\n")
        .unwrap_err()
        .contains("overlaps"));
    assert!(validate_subid_content(&subgid, "alice:60998:1\n").is_ok());
}

#[test]
fn retained_identity_is_not_released_until_cleanup() {
    let lock_dir = tempfile::tempdir().unwrap();
    let lock = std::fs::File::create(lock_dir.path().join("lock")).unwrap();
    let pool = Arc::new(ExtensionIdentityPool {
        identities: Vec::new(),
        in_use: Mutex::new(HashSet::from([FIRST_UID])),
        retained_locks: Mutex::new(HashMap::new()),
        validate_on_acquire: false,
        quarantine_dir: None,
    });
    let mut lease = ExtensionIdentityLease {
        pool: pool.clone(),
        identity: ExtensionIdentity {
            uid: FIRST_UID,
            gid: 999,
            username: "cos-ext-00".to_string(),
        },
        lock: Some(lock),
        release_on_drop: true,
        cleanup_record: None,
    };
    lease.begin_task(1000).unwrap();
    drop(lease);
    assert!(pool.in_use.lock().unwrap().contains(&FIRST_UID));
    assert!(pool.retained_locks.lock().unwrap().contains_key(&FIRST_UID));
}

#[test]
fn cleanup_records_bind_uid_owner_and_task() {
    let record = CleanupRecord {
        uid: FIRST_UID,
        owner_uid: 1000,
        task_name: Some("0123456789abcdef0123456789abcdef".to_string()),
    };
    let text = cleanup_record_text(&record);
    assert_eq!(parse_cleanup_record(&text, FIRST_UID).unwrap(), record);
    assert!(parse_cleanup_record(&text, FIRST_UID + 1).is_err());
    assert!(parse_cleanup_record(
        "version=1\nuid=61000\nowner_uid=1000\ntask_name=../../etc/passwd\n",
        FIRST_UID
    )
    .is_err());
}
