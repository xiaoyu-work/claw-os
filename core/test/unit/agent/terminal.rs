use super::*;

fn parse(args: &[&str]) -> Result<ChatOptions, String> {
    ChatOptions::parse(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
}

#[test]
fn terminal_is_default_only_when_all_streams_are_interactive() {
    let options = parse(&[]).unwrap();
    assert!(options.use_tui(true, false).unwrap());
    assert!(!options.use_tui(false, false).unwrap());
    assert!(!options.use_tui(true, true).unwrap());
}

#[test]
fn plain_mode_and_existing_line_output_flags_keep_the_repl() {
    for args in [
        &["--plain"][..],
        &["--no-stream"][..],
        &["--show-tools"][..],
    ] {
        let options = parse(args).unwrap();
        assert!(!options.use_tui(true, false).unwrap());
    }
}

#[test]
fn explicit_tui_refuses_unusable_terminals() {
    let options = parse(&["--tui"]).unwrap();
    assert!(options.use_tui(false, false).is_err());
    assert!(options.use_tui(true, true).is_err());
    assert!(parse(&["--tui", "--plain"]).is_err());
    assert!(parse(&["--plain", "--tui"]).is_err());
    assert!(parse(&["--tui", "--no-stream"]).is_err());
    assert!(parse(&["--tui", "--show-tools"]).is_err());
}

#[test]
fn conversation_options_keep_their_values() {
    let options = parse(&[
        "--session",
        "existing-session",
        "--no-memory",
        "--max-turns",
        "12",
    ])
    .unwrap();
    assert_eq!(options.session_id.as_deref(), Some("existing-session"));
    assert!(options.no_memory);
    assert_eq!(options.max_turns, Some(12));
}

#[test]
fn malformed_options_fail_before_starting_a_process() {
    for args in [
        &["--session"][..],
        &["--session", ""][..],
        &["--max-turns"][..],
        &["--max-turns", "lots"][..],
        &["--definitely-not-real"][..],
        &["--app", "mail"][..],
    ] {
        assert!(parse(args).is_err(), "{args:?}");
    }
}

#[test]
fn frontend_arguments_cannot_replace_the_claw_backend() {
    for flag in [
        "--remote",
        "--remote-transport",
        "--remote-auth-token-env",
        "--cd",
        "-C",
        "--sandbox",
        "-s",
        "--ask-for-approval",
        "-a",
        "--full-auto",
        "--dangerously-bypass-approvals-and-sandbox",
    ] {
        assert!(parse(&["--", flag, "other"]).is_err());
        assert!(parse(&["--", &format!("{flag}=other")]).is_err());
    }
    assert!(parse(&["--", "--", "another-command"]).is_err());
    let options = parse(&["--", "--no-alt-screen"]).unwrap();
    assert_eq!(options.frontend_args, vec!["--no-alt-screen"]);
    assert!(options.use_tui(false, false).is_err());
    assert!(parse(&["--plain", "--", "--no-alt-screen"]).is_err());
}

#[test]
fn frontend_credentials_and_telemetry_are_not_inherited() {
    for name in [
        "OPENAI_API_KEY",
        "OPENAI_FEDERATION_RULE_ID",
        "OPENAI_IDENTITY_TOKEN_FILE",
        "OPENAI_WORKLOAD_IDENTITY_CONTEXT",
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
        "CODEX_EXEC_SERVER_URL",
        "CODEX_EXEC_SERVER_NOISE_AUTH_TOKEN",
        "OTEL_EXPORTER_OTLP_HEADERS",
    ] {
        assert!(
            private_backend_environment(std::ffi::OsStr::new(name)),
            "{name}"
        );
    }
    for name in ["TERM", "EDITOR", "DISPLAY", "WAYLAND_DISPLAY", "CODEX_HOME"] {
        assert!(
            !private_backend_environment(std::ffi::OsStr::new(name)),
            "{name}"
        );
    }
}

#[test]
fn managed_frontend_settings_cannot_be_silently_overridden() {
    for args in [
        vec!["--", "--config", "web_search=\"live\""],
        vec!["--", "-c", "cli_auth_credentials_store=\"file\""],
        vec!["--", "--config=analytics.enabled=true"],
        vec!["--", "-cotel.exporter=\"otlp\""],
        vec!["--", "--config", "otel={exporter=\"otlp\"}"],
        vec!["--", "--config"],
    ] {
        assert!(parse(&args).is_err(), "{args:?}");
    }
    assert!(parse(&["--", "--config", "tui.status_line=[\"model\"]"]).is_ok());
}

#[cfg(unix)]
#[test]
fn executable_must_be_a_real_executable_file() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    assert!(require_executable(directory.path()).is_err());
    assert!(require_executable(&directory.path().join("missing")).is_err());
    let file = directory.path().join("frontend");
    std::fs::write(&file, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(require_executable(&file).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        require_executable(&file).unwrap(),
        std::fs::canonicalize(&file).unwrap()
    );
}

#[cfg(unix)]
async fn launch_probe(exit_code: u8) -> (tempfile::TempDir, Result<Value, String>) {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("frontend-state");
    std::fs::create_dir(&home).unwrap();
    let frontend = directory.path().join("frontend");
    std::fs::write(
        &frontend,
        format!(        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CODEX_HOME/arguments\"\npwd > \"$CODEX_HOME/cwd\"\nexit {exit_code}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&frontend, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = crate::config::CosConfig::default();
    config.agent.provider = "mock".into();
    config.agent.model = "mock".into();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_async(
            frontend,
            home,
            std::sync::Arc::new(config),
            parse(&["--", "--no-alt-screen"]).unwrap(),
        ),
    )
    .await
    .expect("frontend exit must stop the listener even without a connection");
    (directory, result)
}

#[cfg(unix)]
#[tokio::test]
async fn frontend_exit_reaps_the_private_listener_and_preserves_backend_selection() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let (directory, result) = launch_probe(0).await;
    assert!(result.unwrap().is_null());
    let args = std::fs::read_to_string(directory.path().join("frontend-state/arguments")).unwrap();
    let args = args.lines().collect::<Vec<_>>();
    assert_eq!(args[0], "--remote");
    let socket = args[1]
        .strip_prefix("unix://")
        .expect("local Unix endpoint");
    assert!(
        !Path::new(socket).exists(),
        "private socket must be removed"
    );
    assert!(args.contains(&"--no-alt-screen"));
    for setting in FRONTEND_CONFIG {
        assert!(args.contains(setting));
    }
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--ask-for-approval", "on-request"]));
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--sandbox", "danger-full-access"]));
    let cwd = std::fs::read_to_string(directory.path().join("frontend-state/cwd")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()),
        crate::paths::verified_home_for_uid(unsafe { libc::geteuid() }).unwrap()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn frontend_failure_is_not_replaced_by_another_backend() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let (_directory, result) = launch_probe(7).await;
    assert!(result.unwrap_err().contains("7"));
}
