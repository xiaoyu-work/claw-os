use super::*;

#[test]
fn host_environment_is_an_allowlist_without_broker_or_credentials() {
    for key in INHERITED_ENV_KEYS {
        assert!(!key.starts_with("CLAWD_"), "{key}");
        let lowered = key.to_ascii_lowercase();
        for secret in ["token", "secret", "password", "credential", "api_key"] {
            assert!(!lowered.contains(secret), "{key}");
        }
    }
    assert!(!INHERITED_ENV_KEYS.contains(&"HOME"));
    assert!(!INHERITED_ENV_KEYS.contains(&"PATH"));
}

#[test]
fn host_resource_limits_are_finite() {
    assert!(HOST_NOFILE_LIMIT <= 1024);
    assert!(HOST_NPROC_LIMIT <= 1024);
    assert!(HOST_ADDRESS_SPACE_LIMIT <= 2 * 1024 * 1024 * 1024);
    assert!(HOST_FILE_SIZE_LIMIT <= 256 * 1024 * 1024);
}

#[test]
fn task_cleanup_is_recursive_and_never_follows_links() {
    use std::io::{Read, Seek};
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let owner = root.path().join("owner");
    let task = owner.join("0123456789abcdef0123456789abcdef");
    let nested = task.join("control/nested");
    std::fs::create_dir_all(&nested).unwrap();
    let external = root.path().join("external");
    std::fs::write(&external, b"outside").unwrap();
    symlink(&external, nested.join("link")).unwrap();
    std::fs::hard_link(&external, nested.join("hard")).unwrap();
    let open_path = nested.join("open");
    std::fs::write(&open_path, b"open-data").unwrap();
    let mut open_file = std::fs::File::open(&open_path).unwrap();

    let owner_fd = open_path_dir(&owner).unwrap();
    let task_fd = open_path_dir(&task).unwrap();
    let task_name = CString::new("0123456789abcdef0123456789abcdef").unwrap();
    cleanup_task_directory(owner_fd.as_raw_fd(), task_fd.as_raw_fd(), &task_name).unwrap();

    assert!(!task.exists());
    assert_eq!(std::fs::read(&external).unwrap(), b"outside");
    let mut retained = String::new();
    open_file.rewind().unwrap();
    open_file.read_to_string(&mut retained).unwrap();
    assert_eq!(retained, "open-data");
}

fn fake_group(root: &std::path::Path, populated: &str, members: &str) {
    std::fs::create_dir(root).unwrap();
    std::fs::write(
        root.join("cgroup.events"),
        format!("populated {populated}\n"),
    )
    .unwrap();
    std::fs::write(root.join("cgroup.procs"), members).unwrap();
}

#[test]
fn containment_rejects_a_non_cgroup_root() {
    let root = tempfile::tempdir().unwrap();
    let error =
        validate_cgroup_root(root.path(), false).expect_err("ordinary directory must be refused");
    assert!(error.contains("cgroup"), "{error}");
}

#[test]
fn containment_requires_every_resource_limit() {
    let root = tempfile::tempdir().unwrap();
    for (name, _) in CGROUP_LIMITS {
        if name != "memory.max" {
            std::fs::write(root.path().join(name), "").unwrap();
        }
    }
    let error = configure_limits(root.path()).expect_err("missing memory limit must fail");
    assert!(error.contains("memory.max"), "{error}");
}

#[test]
fn containment_rejects_missing_or_failed_cgroup_kill() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    fake_group(&missing, "0", "");
    let error = cleanup_cgroup_blocking(&missing, Duration::from_millis(1))
        .expect_err("missing cgroup.kill must fail");
    assert!(error.contains("cgroup.kill"), "{error}");

    let failed = root.path().join("failed");
    fake_group(&failed, "0", "");
    std::fs::create_dir(failed.join("cgroup.kill")).unwrap();
    let error = cleanup_cgroup_blocking(&failed, Duration::from_millis(1))
        .expect_err("unwritable cgroup.kill must fail");
    assert!(error.contains("cgroup.kill"), "{error}");
}

#[test]
fn containment_never_reports_a_populated_group_as_clean() {
    let root = tempfile::tempdir().unwrap();
    let group = root.path().join("populated");
    fake_group(&group, "1", "4242\n");
    std::fs::write(group.join("cgroup.kill"), "").unwrap();
    let error = cleanup_cgroup_blocking(&group, Duration::from_millis(1))
        .expect_err("a populated group must not report cleanup success");
    assert!(error.contains("remained populated"), "{error}");
}
