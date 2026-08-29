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
