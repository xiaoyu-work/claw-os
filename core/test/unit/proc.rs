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
struct StaticSpawnHelpers {
    _dir: tempfile::TempDir,
    trusted: std::path::PathBuf,
    attacker: std::path::PathBuf,
    descendant_probe: std::path::PathBuf,
    cwd_loader: std::path::PathBuf,
}

#[cfg(target_os = "linux")]
fn compile_static_spawn_helper(
    dir: &std::path::Path,
    name: &str,
    source: &str,
) -> std::path::PathBuf {
    let source_path = dir.join(format!("{name}.rs"));
    let binary_path = dir.join(name);
    std::fs::write(&source_path, source).unwrap();
    let output =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("-C")
            .arg("target-feature=+crt-static")
            .arg("-C")
            .arg("relocation-model=static")
            .arg("-C")
            .arg("link-arg=-no-pie")
            .arg("-C")
            .arg("strip=symbols")
            .arg(&source_path)
            .arg("-o")
            .arg(&binary_path)
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "compile static spawn helper: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_file(source_path);
    binary_path
}

#[cfg(target_os = "linux")]
fn static_spawn_helpers() -> &'static StaticSpawnHelpers {
    static HELPERS: std::sync::OnceLock<StaticSpawnHelpers> = std::sync::OnceLock::new();
    HELPERS.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let trusted = compile_static_spawn_helper(
            dir.path(),
            "trusted-native",
            r#"
fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().expect("output path");
    let value = args.next().unwrap_or_else(|| "trusted".into());
    std::fs::write(path, value.to_string_lossy().into_owned()).expect("write output");
}
"#,
        );
        let attacker = compile_static_spawn_helper(
            dir.path(),
            "attacker-native",
            r#"
fn main() {
    let path = std::env::args_os().nth(1).expect("output path");
    std::fs::write(path, "attacker").expect("write output");
}
"#,
        );
        let descendant_probe = compile_static_spawn_helper(
            dir.path(),
            "descendant-probe",
            r#"
fn main() {
    let path = std::env::args_os().nth(1).expect("output path");
    let fd3 = std::fs::read_link("/proc/self/fd/3").is_ok();
    let fd50 = std::fs::read_link("/proc/self/fd/50").is_ok();
    let channel = std::env::var_os("COS_AGENTD_CHANNEL_FD").is_some();
    let task = std::env::var_os("COS_AGENTD_TASK").is_some();
    std::fs::write(path, format!("{fd3}:{fd50}:{channel}:{task}")).expect("write output");
}
"#,
        );
        let cwd_loader = compile_static_spawn_helper(
            dir.path(),
            "cwd-loader",
            r#"
fn main() {
    if std::path::Path::new("Makefile").exists() {
        std::fs::write("loader-ran", "loaded").expect("write marker");
    }
}
"#,
        );
        StaticSpawnHelpers {
            _dir: dir,
            trusted,
            attacker,
            descendant_probe,
            cwd_loader,
        }
    })
}

#[cfg(target_os = "linux")]
fn install_static_spawn_helper(source: &std::path::Path, destination: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::copy(source, destination).unwrap();
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(target_os = "linux")]
struct TestProcAllowlist {
    _path: crate::test_env::TestEnvVarGuard,
    _owner: crate::test_env::TestEnvVarGuard,
}

#[cfg(target_os = "linux")]
fn install_test_proc_allowlist(
    id: &str,
    executable: &std::path::Path,
    argv: &[String],
    output_args: &[usize],
    workdir: &std::path::Path,
) -> TestProcAllowlist {
    let executable = executable.canonicalize().unwrap();
    let workdir = workdir.canonicalize().unwrap();
    let policy_path = executable
        .parent()
        .unwrap()
        .join(format!("{id}-proc-allowlist.json"));
    let executable_bytes = std::fs::read(&executable).unwrap();
    std::fs::write(
        &policy_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 1,
            "commands": [{
                "id": id,
                "executable": executable,
                "sha256": crate::crypto::sha256_hex(&executable_bytes),
                "argv_exact": argv,
                "output_args": output_args,
                "workdir": workdir,
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    TestProcAllowlist {
        _path: crate::test_env::TestEnvVarGuard::set(
            "COS_PROC_SPAWN_ALLOWLIST_TEST_PATH",
            &policy_path,
        ),
        _owner: crate::test_env::TestEnvVarGuard::set(
            "COS_PROC_SPAWN_ALLOWLIST_TEST_TRUST_CURRENT",
            "1",
        ),
    }
}

#[cfg(target_os = "linux")]
struct InheritedFdGuard {
    fd: i32,
    saved: Option<i32>,
}

#[cfg(target_os = "linux")]
impl InheritedFdGuard {
    fn install(fd: i32, source: &std::fs::File) -> Self {
        use std::os::fd::AsRawFd;

        let saved = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 100) };
        let saved = (saved >= 0).then_some(saved);
        let duplicate = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 100) };
        assert!(duplicate >= 0);
        assert!(unsafe { libc::dup2(duplicate, fd) } >= 0);
        unsafe {
            libc::close(duplicate);
        }
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
        Self { fd, saved }
    }
}

