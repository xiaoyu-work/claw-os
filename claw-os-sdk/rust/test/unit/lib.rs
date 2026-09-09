use super::*;

#[cfg(unix)]
#[tokio::test]
async fn async_stdin_binary_is_bounded_and_keeps_business_text_out_of_argv() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let binary = dir.path().join("cos");
    let oversized = vec![b'x'; APP_ARGS_STDIN_MAX_BYTES + 1];
    assert!(matches!(cos_call_json_async_with_stdin_binary(
        &binary, "notification", "post", ["__notifications"], &oversized,
    ).await, Err(BridgeError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput));
    std::fs::write(&binary, concat!(
        "#!/usr/bin/python3\nimport sys,json\n",
        "assert sys.argv[1:]==['--wire=1','__notifications']\n",
        "data=json.load(sys.stdin)\n",
        "print(json.dumps({'wire_version':1,'ok':True,'data':data}))\n",
    )).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let result = cos_call_json_async_with_stdin_binary(
        &binary, "notification", "post", ["__notifications"], br#"{"body":"private text"}"#,
    ).await.unwrap();
    assert_eq!(result["body"], "private text");
    let error_binary = write_fake_cos(
        dir.path(), r#"{"ok":false,"wire_version":1,"error":"refused","code":"PERMISSION_DENIED"}"#, 1,
    );
    let error = cos_call_json_async_with_stdin_binary(
        error_binary, "notification", "post", ["__notifications"], &vec![b' '; 128 * 1024],
    ).await.unwrap_err();
    assert!(error.is_denied(), "an early refusal takes precedence over EPIPE: {error}");
}

