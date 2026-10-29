use super::*;

fn client(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: None,
        uid: Some(uid),
        gid: Some(uid),
        execution_uid: None,
        start_time_ticks: None,
        attended_local: true,
        extension_host: None,
    }
}

#[cfg(unix)]
#[test]
fn legacy_memory_reads_do_not_create_a_database() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let uid = unsafe { libc::geteuid() };
    let peer = client(uid);
    assert_eq!(sessions(json!({}), &peer).unwrap()["n"], 0);
    assert_eq!(
        history(json!({ "session_id": "missing" }), &peer).unwrap()["n"],
        0
    );
    let path = if uid == 0 {
        crate::paths::agent_memory_db_path()
    } else {
        crate::paths::clawd_user_memory_db_path(uid)
    };
    assert!(!path.exists());
    assert!(!path.with_extension("db.lifecycle.lock").exists());
}

#[cfg(unix)]
#[test]
fn legacy_memory_reads_preserve_existing_messages() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let uid = unsafe { libc::geteuid() };
    let path = if uid == 0 {
        crate::paths::agent_memory_db_path()
    } else {
        crate::storage::ensure_owner_agent_state_dir(uid, unsafe { libc::getegid() }).unwrap();
        crate::paths::clawd_user_memory_db_path(uid)
    };
    let db = MemoryDb::open(&path).unwrap();
    db.record_message("legacy", "user", "preserved message")
        .unwrap();
    let peer = client(uid);
    assert_eq!(sessions(json!({}), &peer).unwrap()["n"], 1);
    let response = history(json!({ "session_id": "legacy" }), &peer).unwrap();
    assert_eq!(response["messages"][0]["text"], "preserved message");
    assert_eq!(db.sessions(10).unwrap()[0].message_count, 1);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires root to verify the broker's owner filesystem identity"]
fn root_legacy_reads_never_create_or_transfer_owner_state() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    assert_eq!(unsafe { libc::geteuid() }, 0, "root fixture required");
    let uid: u32 = std::env::var("SUDO_UID").unwrap().parse().unwrap();
    assert_ne!(uid, 0);
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o711)).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("state"));
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", root.path().join("logs"));
    let (gid, _) = crate::clawd::client_identity::owner_groups(uid).unwrap();
    crate::storage::ensure_owner_agent_state_dir(uid, gid).unwrap();
    crate::storage::harden_clawd_state().unwrap();
    let path = crate::paths::clawd_user_memory_db_path(uid);
    assert_eq!(sessions(json!({}), &client(uid)).unwrap()["n"], 0);
    assert!(!path.exists());
    run_as_owner(uid, |path| {
        MemoryDb::open(path)
            .map_err(|error| error.to_string())?
            .record_message("legacy", "user", "preserved owner history")
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().uid(), uid);

    let response = history(json!({ "session_id": "legacy" }), &client(uid)).unwrap();
    assert_eq!(response["messages"][0]["text"], "preserved owner history");
    assert_eq!(std::fs::metadata(&path).unwrap().uid(), uid);
    let original = std::fs::read(&path).unwrap();
    std::os::unix::fs::chown(&path, Some(0), Some(0)).unwrap();
    let error = history(json!({ "session_id": "legacy" }), &client(uid)).unwrap_err();
    assert!(error.contains("owned by uid 0"), "{error}");
    assert!(error.contains("Existing files were not changed"), "{error}");
    assert_eq!(std::fs::metadata(&path).unwrap().uid(), 0);
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let legacy_writer = MemoryDb::open(&path).unwrap();
    legacy_writer
        .record_message("legacy", "assistant", "preserved WAL history")
        .unwrap();
    let agent_dir = path.parent().unwrap();
    let files = [
        "memory.db",
        "memory.db-wal",
        "memory.db-shm",
        "memory.db.lifecycle.lock",
    ]
    .map(|name| agent_dir.join(name));
    let before = files
        .iter()
        .map(|file| {
            std::os::unix::fs::chown(file, Some(0), Some(0)).unwrap();
            (file.clone(), std::fs::read(file).unwrap())
        })
        .collect::<Vec<_>>();
    assert!(std::fs::metadata(&files[1]).unwrap().len() > 32);
    crate::storage::harden_clawd_state().unwrap();
    for (file, contents) in &before {
        let metadata = std::fs::metadata(file).unwrap();
        assert_eq!(metadata.uid(), uid);
        assert_eq!(metadata.gid(), gid);
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(&std::fs::read(file).unwrap(), contents);
    }
    let response = history(json!({ "session_id": "legacy" }), &client(uid)).unwrap();
    assert_eq!(response["messages"][0]["text"], "preserved owner history");
    assert_eq!(response["messages"][1]["text"], "preserved WAL history");
    drop(legacy_writer);
    run_as_owner(uid, |path| {
        MemoryDb::open(path)
            .map_err(|error| error.to_string())?
            .record_message("legacy", "user", "new owner message")
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .unwrap();
    let original = std::fs::read(&path).unwrap();
    crate::storage::harden_clawd_state().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(std::fs::metadata(&path).unwrap().uid(), uid);
    assert_eq!(
        history(json!({ "session_id": "legacy" }), &client(uid)).unwrap()["n"],
        3
    );

    let foreign = root.path().join("foreign");
    std::fs::write(&foreign, b"not owner memory").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&foreign, &path).unwrap();
    assert!(crate::storage::ensure_owner_agent_state_dir(uid, gid).is_err());
    assert!(sessions(json!({}), &client(uid)).is_err());
    assert_eq!(std::fs::metadata(&foreign).unwrap().uid(), 0);
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&foreign, &path).unwrap();
    assert!(crate::storage::ensure_owner_agent_state_dir(uid, gid).is_err());
    assert!(sessions(json!({}), &client(uid)).is_err());
    assert_eq!(std::fs::metadata(&foreign).unwrap().uid(), 0);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"not owner memory");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"another owner's state").unwrap();
    let foreign_uid = if uid == 65534 { 65533 } else { 65534 };
    std::os::unix::fs::chown(&path, Some(foreign_uid), Some(foreign_uid)).unwrap();
    assert!(crate::storage::ensure_owner_agent_state_dir(uid, gid).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().uid(), foreign_uid);
    assert_eq!(std::fs::read(&path).unwrap(), b"another owner's state");
}

#[test]
fn learned_memory_reset_requires_explicit_confirmation() {
    let error = reset(json!({ "confirm": false }), &client(1000)).unwrap_err();
    assert_eq!(error, "memory reset requires confirm=true");
}

#[test]
fn learned_memory_reset_refuses_root_owner() {
    let error = reset(json!({ "confirm": true }), &client(0)).unwrap_err();
    assert_eq!(error, ROOT_OWNER_REFUSAL);
}

#[test]
fn learned_memory_reset_refuses_an_active_owner_task() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let owner_uid = 1000;
    Store::open_default()
        .unwrap()
        .submit(
            "pending task".into(),
            None,
            None,
            Some(owner_uid),
            Some("/home/test".into()),
        )
        .unwrap();

    let error = refuse_active_tasks(owner_uid).unwrap_err();
    assert!(error.contains("active Agent task"));
}