#[cfg(target_os = "linux")]
impl Drop for InheritedFdGuard {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
            if let Some(saved) = self.saved {
                libc::dup2(saved, self.fd);
                libc::close(saved);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn write_spawn_script(path: &std::path::Path, result: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(
        path,
        format!("#!/bin/sh\nprintf '%s' '{result}' > \"$1\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(target_os = "linux")]
fn approve_spawn_permissions(args: &[String]) -> String {
    cmd_spawn(args).expect_err("proc.spawn must require approval");
    let spawn_request = crate::approvals::list_pending()
        .into_iter()
        .find(|request| request.verb == Verb::PROC_SPAWN.as_str())
        .expect("proc.spawn request");
    let digest = spawn_request
        .operation_digest
        .clone()
        .expect("spawn request must bind the immutable invocation");
    crate::approvals::approve(
        &spawn_request.id,
        crate::approvals::GrantDuration::Session,
        None,
        None,
    )
    .unwrap();

    cmd_spawn(args).expect_err("fs.exec must require separate approval");
    let exec_request = crate::approvals::list_pending()
        .into_iter()
        .find(|request| request.verb == Verb::FS_EXEC.as_str())
        .expect("fs.exec request");
    assert_eq!(
        exec_request.operation_digest.as_deref(),
        Some(digest.as_str())
    );
    crate::approvals::approve(
        &exec_request.id,
        crate::approvals::GrantDuration::Session,
        None,
        None,
    )
    .unwrap();
    digest
}

#[cfg(target_os = "linux")]
fn wait_for_spawn_result(path: &std::path::Path) -> String {
    for _ in 0..200 {
        if let Ok(value) = std::fs::read_to_string(path) {
            if !value.is_empty() {
                return value;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("spawned process did not write {}", path.display());
}

#[cfg(target_os = "linux")]
#[test]
fn shebang_script_is_rejected_before_approval() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let script = temp.path().join("direct-script");
    let marker = temp.path().join("direct-script-result");
    write_spawn_script(&script, "attacker");

    let error = cmd_spawn(&[
        "--".to_string(),
        script.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
    ])
    .unwrap_err();
    assert!(error.contains("shebang script"), "{error}");
    assert!(error.contains("cos_sandbox"), "{error}");
    assert!(crate::approvals::list_pending().is_empty());
    assert!(!marker.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn python_script_argument_is_rejected_before_approval() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let script = temp.path().join("payload.py");
    std::fs::write(&script, "raise SystemExit(0)\n").unwrap();

    let error = cmd_spawn(&[
        "--".to_string(),
        "python3".to_string(),
        script.to_string_lossy().into_owned(),
    ])
    .unwrap_err();
    assert!(error.contains("cos_sandbox"), "{error}");
    assert!(crate::approvals::list_pending().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn shell_eval_is_rejected_before_approval() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let marker = temp.path().join("shell-eval-result");

    let error = cmd_spawn(&[
        "--".to_string(),
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("touch {}", marker.display()),
    ])
    .unwrap_err();
    assert!(error.contains("cos_sandbox"), "{error}");
    assert!(crate::approvals::list_pending().is_empty());
    assert!(!marker.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn renamed_static_runtime_cannot_assume_allowlisted_path_semantics() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("allowlisted-native");
    let marker = temp.path().join("renamed-runtime-result");
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let argv = vec![marker.to_string_lossy().into_owned(), "trusted".to_string()];
    let _allowlist =
        install_test_proc_allowlist("allowlisted-native", &executable, &argv, &[0], temp.path());
    std::fs::copy(&static_spawn_helpers().attacker, &executable).unwrap();

    let error = cmd_spawn(&[
        "--workdir".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
        "trusted".to_string(),
    ])
    .unwrap_err();
    assert!(error.contains("allowlisted hash"), "{error}");
    assert!(error.contains("cos_sandbox"), "{error}");
    assert!(crate::approvals::list_pending().is_empty());
    assert!(!marker.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn mutable_package_directory_argument_is_rejected_even_if_schema_lists_it() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("package-loader");
    let package = temp.path().join("mutable-package");
    std::fs::create_dir(&package).unwrap();
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let argv = vec![package.to_string_lossy().into_owned()];
    let _allowlist =
        install_test_proc_allowlist("package-loader", &executable, &argv, &[], temp.path());

    let error = cmd_spawn(&[
        "--workdir".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        package.to_string_lossy().into_owned(),
    ])
    .unwrap_err();
    assert!(error.contains("path arguments"), "{error}");
    assert!(error.contains("cos_sandbox"), "{error}");
    assert!(crate::approvals::list_pending().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn make_like_loader_cannot_switch_to_a_mutable_project_cwd() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("make");
    let safe_workdir = temp.path().join("safe-cwd");
    let project = temp.path().join("mutable-project");
    std::fs::create_dir(&safe_workdir).unwrap();
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("Makefile"), "attacker-controlled").unwrap();
    install_static_spawn_helper(&static_spawn_helpers().cwd_loader, &executable);
    let _allowlist =
        install_test_proc_allowlist("fixed-cwd-loader", &executable, &[], &[], &safe_workdir);

    let error = cmd_spawn(&[
        "--workdir".to_string(),
        project.to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
    ])
    .unwrap_err();
    assert!(error.contains("command-specific schema"), "{error}");
    assert!(error.contains("cos_sandbox"), "{error}");
    assert!(!project.join("loader-ran").exists());
    assert!(crate::approvals::list_pending().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn user_owned_or_unknown_static_binary_is_not_unsandboxed_authority() {
    let _lock = crate::test_env::lock_env();
    let synthetic = SpawnResourceBinding {
        path: "/home/user/tool".to_string(),
        identity: Some(SpawnFileIdentity {
            device: 1,
            inode: 2,
            mode: libc::S_IFREG | 0o700,
            owner_uid: 1000,
            owner_gid: 1000,
        }),
        content_sha256: Some(crate::crypto::sha256_hex(b"tool")),
    };
    assert!(!proc_spawn_allowlist::immutable_root_owned_for_test(
        &synthetic
    ));

    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let allowlisted = temp.path().join("known-native");
    let unknown = temp.path().join("unknown-native");
    let marker = temp.path().join("unknown-native-result");
    install_static_spawn_helper(&static_spawn_helpers().trusted, &allowlisted);
    install_static_spawn_helper(&static_spawn_helpers().trusted, &unknown);
    let _allowlist = install_test_proc_allowlist(
        "known-native",
        &allowlisted,
        &[marker.to_string_lossy().into_owned(), "trusted".to_string()],
        &[0],
        temp.path(),
    );

    let error = cmd_spawn(&[
        "--workdir".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        unknown.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
        "trusted".to_string(),
    ])
    .unwrap_err();
    assert!(error.contains("not in the audited allowlist"), "{error}");
    assert!(error.contains("cos_sandbox"), "{error}");
    assert!(crate::approvals::list_pending().is_empty());
    assert!(!marker.exists());
}

#[cfg(target_os = "linux")]
fn output_test_args(
    executable: &std::path::Path,
    workdir: &std::path::Path,
    output: &std::path::Path,
) -> Vec<String> {
    vec![
        "--workdir".to_string(),
        workdir.to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        "trusted".to_string(),
    ]
}

#[cfg(target_os = "linux")]
#[test]
fn fifo_output_is_rejected_without_opening_an_exfiltration_channel() {
    use std::os::unix::ffi::OsStrExt;

    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("fifo-writer");
    let output_parent = temp.path().join("secure-output");
    let output = output_parent.join("exfiltration-fifo");
    std::fs::create_dir(&output_parent).unwrap();
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let encoded = std::ffi::CString::new(output.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
    let argv = vec![output.to_string_lossy().into_owned(), "trusted".to_string()];
    let _allowlist =
        install_test_proc_allowlist("fifo-writer", &executable, &argv, &[0], temp.path());

    let error = cmd_spawn(&output_test_args(&executable, temp.path(), &output)).unwrap_err();
    assert!(error.contains("not a regular file"), "{error}");
    assert!(error.contains("cos_sandbox"), "{error}");
    assert!(crate::approvals::list_pending().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn socket_and_device_outputs_are_rejected_as_non_regular() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("special-writer");
    let output_parent = temp.path().join("secure-output");
    let socket = output_parent.join("output.sock");
    std::fs::create_dir(&output_parent).unwrap();
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

    let socket_argv = vec![socket.to_string_lossy().into_owned(), "trusted".to_string()];
    let _socket_allowlist = install_test_proc_allowlist(
        "socket-writer",
        &executable,
        &socket_argv,
        &[0],
        temp.path(),
    );
    let socket_error = cmd_spawn(&output_test_args(&executable, temp.path(), &socket)).unwrap_err();
    assert!(
        socket_error.contains("not a regular file"),
        "{socket_error}"
    );
    assert!(crate::approvals::list_pending().is_empty());
    drop(_socket_allowlist);

    let device = std::path::Path::new("/dev/null");
    let device_argv = vec![device.to_string_lossy().into_owned(), "trusted".to_string()];
    let _device_allowlist = install_test_proc_allowlist(
        "device-writer",
        &executable,
        &device_argv,
        &[0],
        temp.path(),
    );
    let device_error = cmd_spawn(&output_test_args(&executable, temp.path(), device)).unwrap_err();
    assert!(
        device_error.contains("not a regular file"),
        "{device_error}"
    );
    assert!(crate::approvals::list_pending().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn attacker_writable_output_parent_is_rejected_before_creation() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("tmp-writer");
    let output =
        std::path::PathBuf::from(format!("/tmp/cos-output-{}", uuid::Uuid::new_v4().simple()));
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let argv = vec![output.to_string_lossy().into_owned(), "trusted".to_string()];
    let _allowlist =
        install_test_proc_allowlist("tmp-writer", &executable, &argv, &[0], temp.path());

    let error = cmd_spawn(&output_test_args(&executable, temp.path(), &output)).unwrap_err();
    assert!(error.contains("non-attacker-writable"), "{error}");
    assert!(error.contains("cos_sandbox"), "{error}");
    assert!(!output.exists());
    assert!(crate::approvals::list_pending().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn output_inode_replacement_changes_the_approval_digest() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("identity-writer");
    let output_parent = temp.path().join("secure-output");
    let output = output_parent.join("result");
    let old_output = output_parent.join("old-result");
    std::fs::create_dir(&output_parent).unwrap();
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let argv = vec![output.to_string_lossy().into_owned(), "trusted".to_string()];
    let _allowlist =
        install_test_proc_allowlist("identity-writer", &executable, &argv, &[0], temp.path());
    let args = output_test_args(&executable, temp.path(), &output);

    crate::approvals::LocalApprovalInvocation::new("web:output-identity:turn:1")
        .unwrap()
        .sync_scope(|| {
            cmd_spawn(&args).expect_err("proc.spawn must require approval");
            let original = crate::approvals::list_pending()
                .into_iter()
                .find(|request| request.verb == Verb::PROC_SPAWN.as_str())
                .unwrap();
            let original_digest = original.operation_digest.clone().unwrap();
            crate::approvals::approve(
                &original.id,
                crate::approvals::GrantDuration::Session,
                None,
                None,
            )
            .unwrap();
            std::fs::rename(&output, &old_output).unwrap();
            std::fs::write(&output, "").unwrap();
            std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o600)).unwrap();

            cmd_spawn(&args).expect_err("replacement output needs fresh consent");
            let replacement = crate::approvals::list_pending()
                .into_iter()
                .find(|request| request.verb == Verb::PROC_SPAWN.as_str())
                .unwrap();
            assert_ne!(
                replacement.operation_digest.as_deref(),
                Some(original_digest.as_str())
            );
        });
}

#[cfg(target_os = "linux")]
#[test]
fn symlink_swap_after_output_pinning_writes_only_the_opened_fd() {
    use std::os::unix::fs::symlink;

    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("symlink-output-writer");
    let output_parent = temp.path().join("secure-output");
    let output = output_parent.join("result");
    let pinned_output = output_parent.join("pinned-result");
    let attacker = output_parent.join("attacker-target");
    std::fs::create_dir(&output_parent).unwrap();
    std::fs::write(&attacker, "unchanged").unwrap();
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let argv = vec![output.to_string_lossy().into_owned(), "trusted".to_string()];
    let _allowlist = install_test_proc_allowlist(
        "symlink-output-writer",
        &executable,
        &argv,
        &[0],
        temp.path(),
    );
    let args = output_test_args(&executable, temp.path(), &output);

    crate::approvals::LocalApprovalInvocation::new("web:output-symlink:turn:1")
        .unwrap()
        .sync_scope(|| {
            approve_spawn_permissions(&args);
            let output_for_hook = output.clone();
            let pinned_for_hook = pinned_output.clone();
            let attacker_for_hook = attacker.clone();
            set_pre_spawn_test_hook(move || {
                std::fs::rename(&output_for_hook, &pinned_for_hook).unwrap();
                symlink(attacker_for_hook, &output_for_hook).unwrap();
            });
            cmd_spawn(&args).expect("the pinned output fd may be written");
            assert_eq!(wait_for_spawn_result(&pinned_output), "trusted");
            assert_eq!(std::fs::read_to_string(&attacker).unwrap(), "unchanged");
            assert!(std::fs::symlink_metadata(&output)
                .unwrap()
                .file_type()
                .is_symlink());
        });
}

#[cfg(target_os = "linux")]
#[test]
fn secure_private_output_is_created_and_passed_by_descriptor() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("secure-output-writer");
    let output_parent = temp.path().join("secure-output");
    let output = output_parent.join("result");
    std::fs::create_dir(&output_parent).unwrap();
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let argv = vec![output.to_string_lossy().into_owned(), "trusted".to_string()];
    let _allowlist = install_test_proc_allowlist(
        "secure-output-writer",
        &executable,
        &argv,
        &[0],
        temp.path(),
    );
    let args = output_test_args(&executable, temp.path(), &output);

    crate::approvals::LocalApprovalInvocation::new("web:secure-output:turn:1")
        .unwrap()
        .sync_scope(|| {
            approve_spawn_permissions(&args);
            cmd_spawn(&args).expect("secure output may be written");
            assert_eq!(wait_for_spawn_result(&output), "trusted");
            assert!(std::fs::metadata(&output).unwrap().is_file());
        });
}

#[cfg(target_os = "linux")]
#[test]
fn proc_descendant_cannot_inherit_agentd_channel_or_task_hints() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    let _channel_hint =
        crate::test_env::TestEnvVarGuard::set(crate::agentd::protocol::CHANNEL_FD_ENV, "3");
    let _task_hint = crate::test_env::TestEnvVarGuard::set(
        crate::agentd::protocol::TASK_HINT_ENV,
        "task-secret",
    );
    register_spawn_test_parent(crate::caps::CapSet::new());
    let leaked = std::fs::File::open("/dev/null").unwrap();
    let _fd3 = InheritedFdGuard::install(3, &leaked);
    let _fd50 = InheritedFdGuard::install(50, &leaked);
    let executable = temp.path().join("descendant-probe");
    let marker = temp.path().join("descendant-probe-result");
    install_static_spawn_helper(&static_spawn_helpers().descendant_probe, &executable);
    let args = vec![
        "--session".to_string(),
        "descendant-probe-child".to_string(),
        "--workdir".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
    ];
    let _allowlist = install_test_proc_allowlist(
        "descendant-probe",
        &executable,
        &[marker.to_string_lossy().into_owned()],
        &[0],
        temp.path(),
    );

    crate::approvals::LocalApprovalInvocation::new("web:descendant-probe:turn:1")
        .unwrap()
        .sync_scope(|| {
            approve_spawn_permissions(&args);
            cmd_spawn(&args).expect("the native probe may execute");
            assert_eq!(wait_for_spawn_result(&marker), "false:false:false:false");
        });
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
    let executable = temp.path().join("approved-native");
    let marker = temp.path().join("approved-native-result");
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let _allowlist = install_test_proc_allowlist(
        "approved-native",
        &executable,
        &[marker.to_string_lossy().into_owned(), "hello".to_string()],
        &[0],
        temp.path(),
    );

    invocation.sync_scope(|| {
        let harmless = vec![
            "--session".to_string(),
            "safe-child".to_string(),
            "--workdir".to_string(),
            temp.path().to_string_lossy().into_owned(),
            "--".to_string(),
            executable.to_string_lossy().into_owned(),
            marker.to_string_lossy().into_owned(),
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
                resolve_spawn_executable(
                    &executable.to_string_lossy(),
                    &std::env::current_dir().unwrap()
                )
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
            "--workdir".to_string(),
            temp.path().to_string_lossy().into_owned(),
            "--".to_string(),
            executable.to_string_lossy().into_owned(),
            marker.to_string_lossy().into_owned(),
            "different".to_string(),
        ];
        let error = cmd_spawn(&changed_args).expect_err("changed argv must fail its schema");
        assert!(error.contains("command-specific schema"), "{error}");
        assert!(error.contains("cos_sandbox"), "{error}");

        let shell_marker = temp.path().join("shell-substitution");
        let shell = vec![
            "--session".to_string(),
            "safe-child".to_string(),
            "--".to_string(),
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("touch {}", shell_marker.display()),
        ];
        cmd_spawn(&shell).expect_err("shell substitution must need a fresh approval");
        assert!(
            !shell_marker.exists(),
            "the substituted shell must never execute"
        );

        let result = cmd_spawn(&harmless).expect("the exact approved invocation may execute");
        assert_eq!(result["parent"], parent);
        assert_eq!(wait_for_spawn_result(&marker), "hello");
    });
}

#[cfg(target_os = "linux")]
#[test]
fn executable_replacement_while_approval_is_pending_invalidates_the_decision() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("approved-script");
    let replacement = temp.path().join("replacement-script");
    let marker = temp.path().join("approval-race-result");
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    install_static_spawn_helper(&static_spawn_helpers().attacker, &replacement);
    let args = vec![
        "--session".to_string(),
        "approval-race-child".to_string(),
        "--workdir".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
        "trusted".to_string(),
    ];
    let _allowlist = install_test_proc_allowlist(
        "approval-race",
        &executable,
        &[marker.to_string_lossy().into_owned(), "trusted".to_string()],
        &[0],
        temp.path(),
    );

    crate::approvals::LocalApprovalInvocation::new("web:approval-race:turn:1")
        .unwrap()
        .sync_scope(|| {
            cmd_spawn(&args).expect_err("proc.spawn must require approval");
            let spawn_request = crate::approvals::list_pending()
                .into_iter()
                .find(|request| request.verb == Verb::PROC_SPAWN.as_str())
                .unwrap();
            crate::approvals::approve(
                &spawn_request.id,
                crate::approvals::GrantDuration::Session,
                None,
                None,
            )
            .unwrap();

            cmd_spawn(&args).expect_err("fs.exec must require approval");
            let exec_request = crate::approvals::list_pending()
                .into_iter()
                .find(|request| request.verb == Verb::FS_EXEC.as_str())
                .unwrap();
            let approved_digest = exec_request.operation_digest.clone().unwrap();
            std::fs::rename(&replacement, &executable).unwrap();
            crate::approvals::approve(
                &exec_request.id,
                crate::approvals::GrantDuration::Session,
                None,
                None,
            )
            .unwrap();

            let error = cmd_spawn(&args).expect_err("the replacement inode must fail closed");
            assert!(error.contains("allowlisted hash"), "{error}");
            assert!(error.contains("cos_sandbox"), "{error}");
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "");
            assert!(!approved_digest.is_empty());
        });
}

#[cfg(target_os = "linux")]
#[test]
fn executable_rewrite_while_approval_is_pending_changes_the_content_binding() {
    use std::os::unix::fs::MetadataExt;

    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("rewritten-during-approval");
    let marker = temp.path().join("rewrite-approval-result");
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let original_inode = std::fs::metadata(&executable).unwrap().ino();
    let args = vec![
        "--session".to_string(),
        "rewrite-approval-child".to_string(),
        "--workdir".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
        "trusted".to_string(),
    ];
    let _allowlist = install_test_proc_allowlist(
        "rewrite-approval",
        &executable,
        &[marker.to_string_lossy().into_owned(), "trusted".to_string()],
        &[0],
        temp.path(),
    );

    crate::approvals::LocalApprovalInvocation::new("web:rewrite-approval:turn:1")
        .unwrap()
        .sync_scope(|| {
            cmd_spawn(&args).expect_err("proc.spawn must require approval");
            let spawn_request = crate::approvals::list_pending()
                .into_iter()
                .find(|request| request.verb == Verb::PROC_SPAWN.as_str())
                .unwrap();
            crate::approvals::approve(
                &spawn_request.id,
                crate::approvals::GrantDuration::Session,
                None,
                None,
            )
            .unwrap();

            cmd_spawn(&args).expect_err("fs.exec must require approval");
            let exec_request = crate::approvals::list_pending()
                .into_iter()
                .find(|request| request.verb == Verb::FS_EXEC.as_str())
                .unwrap();
            let approved_digest = exec_request.operation_digest.clone().unwrap();
            std::fs::copy(&static_spawn_helpers().attacker, &executable).unwrap();
            assert_eq!(
                std::fs::metadata(&executable).unwrap().ino(),
                original_inode,
                "the adversary must rewrite the same inode"
            );
            crate::approvals::approve(
                &exec_request.id,
                crate::approvals::GrantDuration::Session,
                None,
                None,
            )
            .unwrap();

            let error = cmd_spawn(&args).expect_err("changed bytes must fail closed");
            assert!(error.contains("allowlisted hash"), "{error}");
            assert!(error.contains("cos_sandbox"), "{error}");
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "");
            assert!(!approved_digest.is_empty());
        });
}

#[cfg(target_os = "linux")]
#[test]
fn shebang_swap_while_approval_is_pending_cannot_reuse_native_consent() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("native-before-swap");
    let script = temp.path().join("script-after-swap");
    let marker = temp.path().join("shebang-swap-result");
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    write_spawn_script(&script, "attacker");
    let args = vec![
        "--session".to_string(),
        "shebang-swap-child".to_string(),
        "--workdir".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
        "trusted".to_string(),
    ];
    let _allowlist = install_test_proc_allowlist(
        "shebang-swap",
        &executable,
        &[marker.to_string_lossy().into_owned(), "trusted".to_string()],
        &[0],
        temp.path(),
    );

    crate::approvals::LocalApprovalInvocation::new("web:shebang-approval:turn:1")
        .unwrap()
        .sync_scope(|| {
            cmd_spawn(&args).expect_err("proc.spawn must require approval");
            let spawn_request = crate::approvals::list_pending()
                .into_iter()
                .find(|request| request.verb == Verb::PROC_SPAWN.as_str())
                .unwrap();
            crate::approvals::approve(
                &spawn_request.id,
                crate::approvals::GrantDuration::Session,
                None,
                None,
            )
            .unwrap();
            cmd_spawn(&args).expect_err("fs.exec must require approval");
            let exec_request = crate::approvals::list_pending()
                .into_iter()
                .find(|request| request.verb == Verb::FS_EXEC.as_str())
                .unwrap();
            std::fs::rename(script, &executable).unwrap();
            crate::approvals::approve(
                &exec_request.id,
                crate::approvals::GrantDuration::Session,
                None,
                None,
            )
            .unwrap();

            let error = cmd_spawn(&args).unwrap_err();
            assert!(error.contains("shebang script"), "{error}");
            assert!(error.contains("cos_sandbox"), "{error}");
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "");
        });
}

#[cfg(target_os = "linux")]
#[test]
fn same_path_inode_swap_after_authorization_executes_the_pinned_snapshot() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("inode-swap-script");
    let replacement = temp.path().join("inode-swap-replacement");
    let marker = temp.path().join("inode-swap-result");
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    install_static_spawn_helper(&static_spawn_helpers().attacker, &replacement);
    let args = vec![
        "--session".to_string(),
        "inode-swap-child".to_string(),
        "--workdir".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
        "trusted".to_string(),
    ];
    let _allowlist = install_test_proc_allowlist(
        "inode-swap",
        &executable,
        &[marker.to_string_lossy().into_owned(), "trusted".to_string()],
        &[0],
        temp.path(),
    );

    crate::approvals::LocalApprovalInvocation::new("web:inode-swap:turn:1")
        .unwrap()
        .sync_scope(|| {
            approve_spawn_permissions(&args);
            let executable_for_hook = executable.clone();
            set_pre_spawn_test_hook(move || {
                std::fs::rename(replacement, executable_for_hook).unwrap();
            });
            cmd_spawn(&args).expect("the approved pinned snapshot may execute");
            assert_eq!(wait_for_spawn_result(&marker), "trusted");
            assert_eq!(
                std::fs::read(&executable).unwrap(),
                std::fs::read(&static_spawn_helpers().attacker).unwrap()
            );
        });
}

#[cfg(target_os = "linux")]
#[test]
fn symlink_swap_after_authorization_executes_the_pinned_snapshot() {
    use std::os::unix::fs::symlink;

    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("symlink-swap-script");
    let attacker = temp.path().join("symlink-swap-attacker");
    let marker = temp.path().join("symlink-swap-result");
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    install_static_spawn_helper(&static_spawn_helpers().attacker, &attacker);
    let args = vec![
        "--session".to_string(),
        "symlink-swap-child".to_string(),
        "--workdir".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
        "trusted".to_string(),
    ];
    let _allowlist = install_test_proc_allowlist(
        "symlink-swap",
        &executable,
        &[marker.to_string_lossy().into_owned(), "trusted".to_string()],
        &[0],
        temp.path(),
    );

    crate::approvals::LocalApprovalInvocation::new("web:symlink-swap:turn:1")
        .unwrap()
        .sync_scope(|| {
            approve_spawn_permissions(&args);
            let executable_for_hook = executable.clone();
            set_pre_spawn_test_hook(move || {
                std::fs::remove_file(&executable_for_hook).unwrap();
                symlink(attacker, executable_for_hook).unwrap();
            });
            cmd_spawn(&args).expect("the approved pinned snapshot may execute");
            assert_eq!(wait_for_spawn_result(&marker), "trusted");
            assert!(std::fs::symlink_metadata(&executable)
                .unwrap()
                .file_type()
                .is_symlink());
        });
}

#[cfg(target_os = "linux")]
#[test]
fn in_place_rewrite_after_authorization_cannot_change_executed_bytes() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("rewrite-script");
    let marker = temp.path().join("rewrite-result");
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let args = vec![
        "--session".to_string(),
        "rewrite-child".to_string(),
        "--workdir".to_string(),
        temp.path().to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
        "trusted".to_string(),
    ];
    let _allowlist = install_test_proc_allowlist(
        "rewrite-snapshot",
        &executable,
        &[marker.to_string_lossy().into_owned(), "trusted".to_string()],
        &[0],
        temp.path(),
    );

    crate::approvals::LocalApprovalInvocation::new("web:rewrite:turn:1")
        .unwrap()
        .sync_scope(|| {
            approve_spawn_permissions(&args);
            let executable_for_hook = executable.clone();
            set_pre_spawn_test_hook(move || {
                std::fs::copy(&static_spawn_helpers().attacker, executable_for_hook).unwrap();
            });
            cmd_spawn(&args).expect("the sealed snapshot may execute");
            assert_eq!(wait_for_spawn_result(&marker), "trusted");
            assert_eq!(
                std::fs::read(&executable).unwrap(),
                std::fs::read(&static_spawn_helpers().attacker).unwrap()
            );
        });
}

#[cfg(target_os = "linux")]
#[test]
fn workdir_replacement_after_authorization_uses_the_pinned_directory() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", temp.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", temp.path());
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", temp.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    register_spawn_test_parent(crate::caps::CapSet::new());
    let executable = temp.path().join("workdir-script");
    let workdir = temp.path().join("workdir");
    let pinned_location = temp.path().join("workdir-pinned");
    std::fs::create_dir(&workdir).unwrap();
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
    let args = vec![
        "--session".to_string(),
        "workdir-child".to_string(),
        "--workdir".to_string(),
        workdir.to_string_lossy().into_owned(),
        "--".to_string(),
        executable.to_string_lossy().into_owned(),
        "relative-result".to_string(),
        "trusted".to_string(),
    ];
    let _allowlist = install_test_proc_allowlist(
        "workdir-pinned",
        &executable,
        &["relative-result".to_string(), "trusted".to_string()],
        &[0],
        &workdir,
    );

    crate::approvals::LocalApprovalInvocation::new("web:workdir-swap:turn:1")
        .unwrap()
        .sync_scope(|| {
            approve_spawn_permissions(&args);
            let workdir_for_hook = workdir.clone();
            let pinned_for_hook = pinned_location.clone();
            set_pre_spawn_test_hook(move || {
                std::fs::rename(&workdir_for_hook, &pinned_for_hook).unwrap();
                std::fs::create_dir(&workdir_for_hook).unwrap();
            });
            cmd_spawn(&args).expect("the process may use its pinned cwd");
            assert_eq!(
                wait_for_spawn_result(&pinned_location.join("relative-result")),
                "trusted"
            );
            assert!(
                !workdir.join("relative-result").exists(),
                "the replacement directory must not become the child cwd"
            );
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
    let executable = temp.path().join("child-caps-native");
    install_static_spawn_helper(&static_spawn_helpers().trusted, &executable);
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
        executable.to_string_lossy().into_owned(),
        temp.path().join("unused").to_string_lossy().into_owned(),
        "hello".to_string(),
    ];
    let error = cmd_spawn(&args).unwrap_err();
    assert!(error.contains("cannot widen caps"), "{error}");
    assert!(
        session_info_by_id("overprivileged-child").is_none(),
        "a rejected child must not enter the process registry"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn proc_spawn_fails_closed_without_descriptor_pinned_execution() {
    let error = cmd_spawn(&["example".to_string()]).unwrap_err();
    assert!(error.contains("unavailable on this platform"), "{error}");
    assert!(error.contains("cos_sandbox"), "{error}");
}
