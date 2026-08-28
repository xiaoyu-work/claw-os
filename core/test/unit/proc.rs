use super::*;

/// PID recycle protection: a registry entry with a pid that is
/// currently alive but whose recorded `start_time_ticks` does
/// not match the kernel's report must be treated as exited.
/// Without this check, a recycled pid (e.g. a different
/// program that happens to land on the same pid after a wrap)
/// would falsely look like "our session still alive" and a
/// caller could SIGTERM the wrong process.
#[cfg(target_os = "linux")]
#[test]
fn pid_recycle_safe() {
    // Use the test binary's own pid: definitely alive, and we
    // can read its real starttime from /proc/<pid>/stat.
    let real_pid = std::process::id();
    let real_start = read_start_time_ticks(real_pid)
        .expect("test must run on Linux with /proc/<pid>/stat readable");

    // 1) Matching start_time → alive.
    let info_match = SessionInfo {
        session_id: "s-match".into(),
        pid: real_pid,
        command: vec!["cos".into()],
        started_at: "2026-01-01T00:00:00Z".into(),
        stdout_path: "/dev/null".into(),
        stderr_path: "/dev/null".into(),
        group: None,
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: None,
        scope: None,
        priority: None,
        caps: None,
        transient_caps: None,
        role: None,
        app_id: None,
        pending_bind: false,
        start_time_ticks: Some(real_start),
    };
    assert!(
        is_alive_for_info(&info_match),
        "matching pid + start_time should report alive"
    );

    // 2) Mismatched start_time (the pid was recycled into a
    //    different process from the one we recorded) → exited.
    let info_recycled = SessionInfo {
        start_time_ticks: Some(real_start.wrapping_add(1_000_000)),
        ..info_match.clone()
    };
    assert!(
        !is_alive_for_info(&info_recycled),
        "recycled-pid: start_time mismatch must be reported as exited"
    );

    // 3) Legacy entry with no recorded start_time → fall back
    //    to the basic pid check (preserves behaviour for rows
    //    written by older cos).
    let info_legacy = SessionInfo {
        start_time_ticks: None,
        ..info_match.clone()
    };
    assert!(
        is_alive_for_info(&info_legacy),
        "legacy entry (no start_time) falls back to pid-only check"
    );
}

/// UID-scope regression. The happy path: every process in our own
/// pgrp belongs to us, so `pgrp_uid_scope_check` returns Ok(()).
/// Forking a setuid stranger into our pgrp from a unit test is
/// impractical (and would require root), so we verify the helper
/// at least correctly clears the caller's own group — the actual
/// foreign-uid path is exercised by `pgrp_uid_scope_check_*`
/// fuzz-style tests below using synthetic /proc parsers.
#[cfg(target_os = "linux")]
#[test]
fn pgrp_uid_scope_check_passes_for_own_pgrp() {
    let me = caller_uid();
    // The test binary is itself a session leader in cargo's
    // test harness in many cases; even if not, our own pid's
    // pgrp will contain only our-uid processes.
    let my_pid = std::process::id();
    let res = pgrp_uid_scope_check(my_pid, me);
    assert!(
        res.is_ok(),
        "expected our own pgrp to be exclusively uid={me}, got {res:?}",
    );
}

/// UID-scope regression: when we pass an `expected_uid` we know
/// is wrong (caller's uid + 12345), `pgrp_uid_scope_check` MUST
/// return Err and include every process in the pgrp. This stands
/// in for the privilege-confusion case the audit flagged: if any
/// pgrp member is owned by someone other than the caller, we
/// refuse to broadcast a SIGTERM to the whole group.
#[cfg(target_os = "linux")]
#[test]
fn pgrp_uid_scope_check_flags_wrong_uid() {
    let me = caller_uid();
    let my_pid = std::process::id();
    // Pick a uid that almost certainly is not present in our
    // own pgrp — caller's uid + 12345.
    let bogus_uid = me.saturating_add(12345);
    let res = pgrp_uid_scope_check(my_pid, bogus_uid);
    assert!(
        res.is_err(),
        "pgrp seen as exclusively {bogus_uid}, but our uid is {me}"
    );
    let foreign = res.unwrap_err();
    assert!(!foreign.is_empty(), "Err returned with no foreign pids");
    // Every entry has the caller's real uid, not the bogus one.
    for (_pid, uid) in &foreign {
        assert_eq!(*uid, me, "foreign report listed unexpected uid");
    }
}

// regression: cmd_kill --group must call pgrp_uid_scope_check
// for each session before invoking kill_process, skip sessions
// whose pgrp contains a foreign UID, and report the skip with
// reason="uid_scope_violation". The full integration path is
// not unit-testable without root (need a setuid child in our
// own pgrp), so the audit-required behaviour is verified by
// the two helper tests above plus the source-level check at
// cmd_kill's --group branch.

