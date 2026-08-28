use super::*;

#[test]
fn build_command_uses_env_override() {
    std::env::set_var("CLAW_COS_BIN", "/tmp/fake-cos");
    let cmd = build_command("fs", "ls", &["/tmp"]);
    assert_eq!(cmd.get_program(), "/tmp/fake-cos");
    let argv: Vec<&OsStr> = cmd.get_args().collect();
    assert_eq!(argv, &["app", "fs", "ls", "/tmp"]);
    std::env::remove_var("CLAW_COS_BIN");
}

#[test]
fn build_command_default_is_path_lookup() {
    std::env::remove_var("CLAW_COS_BIN");
    let cmd = build_command("fs", "ls", std::iter::empty::<&str>());
    assert_eq!(cmd.get_program(), "cos");
}

/// Fake `cos` binary that emits a fixed JSON object so we can
/// exercise the parsing path without a real backend. Used by
/// several integration-style tests.
fn write_fake_cos(dir: &std::path::Path, json: &str) -> std::path::PathBuf {
    let script = dir.join("cos");
    std::fs::write(&script, format!("#!/bin/sh\ncat <<'EOF'\n{json}\nEOF\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    script
}

#[test]
#[cfg(unix)]
fn call_parses_success_json() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake_cos(dir.path(), r#"{"hello":"world","n":3}"#);
    std::env::set_var("CLAW_COS_BIN", &bin);

    let v = call("noop", "ping", std::iter::empty::<&str>(), None).unwrap();
    assert_eq!(v["hello"], "world");
    assert_eq!(v["n"], 3);

    std::env::remove_var("CLAW_COS_BIN");
}

#[test]
#[cfg(unix)]
fn call_surfaces_error_field_as_app_error() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake_cos(
        dir.path(),
        r#"{"error":"file not found: /x","code":"not-found"}"#,
    );
    std::env::set_var("CLAW_COS_BIN", &bin);

    let err = call("fs", "read", &["/x"], None).unwrap_err();
    match err {
        BridgeError::AppError {
            app,
            verb,
            message,
            code,
        } => {
            assert_eq!(app, "fs");
            assert_eq!(verb, "read");
            assert_eq!(message, "file not found: /x");
            assert_eq!(code.as_deref(), Some("not-found"));
        }

        other => panic!("expected AppError, got {other:?}"),
    }

    std::env::remove_var("CLAW_COS_BIN");
}

#[test]
#[cfg(target_os = "linux")]
fn sensitive_call_timeout_kills_and_reaps_cos() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let pid_file = dir.path().join("pid");
    let script = dir.path().join("slow-cos");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 30\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("CLAW_COS_BIN", &script);

    let started = Instant::now();
    let input = vec![b'x'; 128 * 1024];
    let error = call_sensitive_with_timeout(
        "exec",
        "start",
        ["--stdin", "--", "program"],
        &input,
        Duration::from_millis(100),
    )
    .unwrap_err();
    std::env::remove_var("CLAW_COS_BIN");

    assert!(matches!(error, BridgeError::Timeout { .. }));
    assert!(started.elapsed() < Duration::from_secs(5));
    let pid = std::fs::read_to_string(pid_file).unwrap();
    let process_path = std::path::Path::new("/proc").join(pid);
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_path.exists(),
        "timed-out cos process was not reaped"
    );
}

#[test]
fn is_denied_recognises_denied_code() {
    let err = BridgeError::AppError {
        app: "fs".into(),
        verb: "write".into(),
        message: "permission denied".into(),
        code: Some("denied".into()),
    };
    assert!(err.is_denied());
}
