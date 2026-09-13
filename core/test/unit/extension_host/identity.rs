use super::*;

fn fixture_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(".extension-identity-test-")
        .tempdir_in(".")
        .unwrap()
}

fn unused_fixture_uid() -> u32 {
    (u32::MAX - IDENTITY_COUNT..u32::MAX)
        .find(|uid| !uid_has_process(*uid) && !uid_runtime_exists(*uid))
        .expect("an unused fixture uid")
}

fn fixture_lease(
    pool: Arc<ExtensionIdentityPool>,
    uid: u32,
    lock_path: &Path,
) -> ExtensionIdentityLease {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(pool.in_use.lock().unwrap().insert(uid));
    ExtensionIdentityLease {
        pool,
        identity: ExtensionIdentity {
            uid,
            gid: GROUP_GID,
            username: "fixture-extension".to_string(),
        },
        lock: Some(lock),
        release_on_drop: true,
        cleanup_record: None,
    }
}

fn assert_fixture_lock_held(path: &Path) {
    let file = std::fs::File::open(path).unwrap();
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        -1
    );
    assert!(std::io::Error::last_os_error()
        .raw_os_error()
        .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN));
}

#[test]
fn packaged_uid_range_is_below_systemd_dynamic_users() {
    assert!(validate_fixed_range().is_ok());
    assert_eq!(GROUP_GID, 60_999);
    assert_eq!(IDENTITY_COUNT, 64);
    assert!(FIRST_UID + IDENTITY_COUNT - 1 < SYSTEMD_DYNAMIC_UID_MIN);
}

#[test]
fn task_and_service_identity_pools_are_disjoint() {
    assert_eq!(TASK_IDENTITY_COUNT, 56);
    assert_eq!(SERVICE_IDENTITY_COUNT, 8);
    assert_eq!(TASK_IDENTITY_COUNT + SERVICE_IDENTITY_COUNT, IDENTITY_COUNT);
    for index in 0..IDENTITY_COUNT as usize {
        let task = identity_supports_purpose(index, super::super::protocol::HostPurpose::Task);
        let service =
            identity_supports_purpose(index, super::super::protocol::HostPurpose::AppService);
        assert_ne!(task, service);
        assert_eq!(task, index < TASK_IDENTITY_COUNT as usize);
    }
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
    let retained = reservation_manifest(998);
    assert!(retained.starts_with("version=1\ngroup=cos-extension:998\n"));
    assert!(retained.contains("identity=cos-ext-00:61000:998:"));
}

#[test]
fn subordinate_id_ranges_are_checked_for_overlap() {
    let root = fixture_dir();
    let path = root.path().join("subuid");
    assert!(validate_subid_content(&path, "alice:100000:65536\n", GROUP_GID).is_ok());
    assert!(validate_subid_content(&path, "alice:60990:20\n", GROUP_GID)
        .unwrap_err()
        .contains("overlaps"));
    assert!(validate_subid_content(&path, "alice:61063:1\n", GROUP_GID)
        .unwrap_err()
        .contains("overlaps"));
    assert!(
        validate_subid_content(&path, "alice:4294967295:2\n", GROUP_GID)
            .unwrap_err()
            .contains("overflows")
    );
    assert!(validate_subid_content(&path, ":100000:1\n", GROUP_GID)
        .unwrap_err()
        .contains("empty owner"));
    let subgid = root.path().join("subgid");
    assert!(
        validate_subid_content(&subgid, "alice:60999:1\n", GROUP_GID)
            .unwrap_err()
            .contains("overlaps")
    );
    assert!(validate_subid_content(&subgid, "alice:60998:1\n", GROUP_GID).is_ok());
    assert!(validate_subid_content(&subgid, "alice:998:1\n", 998)
        .unwrap_err()
        .contains("overlaps"));
    assert!(validate_execution_gid(998).is_ok());
}