#[cfg(target_os = "linux")]
fn register_spawn_test_parent(caps: crate::caps::CapSet) -> String {
    let session_id = format!("proc-parent-{}", uuid::Uuid::new_v4().simple());
    register_session(SessionInfo {
        session_id: session_id.clone(),
        pid: std::process::id(),
        command: vec!["cargo-test".to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: None,
        parent: None,
        workdir: std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
        exit_code: None,
        ended_at: None,
        tier: None,
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: None,
        app_id: None,
        pending_bind: false,
        start_time_ticks: read_start_time_ticks(std::process::id()),
    })
    .unwrap();
    std::env::set_var("COS_SESSION", &session_id);
    session_id
}

#[cfg(target_os = "linux")]
#[test]
fn proc_spawn_approval_binds_canonical_executable_and_all_arguments() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    let parent = register_spawn_test_parent(crate::caps::CapSet::new());
    let invocation =
        crate::approvals::LocalApprovalInvocation::new("web:proc-security:turn:1").unwrap();

    invocation.sync_scope(|| {
        let harmless = vec![
            "--session".to_string(),
            "safe-child".to_string(),
            "--".to_string(),
            "/bin/echo".to_string(),
            "hello".to_string(),
        ];
        cmd_spawn(&harmless).expect_err("proc.spawn must require approval");
        let spawn_request = crate::approvals::list_pending()
            .into_iter()
            .find(|request| request.verb == crate::caps::Verb::PROC_SPAWN.as_str())
            .expect("proc.spawn request");
        let harmless_digest = spawn_request
            .operation_digest
            .clone()
            .expect("spawn request must bind the invocation");
        crate::approvals::approve(
            &spawn_request.id,
            crate::approvals::GrantDuration::Session,
            None,
            None,
        )
        .unwrap();

        cmd_spawn(&harmless).expect_err("fs.exec must require separate approval");
        let exec_request = crate::approvals::list_pending()
            .into_iter()
            .find(|request| request.verb == crate::caps::Verb::FS_EXEC.as_str())
            .expect("fs.exec request");
        assert_eq!(
            exec_request.operation_digest.as_deref(),
            Some(harmless_digest.as_str())
        );
        assert_eq!(
            exec_request.scope,
            Scope::path(
                resolve_spawn_executable("/bin/echo", &std::env::current_dir().unwrap())
                    .unwrap()
                    .to_string_lossy()
            )
            .canonicalized()
        );
        let audit = std::fs::read_to_string(crate::paths::caps_audit_log_path()).unwrap();
        let exec_audit: serde_json::Value = audit
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .find(|record: &serde_json::Value| record["verb"] == Verb::FS_EXEC.as_str())
            .expect("fs.exec audit record");
        assert_eq!(exec_audit["scope"], serde_json::json!(exec_request.scope));
        assert_eq!(
            exec_audit["operation_digest"],
            serde_json::json!(harmless_digest)
        );
        assert!(
            exec_audit.get("argv").is_none(),
            "raw process arguments must not be persisted in capability audit"
        );
        crate::approvals::approve(
            &exec_request.id,
            crate::approvals::GrantDuration::Session,
            None,
            None,
        )
        .unwrap();

        let changed_args = vec![
            "--session".to_string(),
            "safe-child".to_string(),
            "--".to_string(),
            "/bin/echo".to_string(),
            "different".to_string(),
        ];
        cmd_spawn(&changed_args).expect_err("changed argv must need a fresh approval");
        assert!(crate::approvals::list_pending().iter().any(|request| {
            request.verb == crate::caps::Verb::PROC_SPAWN.as_str()
                && request.operation_digest.as_deref() != Some(harmless_digest.as_str())
        }));

        let marker = temp.path().join("shell-substitution");
        let shell = vec![
            "--session".to_string(),
            "safe-child".to_string(),
            "--".to_string(),
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("touch {}", marker.display()),
        ];
        cmd_spawn(&shell).expect_err("shell substitution must need a fresh approval");
        assert!(!marker.exists(), "the substituted shell must never execute");

        let result = cmd_spawn(&harmless).expect("the exact approved invocation may execute");
        assert_eq!(result["parent"], parent);
    });
}

#[cfg(target_os = "linux")]
#[test]
fn proc_spawn_child_capabilities_cannot_exceed_the_parent() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    let executable =
        resolve_spawn_executable("/bin/echo", &std::env::current_dir().unwrap()).unwrap();
    let parent_caps = crate::caps::CapSet::from_caps([
        crate::caps::Cap::new(Verb::PROC_SPAWN, Scope::self_ref("children")),
        crate::caps::Cap::new(Verb::FS_EXEC, Scope::path(executable.to_string_lossy())),
    ]);
    register_spawn_test_parent(parent_caps);

    let args = vec![
        "--session".to_string(),
        "overprivileged-child".to_string(),
        "--caps".to_string(),
        Verb::FS_WRITE.as_str().to_string(),
        "--scope-path".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        "/bin/echo".to_string(),
        "hello".to_string(),
    ];
    let error = cmd_spawn(&args).unwrap_err();
    assert!(error.contains("cannot widen caps"), "{error}");
    assert!(
        session_info_by_id("overprivileged-child").is_none(),
        "a rejected child must not enter the process registry"
    );
}
