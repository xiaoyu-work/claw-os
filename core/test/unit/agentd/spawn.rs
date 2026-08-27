use super::*;

#[test]
fn a_root_owned_task_is_refused_before_a_worker_can_exist() {
    let error = resolve_identity(0).expect_err("uid 0 must be refused");
    assert!(error.contains("non-root"), "{error}");
    assert_eq!(error, ROOT_OWNER_REFUSAL);
}

#[test]
fn an_agent_worker_never_points_at_a_home_its_owner_does_not_control() {
    assert!(resolve_identity(u32::MAX - 1).is_err());
}

#[test]
fn the_worker_binary_can_be_pinned_for_a_dev_tree() {
    let _lock = crate::test_env::lock_env();
    let previous = std::env::var_os("COS_AGENTD_BIN");
    std::env::set_var("COS_AGENTD_BIN", "/opt/claw/claw-agentd");
    assert_eq!(
        worker_binary_path(),
        std::path::PathBuf::from("/opt/claw/claw-agentd")
    );
    match previous {
        Some(value) => std::env::set_var("COS_AGENTD_BIN", value),
        None => std::env::remove_var("COS_AGENTD_BIN"),
    }
}

#[test]
fn a_worker_image_any_user_could_replace_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = dir.path().join("claw-agentd");
    std::fs::write(&binary, b"#!/bin/sh\n").expect("write");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        let error = validate_root_owned_executable(&binary)
            .expect_err("a world-writable worker image must be refused");
        // Running as an ordinary user the ownership check fires first;
        // either refusal is the fail-closed outcome we require.
        assert!(
            error.contains("not owned by root") || error.contains("world writable"),
            "{error}"
        );

        let link = dir.path().join("claw-agentd-link");
        std::os::unix::fs::symlink(&binary, &link).expect("symlink");
        let error =
            validate_root_owned_executable(&link).expect_err("a symlinked image must be refused");
        assert!(error.contains("symlink"), "{error}");
    }
}

#[test]
fn post_fork_failures_report_a_bare_errno_without_allocating() {
    // Only the forking thread survives `fork`, so any allocator lock a
    // dropped thread held stays locked. Every post-fork error must be a
    // raw-OS error: `io::Error::new` would box a payload.
    let error = raw_error(libc::EPERM);
    assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    assert_eq!(
        raw_error(libc::ESRCH).raw_os_error(),
        Some(libc::ESRCH),
        "the parent must still be able to tell the failures apart"
    );

    // The identity verification path is the one that runs furthest from
    // the parent; check it yields a raw errno for a mismatch rather than
    // a formatted message.
    let uid = unsafe { libc::getuid() } as u32;
    let gid = unsafe { libc::getgid() } as u32;
    let mismatch = verify_dropped_identity(uid.wrapping_add(1), gid, false)
        .expect_err("a uid mismatch must abort the exec");
    assert_eq!(mismatch.raw_os_error(), Some(libc::EPERM));
    assert!(verify_dropped_identity(uid, gid, false).is_ok());
}

#[test]
fn the_worker_environment_is_an_allowlist_that_excludes_broker_state() {
    // Nothing that could carry a credential, a broker socket path or a
    // capability decision may be inherited.
    for key in INHERITED_ENV_KEYS {
        assert!(
            !key.starts_with("CLAWD_"),
            "broker configuration `{key}` must not reach the worker"
        );
        let lowered = key.to_ascii_lowercase();
        for secret in ["key", "token", "secret", "password", "credential"] {
            assert!(
                !lowered.contains(secret),
                "`{key}` looks like a credential and must not be inherited"
            );
        }
    }
    assert!(!INHERITED_ENV_KEYS.contains(&"HOME"));
    assert!(!INHERITED_ENV_KEYS.contains(&"PATH"));
}