#[cfg(unix)]
#[tokio::test]
async fn async_explicit_binary_preserves_shared_wire_decoding() {
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let missing = cos_call_json_async_with_binary(
        dir.path().join("missing"), "media-player", "status", ["__media-player", "status"],
    ).await;
    assert!(matches!(missing, Err(BridgeError::BinaryNotFound(_))));
    for (body, code, accepted) in [
        (r#"{"ok":true,"wire_version":1,"data":{"status":"Paused"}}"#, 0, true),
        (r#"{"ok":false,"wire_version":1,"error":"refused","code":"PERMISSION_DENIED"}"#, 1, false),
        (r#"{"status":"Paused"}"#, 0, false),
        (r#"{"ok":true,"wire_version":1,"data":{}}"#, 1, false),
    ] {
        let binary = write_fake_cos(dir.path(), body, code);
        let sync = cos_call_json_with_binary(&binary, "media-player", "status", ["__media-player", "status"]);
        let result = cos_call_json_async_with_binary(&binary, "media-player", "status", ["__media-player", "status"]).await;
        assert_eq!(result.is_ok(), accepted);
        assert_eq!(format!("{result:?}"), format!("{sync:?}"));
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn async_explicit_binary_cancellation_kills_and_reaps_the_cli() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let binary = dir.path().join("cos");
    let marker = dir.path().join("started");
    std::fs::write(&binary, "#!/usr/bin/python3\nimport os,sys,time\nassert sys.argv[1:3]==['--wire=1','probe']\nwith open(sys.argv[3], 'w') as out: out.write(str(os.getpid()))\ntime.sleep(60)\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = marker.clone();
    let task = tokio::spawn(async move {
        cos_call_json_async_with_binary(binary, "media-player", "status", ["probe", marker.to_str().unwrap()]).await
    });
    let seen = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(&started) {
                if let Ok(pid) = pid.parse::<u32>() { break pid; }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await;
    task.abort();
    let _ = task.await;
    let pid = seen.expect("CLI must start before cancellation");
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::path::Path::new(&format!("/proc/{pid}")).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("cancelled CLI must be killed and reaped");
}

#[cfg(unix)]
#[test]
fn explicit_binary_uses_shared_transport_without_path_or_override() {
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let original_bin = std::env::var_os("CLAW_COS_BIN");
    let original_path = std::env::var_os("PATH");
    std::env::remove_var("CLAW_COS_BIN");
    std::env::set_var("PATH", "/usr/bin:/bin");
    let binary = write_fake_cos(dir.path(), r#"{"ok":true,"wire_version":1,"data":{"selected":true}}"#, 0);
    let first = cos_call_json_with_binary(&binary, "permissions", "manage", ["__app-permissions", "{}"]);
    std::env::set_var("CLAW_COS_BIN", dir.path().join("not-created"));
    let second = cos_call_json_with_binary(&binary, "permissions", "manage", ["__app-permissions", "{}"]);
    let generic = cos_call_json("permissions", "manage", ["__app-permissions", "{}"]);
    for (name, previous) in [("CLAW_COS_BIN", original_bin), ("PATH", original_path)] {
        match previous {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
    assert_eq!(first.unwrap(), second.unwrap());
    assert!(matches!(generic, Err(BridgeError::BinaryNotFound(_))));
}

#[cfg(unix)]
#[test]
fn explicit_binary_preserves_wire_errors_and_spawn_failures() {
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let missing = cos_call_json_with_binary(
        dir.path().join("not-created"), "permissions", "manage", ["__app-permissions", "{}"],
    );
    assert!(matches!(missing, Err(BridgeError::BinaryNotFound(_))));
    for (body, code, denied) in [
        (r#"{"ok":false,"wire_version":1,"error":"refused","code":"PERMISSION_DENIED"}"#, 1, true),
        (r#"{"enabled":true}"#, 0, false),
        (r#"{"ok":true,"wire_version":1,"data":{}}"#, 1, false),
    ] {
        let binary = write_fake_cos(dir.path(), body, code);
        let error = cos_call_json_with_binary(
            binary, "permissions", "manage", ["__app-permissions", "{}"],
        ).unwrap_err();
        if denied {
            assert!(error.is_denied(), "{error}");
        } else {
            assert!(matches!(error, BridgeError::Decode { .. }), "{error}");
        }
    }
}

#[cfg(unix)]
#[test]
fn controlled_stdin_transports_share_bounds_and_wire_decoding() {
    let dir = tempfile::tempdir().unwrap();
    let original = std::env::var_os("CLAW_COS_BIN");
    std::env::set_var("CLAW_COS_BIN", dir.path().join("not-created"));
    let oversized = vec![b' '; APP_ARGS_STDIN_MAX_BYTES + 1];
    let ordinary = cos_call_json_with_stdin("capture", "screenshot", ["__capture"], &oversized);
    let terminal = cos_call_json_with_stdin_terminal("capture", "screenshot", ["__capture"], &oversized);
    let bin = write_fake_cos(
        dir.path(),
        r#"{"ok":true,"wire_version":1,"data":{"cancelled":true,"path":null}}"#,
        0,
    );
    std::env::set_var("CLAW_COS_BIN", bin);
    let response = cos_call_json_with_stdin("capture", "screenshot", ["__capture"], b"{}");
    let human = cos_call_json_with_stdin_terminal("capture", "screenshot", ["__capture"], b"{}");
    match original {
        Some(value) => std::env::set_var("CLAW_COS_BIN", value),
        None => std::env::remove_var("CLAW_COS_BIN"),
    }
    for error in [ordinary, terminal] {
        assert!(matches!(error, Err(BridgeError::Io(error))
            if error.kind() == std::io::ErrorKind::InvalidInput));
    }
    assert_eq!(response.unwrap(), human.unwrap());
}

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
#[cfg(unix)]
fn call_reaps_early_stdin_refusal_and_preserves_typed_error() {
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let response = serde_json::json!({
        "ok": false, "wire_version": 1,
        "error": "body rejected ".repeat(16 * 1024), "code": "INVALID_ARGUMENT"
    })
    .to_string();
    let bin = write_fake_cos(dir.path(), &response, 1);
    std::env::set_var("CLAW_COS_BIN", &bin);
    let error = call(
        "fs",
        "write",
        ["--args-stdin"],
        Some(&vec![b'x'; APP_ARGS_STDIN_MAX_BYTES]),
    )
    .unwrap_err();
    std::env::remove_var("CLAW_COS_BIN");
    assert!(matches!(error, BridgeError::AppError { code, .. } if code == "INVALID_ARGUMENT"));
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
