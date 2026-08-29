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

#[test]
fn socket_permission_projection_uses_the_exact_uid_and_primary_gid() {
    let socket = FileIdentity {
        device: 1,
        inode: 2,
        uid: 0,
        gid: 27,
        mode: libc::S_IFSOCK | 0o660,
    };
    assert!(socket.writable_by(1000, 27));
    assert!(!socket.writable_by(1000, 65534));

    let owner_socket = FileIdentity {
        uid: 1000,
        ..socket
    };
    assert!(owner_socket.writable_by(1000, 65534));

    let world_socket = FileIdentity {
        mode: libc::S_IFSOCK | 0o662,
        ..socket
    };
    assert!(world_socket.writable_by(1000, 65534));
}

#[test]
fn configured_isolation_group_must_exist_and_be_non_root() {
    let _lock = crate::test_env::lock_env();
    let _group =
        crate::test_env::TestEnvVarGuard::set(ISOLATED_GROUP_ENV, "cos-group-that-must-not-exist");
    let error = resolve_isolated_execution_gid().expect_err("missing group must fail closed");
    assert!(error.contains("does not exist"), "{error}");
}
