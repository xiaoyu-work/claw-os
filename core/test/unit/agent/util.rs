use super::*;

#[test]
fn atomic_write_creates_and_replaces_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    atomic_write_with_fsync(&path, b"first").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    atomic_write_with_fsync(&path, b"second").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"second");
}

#[test]
fn atomic_write_leaves_no_tmp_files_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    atomic_write_with_fsync(&path, b"x").unwrap();
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            !name.ends_with(".tmp"),
            "no leftover tmp file expected, got {name}"
        );
    }
}

#[test]
fn atomic_write_creates_parent_dir() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("a").join("state.json");
    atomic_write_with_fsync(&path, b"hello").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"hello");
}

#[test]
fn durable_directory_preflight_fails_before_mutation() {
    let _lock = crate::test_env::lock_env();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("one").join("two");
    let _guard =
        crate::test_env::TestEnvVarGuard::set("COS_TEST_PERSISTENCE_FAILPOINT", "dir_fsync");
    assert!(ensure_durable_private_dir(&target).is_err());
    assert!(!dir.path().join("one").exists());
}

#[test]
fn visible_directory_creation_error_is_recoverable_only_after_revalidation() {
    let _lock = crate::test_env::lock_env();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("one");
    {
        let _guard = crate::test_env::TestEnvVarGuard::set(
            "COS_TEST_PERSISTENCE_FAILPOINT",
            "after_dir_create",
        );
        assert!(ensure_durable_private_dir(&target).is_err());
    }
    assert!(target.is_dir());
    ensure_durable_private_dir(&target).unwrap();
}

#[test]
fn atomic_write_propagates_every_durability_barrier_failure() {
    let _lock = crate::test_env::lock_env();
    for point in [
        "file_open",
        "file_write",
        "file_fsync",
        "rename",
        "after_rename",
        "dir_open",
        "dir_fsync",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let _guard = crate::test_env::TestEnvVarGuard::set("COS_TEST_PERSISTENCE_FAILPOINT", point);
        let error = atomic_write_with_fsync(&path, b"value").unwrap_err();
        assert!(error.to_string().contains("failpoint"), "{point}: {error}");
        if matches!(point, "file_open" | "file_write" | "file_fsync") {
            assert!(
                std::fs::read_dir(dir.path()).unwrap().all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")),
                "{point} left a staging file"
            );
        }
        if point == "after_rename" {
            assert!(error.to_string().contains("do not retry"), "{error}");
        }
    }
}

#[test]
fn char_safe_truncate_ascii_keeps_n_bytes() {
    assert_eq!(char_safe_truncate("hello world", 5), "hello");
    assert_eq!(char_safe_truncate("hello", 100), "hello");
    assert_eq!(char_safe_truncate("", 10), "");
}

#[test]
fn char_safe_truncate_walks_back_off_multibyte() {
    // "héllo" — 'é' is 2 bytes (0xC3 0xA9). Bytes: h(1) é(2) l(1) l(1) o(1) = 6.
    let s = "héllo";
    // Cut at byte 2 — middle of 'é'. Walk back to byte 1.
    assert_eq!(char_safe_truncate(s, 2), "h");
    // Cut at byte 3 — just past 'é'. Already on boundary.
    assert_eq!(char_safe_truncate(s, 3), "hé");
}

#[test]
fn char_safe_truncate_handles_emoji() {
    // "hi 🌍" — emoji is 4 bytes. Total: h(1) i(1) ' '(1) 🌍(4) = 7.
    let s = "hi 🌍";
    assert_eq!(char_safe_truncate(s, 4), "hi "); // mid-emoji byte 4 walks back to 3
    assert_eq!(char_safe_truncate(s, 7), "hi 🌍");
    assert_eq!(char_safe_truncate(s, 100), "hi 🌍");
}

#[test]
fn char_safe_truncate_zero_returns_empty() {
    assert_eq!(char_safe_truncate("anything", 0), "");
}