#[test]
fn retained_identity_is_not_released_until_cleanup() {
    let lock_dir = fixture_dir();
    let lock = std::fs::File::create(lock_dir.path().join("lock")).unwrap();
    let pool = Arc::new(ExtensionIdentityPool {
        identities: Vec::new(),
        in_use: Mutex::new(HashSet::from([FIRST_UID])),
        retiring_owners: Mutex::new(HashMap::new()),
        retained_locks: Mutex::new(HashMap::new()),
        validate_on_acquire: false,
        execution_gid: 999,
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
    assert!(pool.retiring_owners.lock().unwrap().is_empty());
    assert!(pool.require_owner_cleanup(1000).is_err());
    assert!(pool.require_owner_cleanup(1001).is_err());
}

#[test]
fn release_refuses_an_identity_with_a_live_process() {
    let uid = unsafe { libc::geteuid() } as u32;
    let lock_dir = fixture_dir();
    let lock = std::fs::File::create(lock_dir.path().join("lock")).unwrap();
    let pool = Arc::new(ExtensionIdentityPool {
        identities: Vec::new(),
        in_use: Mutex::new(HashSet::from([uid])),
        retiring_owners: Mutex::new(HashMap::new()),
        retained_locks: Mutex::new(HashMap::new()),
        validate_on_acquire: false,
        execution_gid: GROUP_GID,
        quarantine_dir: None,
    });
    let lease = ExtensionIdentityLease {
        pool: pool.clone(),
        identity: ExtensionIdentity {
            uid,
            gid: GROUP_GID,
            username: "current-process".to_string(),
        },
        lock: Some(lock),
        release_on_drop: true,
        cleanup_record: None,
    };

    assert!(lease
        .release()
        .unwrap_err()
        .contains("still owns a process"));
    assert!(pool.in_use.lock().unwrap().contains(&uid));
    assert!(pool.retained_locks.lock().unwrap().contains_key(&uid));
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

#[test]
fn host_quarantine_fences_its_owner_and_unknown_records() {
    let record = CleanupRecord {
        uid: FIRST_UID + TASK_IDENTITY_COUNT,
        owner_uid: 1000,
        task_name: Some("a".repeat(32)),
    };
    assert!(cleanup_blocks_owner(Some(&record), 1000));
    assert!(!cleanup_blocks_owner(Some(&record), 1001));
    assert!(cleanup_blocks_owner(None, 1000));
}

#[test]
fn a_failed_host_cannot_switch_purpose_or_uid_to_reopen_persistent_data() {
    let root = fixture_dir();
    let lock = std::fs::File::create(root.path().join("retained")).unwrap();
    let pool = Arc::new(ExtensionIdentityPool {
        identities: Vec::new(),
        in_use: Mutex::new(HashSet::new()),
        retiring_owners: Mutex::new(HashMap::new()),
        retained_locks: Mutex::new(HashMap::from([(FIRST_UID + TASK_IDENTITY_COUNT, lock)])),
        validate_on_acquire: false,
        execution_gid: GROUP_GID,
        quarantine_dir: None,
    });
    let service = super::super::protocol::HostPurpose::AppService;
    assert!(pool.acquire(1000, service).unwrap_err().contains("cleanup is unconfirmed"));
    assert!(pool.acquire(1000, super::super::protocol::HostPurpose::Task)
        .unwrap_err().contains("cleanup is unconfirmed"));
    let mut retained = pool.retained_locks.lock().unwrap();
    let lock = retained.remove(&(FIRST_UID + TASK_IDENTITY_COUNT)).unwrap();
    retained.insert(FIRST_UID, lock);
    drop(retained);
    for purpose in [service, super::super::protocol::HostPurpose::Task] {
        assert!(pool.acquire(1000, purpose).unwrap_err().contains("cleanup is unconfirmed"));
    }
}

#[test]
fn held_retirement_fences_only_its_owner_across_host_purposes() {
    let root = fixture_dir();
    let pool = ExtensionIdentityPool::for_test(GROUP_GID);
    let uid = unused_fixture_uid();
    let lock_path = root.path().join("lock");
    let mut lease = fixture_lease(pool.clone(), uid, &lock_path);
    lease.begin_task(1000).unwrap();
    pool.require_owner_cleanup(1000).unwrap();

    lease.begin_retirement().unwrap();
    lease.begin_retirement().unwrap();
    assert_eq!(
        *pool.retiring_owners.lock().unwrap(),
        HashMap::from([(uid, 1000)])
    );
    assert!(pool.in_use.lock().unwrap().contains(&uid));
    assert!(pool.retained_locks.lock().unwrap().is_empty());
    assert_fixture_lock_held(&lock_path);
    for purpose in [
        super::super::protocol::HostPurpose::Task,
        super::super::protocol::HostPurpose::AppService,
    ] {
        let error = pool.acquire(1000, purpose).unwrap_err();
        assert!(error.contains("cleanup is unconfirmed for owner 1000"));
        assert!(error.contains(&format!("execution uid {uid}")));
    }
    pool.require_owner_cleanup(1001).unwrap();

    lease.release_checked().unwrap();
    pool.require_owner_cleanup(1000).unwrap();
    assert!(pool.retiring_owners.lock().unwrap().is_empty());
}

#[test]
fn retiring_owner_stays_fenced_until_all_its_leases_are_released() {
    let root = fixture_dir();
    let pool = ExtensionIdentityPool::from_identities(Vec::new(), false, None);
    let uid = unused_fixture_uid();
    let second_uid = (u32::MAX - IDENTITY_COUNT..u32::MAX)
        .find(|other| *other != uid && !uid_has_process(*other) && !uid_runtime_exists(*other))
        .unwrap();
    let mut first = fixture_lease(pool.clone(), uid, &root.path().join("first"));
    let mut second = fixture_lease(pool.clone(), second_uid, &root.path().join("second"));
    first.begin_task(1000).unwrap();
    second.begin_task(1000).unwrap();
    first.begin_retirement().unwrap();
    second.begin_retirement().unwrap();
    first.release_checked().unwrap();

    assert_eq!(
        *pool.retiring_owners.lock().unwrap(),
        HashMap::from([(second_uid, 1000)])
    );
    assert!(pool.require_owner_cleanup(1000).is_err());
    pool.require_owner_cleanup(1001).unwrap();
    second.release_checked().unwrap();
    pool.require_owner_cleanup(1000).unwrap();
    assert!(pool.retiring_owners.lock().unwrap().is_empty());
}

#[test]
fn retirement_requires_an_active_matching_cleanup_record() {
    let root = fixture_dir();
    let pool = ExtensionIdentityPool::from_identities(Vec::new(), false, None);
    let uid = unused_fixture_uid();
    let mut lease = fixture_lease(pool.clone(), uid, &root.path().join("lock"));
    assert!(lease
        .begin_retirement()
        .unwrap_err()
        .contains("record was not started"));
    assert!(pool.retiring_owners.lock().unwrap().is_empty());

    lease.begin_task(1000).unwrap();
    lease.cleanup_record.as_mut().unwrap().uid = uid - 1;
    assert!(lease
        .begin_retirement()
        .unwrap_err()
        .contains("cleanup uid changed"));
    assert!(pool.retiring_owners.lock().unwrap().is_empty());
    lease.cleanup_record.as_mut().unwrap().uid = uid;
    lease.begin_retirement().unwrap();
    lease.release_checked().unwrap();
    lease.release_checked().unwrap();

    assert!(lease
        .begin_retirement()
        .unwrap_err()
        .contains("already been released"));
    assert!(lease
        .begin_task(1001)
        .unwrap_err()
        .contains("already been released"));
    assert!(pool.retiring_owners.lock().unwrap().is_empty());
    assert!(pool.in_use.lock().unwrap().is_empty());
    assert!(lease.cleanup_record.is_none());
}

#[test]
fn checked_release_retries_parent_sync_with_an_absent_marker() {
    let root = fixture_dir();
    let uid = unused_fixture_uid();
    let lock_path = root.path().join("lock");
    let mut lease = fixture_lease(
        ExtensionIdentityPool::from_identities(Vec::new(), false, None),
        uid,
        &lock_path,
    );
    lease.begin_task(1000).unwrap();

    // Model an unlinked marker whose parent-directory durability is still unconfirmed.
    let backing = root.path().join("backing");
    let directory = root.path().join("quarantine");
    std::fs::create_dir(&backing).unwrap();
    std::os::unix::fs::symlink("backing", &directory).unwrap();
    Arc::get_mut(&mut lease.pool).unwrap().quarantine_dir = Some(directory.clone());
    let pool = lease.pool.clone();
    lease.begin_retirement().unwrap();
    let record = lease.cleanup_record.clone();
    let descriptor = lease.lock.as_ref().unwrap().as_raw_fd();

    assert!(lease
        .release_checked()
        .unwrap_err()
        .contains("open extension quarantine directory for sync"));
    assert_eq!(lease.lock.as_ref().unwrap().as_raw_fd(), descriptor);
    assert_eq!(lease.cleanup_record, record);
    assert!(pool.in_use.lock().unwrap().contains(&uid));
    assert_eq!(pool.retiring_owners.lock().unwrap().get(&uid), Some(&1000));
    assert!(pool.retained_locks.lock().unwrap().is_empty());
    assert_fixture_lock_held(&lock_path);
    for purpose in [
        super::super::protocol::HostPurpose::Task,
        super::super::protocol::HostPurpose::AppService,
    ] {
        assert!(pool
            .acquire(1000, purpose)
            .unwrap_err()
            .contains("cleanup is unconfirmed"));
    }
    pool.require_owner_cleanup(1001).unwrap();

    std::fs::remove_file(&directory).unwrap();
    std::fs::rename(&backing, &directory).unwrap();
    lease.release_checked().unwrap();
    assert!(lease.lock.is_none());
    assert!(lease.cleanup_record.is_none());
    assert!(pool.in_use.lock().unwrap().is_empty());
    assert!(pool.retiring_owners.lock().unwrap().is_empty());
    pool.require_owner_cleanup(1000).unwrap();
}

#[test]
fn repeated_checked_release_cannot_release_a_replacement_identity() {
    let root = fixture_dir();
    let pool = ExtensionIdentityPool::from_identities(Vec::new(), false, None);
    let uid = unused_fixture_uid();
    let lock_path = root.path().join("lock");
    let mut lease = fixture_lease(pool.clone(), uid, &lock_path);
    lease.begin_task(1000).unwrap();
    lease.begin_retirement().unwrap();
    lease.release_checked().unwrap();

    let mut replacement = fixture_lease(pool.clone(), uid, &lock_path);
    replacement.begin_task(1001).unwrap();
    replacement.begin_retirement().unwrap();
    lease.release_checked().unwrap();
    assert!(lease.begin_retirement().is_err());
    lease.release().unwrap();

    assert!(pool.in_use.lock().unwrap().contains(&uid));
    assert_eq!(pool.retiring_owners.lock().unwrap().get(&uid), Some(&1001));
    assert!(pool.retained_locks.lock().unwrap().is_empty());
    assert_fixture_lock_held(&lock_path);
    assert!(pool.require_owner_cleanup(1001).is_err());
    pool.require_owner_cleanup(1000).unwrap();
    replacement.release_checked().unwrap();
}

#[test]
fn retiring_lease_drop_retains_its_owner_fence_and_uid_lock() {
    let root = fixture_dir();
    let pool = ExtensionIdentityPool::from_identities(Vec::new(), false, None);
    let uid = unused_fixture_uid();
    let lock_path = root.path().join("lock");
    let mut lease = fixture_lease(pool.clone(), uid, &lock_path);
    lease.begin_task(1000).unwrap();
    lease.begin_retirement().unwrap();
    drop(lease);

    assert!(pool.in_use.lock().unwrap().contains(&uid));
    assert!(pool.retained_locks.lock().unwrap().contains_key(&uid));
    assert_eq!(pool.retiring_owners.lock().unwrap().get(&uid), Some(&1000));
    assert_fixture_lock_held(&lock_path);
    for purpose in [
        super::super::protocol::HostPurpose::Task,
        super::super::protocol::HostPurpose::AppService,
    ] {
        assert!(pool
            .acquire(1000, purpose)
            .unwrap_err()
            .contains("cleanup is unconfirmed"));
    }
    pool.require_owner_cleanup(1001).unwrap();
}

#[test]
fn checked_release_of_a_live_identity_keeps_custody_and_owner_fence() {
    let root = fixture_dir();
    let pool = ExtensionIdentityPool::from_identities(Vec::new(), false, None);
    let uid = unsafe { libc::geteuid() } as u32;
    let lock_path = root.path().join("lock");
    let mut lease = fixture_lease(pool.clone(), uid, &lock_path);
    lease.begin_task(1000).unwrap();

    assert!(lease
        .release_checked()
        .unwrap_err()
        .contains("still owns a process"));
    assert!(lease.lock.is_some());
    assert!(lease.cleanup_record.is_some());
    assert!(pool.in_use.lock().unwrap().contains(&uid));
    assert_eq!(pool.retiring_owners.lock().unwrap().get(&uid), Some(&1000));
    assert!(pool.retained_locks.lock().unwrap().is_empty());
    assert_fixture_lock_held(&lock_path);
    assert!(pool.require_owner_cleanup(1000).is_err());
    pool.require_owner_cleanup(1001).unwrap();

    drop(lease);
    assert!(pool.retained_locks.lock().unwrap().contains_key(&uid));
    assert!(pool.require_owner_cleanup(1000).is_err());
    assert_fixture_lock_held(&lock_path);
}

#[test]
fn checked_release_surfaces_poisoned_locks_without_surrendering_custody() {
    for poison_owners in [false, true] {
        let root = fixture_dir();
        let pool = ExtensionIdentityPool::from_identities(Vec::new(), false, None);
        let uid = unused_fixture_uid();
        let lock_path = root.path().join("lock");
        let mut lease = fixture_lease(pool.clone(), uid, &lock_path);
        lease.begin_task(1000).unwrap();
        lease.begin_retirement().unwrap();
        assert!(std::panic::catch_unwind(|| {
            if poison_owners {
                let _guard = pool.retiring_owners.lock().unwrap();
                panic!("poison owner tracking");
            } else {
                let _guard = pool.in_use.lock().unwrap();
                panic!("poison identity tracking");
            }
        })
        .is_err());
        let expected = if poison_owners {
            "extension retiring owners are poisoned"
        } else {
            "extension identity pool is poisoned"
        };
        assert_eq!(lease.begin_retirement().unwrap_err(), expected);
        assert_eq!(lease.release_checked().unwrap_err(), expected);
        assert!(lease.lock.is_some());
        assert!(lease.cleanup_record.is_some());
        assert!(pool.retained_locks.lock().unwrap().is_empty());
        assert_fixture_lock_held(&lock_path);

        pool.retiring_owners.clear_poison();
        pool.in_use.clear_poison();
        lease.release_checked().unwrap();
        assert!(pool.retiring_owners.lock().unwrap().is_empty());
        assert!(pool.in_use.lock().unwrap().is_empty());
    }
}

#[test]
#[ignore = "requires Root for authenticated durable cleanup fixtures"]
fn checked_release_retries_authenticated_marker_cleanup() {
    use std::os::unix::fs::PermissionsExt;

    assert_eq!(unsafe { libc::geteuid() }, 0);
    let root = fixture_dir();
    let directory = root.path().join("quarantine");
    let pool = ExtensionIdentityPool::from_identities(Vec::new(), false, Some(directory.clone()));
    let uid = unused_fixture_uid();
    let lock_path = root.path().join("lock");
    let mut lease = fixture_lease(pool.clone(), uid, &lock_path);
    lease.begin_task(1000).unwrap();
    lease.record_task(1000, &"a".repeat(32)).unwrap();
    let record = read_cleanup_record(&directory, uid).unwrap().unwrap();
    lease.begin_retirement().unwrap();
    lease.begin_retirement().unwrap();
    assert_eq!(read_cleanup_record(&directory, uid).unwrap(), Some(record));

    let marker = marker_path(&directory, uid);
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(lease
        .release_checked()
        .unwrap_err()
        .contains("unsafe ownership or mode"));
    assert!(marker.exists());
    assert!(lease.lock.is_some());
    assert!(pool.retained_locks.lock().unwrap().is_empty());
    assert!(pool.require_owner_cleanup(1000).is_err());
    assert_fixture_lock_held(&lock_path);
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    lease.release_checked().unwrap();
    assert!(!marker.exists());

    let mut replacement = fixture_lease(pool.clone(), uid, &lock_path);
    replacement.begin_task(1001).unwrap();
    lease.release_checked().unwrap();
    assert_eq!(
        read_cleanup_record(&directory, uid).unwrap(),
        replacement.cleanup_record
    );
    assert!(pool.in_use.lock().unwrap().contains(&uid));
    assert_fixture_lock_held(&lock_path);
    replacement.release_checked().unwrap();
    assert!(!marker.exists());
}
