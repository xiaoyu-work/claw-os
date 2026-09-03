use super::*;

#[test]
fn build_command_uses_env_override() {
    std::env::set_var("CLAW_COS_BIN", "/tmp/fake-cos");
    let cmd = build_command("fs", "ls", &["/tmp"]);
    assert_eq!(cmd.get_program(), "/tmp/fake-cos");
    let argv: Vec<&OsStr> = cmd.get_args().collect();
    assert_eq!(argv, &["--wire=1", "app", "fs", "ls", "/tmp"]);
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
fn write_fake_cos(dir: &std::path::Path, json: &str, exit_code: i32) -> std::path::PathBuf {
    let script = dir.join("cos");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ncat <<'EOF'\n{json}\nEOF\nexit {exit_code}\n"),
    )
    .unwrap();
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
    let bin = write_fake_cos(
        dir.path(),
        r#"{"ok":true,"wire_version":1,"data":{"hello":"world","n":3}}"#,
        0,
    );
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
        r#"{"ok":false,"wire_version":1,"error":"file not found: /x","code":"RESOURCE_NOT_FOUND"}"#,
        1,
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
            assert_eq!(code, "RESOURCE_NOT_FOUND");
        }
        other => panic!("expected AppError, got {other:?}"),
    }

    std::env::remove_var("CLAW_COS_BIN");
}

#[test]
fn is_denied_recognises_denied_code() {
    let err = BridgeError::AppError {
        app: "fs".into(),
        verb: "write".into(),
        message: "permission denied".into(),
        code: "PERMISSION_DENIED".into(),
    };
    assert!(err.is_denied());
}

#[test]
#[cfg(unix)]
fn call_rejects_flat_and_status_incoherent_wire_replies() {
    let dir = tempfile::tempdir().unwrap();
    for (body, exit_code) in [
        (r#"{"hello":"world"}"#, 0),
        (
            r#"{"ok":true,"wire_version":1,"data":{"hello":"world"}}"#,
            1,
        ),
        (
            r#"{"ok":false,"wire_version":1,"error":"denied","code":"PERMISSION_DENIED"}"#,
            0,
        ),
    ] {
        let bin = write_fake_cos(dir.path(), body, exit_code);
        std::env::set_var("CLAW_COS_BIN", &bin);
        let error = call("noop", "ping", std::iter::empty::<&str>(), None).unwrap_err();
        assert!(
            matches!(error, BridgeError::Decode { .. }),
            "body {body} with exit {exit_code} returned {error:?}"
        );
    }
    std::env::remove_var("CLAW_COS_BIN");
}
