use super::*;

use std::sync::Once;
static PERMS_INIT: Once = Once::new();
fn perms_init() {
    PERMS_INIT.call_once(|| std::env::set_var("COS_PERMS_MODE", "permissive"));
}

#[test]
fn test_default_values() {
    perms_init();
    assert_eq!(default_interval(), 10);
    assert_eq!(default_timeout(), 5);
    assert_eq!(default_grace(), 15);
    assert_eq!(default_restart(), "on-failure");
}

#[test]
fn test_deserialize_service_def_minimal() {
    perms_init();
    let json = r#"{"name": "test", "command": "echo hello"}"#;
    let def: ServiceDef = serde_json::from_str(json).unwrap();
    assert_eq!(def.name, "test");
    assert_eq!(def.command, "echo hello");
    assert_eq!(def.description, "");
    assert!(def.workdir.is_none());
    assert!(def.env.is_empty());
    assert!(def.health.is_none());
    assert_eq!(def.restart, "on-failure");
    assert!(def.depends_on.is_empty());
    assert!(def.lifecycle.is_none());
}

#[test]
fn test_deserialize_service_def_full() {
    perms_init();
    let json = r#"{
            "name": "browser",
            "description": "cos-browser CDP server",
            "command": "cos-browser serve --port 9222",
            "workdir": "/var/lib/cos/browser",
            "env": {"KEY": "val"},
            "health": {
                "url": "http://localhost:9222/json/version",
                "interval_secs": 5,
                "timeout_secs": 2,
                "start_grace_secs": 30
            },
            "restart": "always",
            "depends_on": ["redis"]
        }"#;
    let def: ServiceDef = serde_json::from_str(json).unwrap();
    assert_eq!(def.name, "browser");
    assert_eq!(def.description, "cos-browser CDP server");
    assert_eq!(def.command, "cos-browser serve --port 9222");
    assert_eq!(def.workdir.as_deref(), Some("/var/lib/cos/browser"));
    assert_eq!(def.env.get("KEY").unwrap(), "val");
    let h = def.health.unwrap();
    assert_eq!(h.url.as_deref(), Some("http://localhost:9222/json/version"));
    assert_eq!(h.interval_secs, 5);
    assert_eq!(h.timeout_secs, 2);
    assert_eq!(h.start_grace_secs, 30);
    assert_eq!(def.restart, "always");
    assert_eq!(def.depends_on, vec!["redis"]);
}

#[test]
fn test_deserialize_health_defaults() {
    perms_init();
    let json = r#"{"url": "http://localhost:8080"}"#;
    let h: HealthConfig = serde_json::from_str(json).unwrap();
    assert_eq!(h.url.as_deref(), Some("http://localhost:8080"));
    assert_eq!(h.interval_secs, 10);
    assert_eq!(h.timeout_secs, 5);
    assert_eq!(h.start_grace_secs, 15);
}

#[test]
fn test_unknown_command() {
    perms_init();
    let result = run("bogus", &[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown service command"));
}

#[test]
fn test_start_missing_name() {
    perms_init();
    let result = cmd_start(&[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("usage"));
}

#[test]
fn test_stop_missing_name() {
    perms_init();
    let result = cmd_stop(&[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("usage"));
}

#[test]
fn test_status_missing_name() {
    perms_init();
    let result = cmd_status(&[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("usage"));
}

#[test]
fn test_health_missing_name() {
    perms_init();
    let result = cmd_health(&[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("usage"));
}

#[test]
fn test_logs_missing_name() {
    perms_init();
    let result = cmd_logs(&[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("usage"));
}

#[test]
fn test_register_missing_args() {
    perms_init();
    let result = cmd_register(&[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("--name is required"));

    let result = cmd_register(&["--name".into(), "foo".into()]);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("--command is required"));
}

#[test]
fn test_register_invalid_name() {
    perms_init();
    let result = cmd_register(&[
        "--name".into(),
        "bad/name".into(),
        "--command".into(),
        "echo hi".into(),
    ]);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("alphanumeric"));
}

#[test]
fn test_serialize_roundtrip() {
    perms_init();
    let def = ServiceDef {
        name: "test-svc".into(),
        description: "A test service".into(),
        command: "echo hello".into(),
        workdir: Some("/tmp".into()),
        env: BTreeMap::from([("FOO".into(), "bar".into())]),
        health: Some(HealthConfig {
            url: Some("http://localhost:9090".into()),
            interval_secs: 10,
            timeout_secs: 5,
            start_grace_secs: 15,
        }),
        restart: "on-failure".into(),
        depends_on: vec!["dep1".into()],
        credentials: Vec::new(),
        lifecycle: Some(LifecycleHooks {
            pre_start: Some("echo pre".into()),
            post_start: None,
            pre_stop: Some("echo drain".into()),
            post_stop: Some("echo cleanup".into()),
            drain_timeout_secs: 3,
            stop_timeout_secs: 8,
            checkpoint_cmd: None,
        }),
    };

    let json_str = serde_json::to_string(&def).unwrap();
    let restored: ServiceDef = serde_json::from_str(&json_str).unwrap();
    assert_eq!(restored.name, def.name);
    assert_eq!(restored.command, def.command);
    assert_eq!(restored.env.get("FOO").unwrap(), "bar");
    let lc = restored.lifecycle.unwrap();
    assert_eq!(lc.pre_start.as_deref(), Some("echo pre"));
    assert!(lc.post_start.is_none());
    assert_eq!(lc.pre_stop.as_deref(), Some("echo drain"));
    assert_eq!(lc.post_stop.as_deref(), Some("echo cleanup"));
    assert_eq!(lc.drain_timeout_secs, 3);
    assert_eq!(lc.stop_timeout_secs, 8);
    assert!(lc.checkpoint_cmd.is_none());
}

#[test]
fn test_lifecycle_hooks_deserialization() {
    perms_init();
    // Empty object should use all defaults
    let json = r#"{}"#;
    let hooks: LifecycleHooks = serde_json::from_str(json).unwrap();
    assert!(hooks.pre_start.is_none());
    assert!(hooks.post_start.is_none());
    assert!(hooks.pre_stop.is_none());
    assert!(hooks.post_stop.is_none());
    assert_eq!(hooks.drain_timeout_secs, default_drain_timeout());
    assert_eq!(hooks.stop_timeout_secs, default_stop_timeout());
    assert!(hooks.checkpoint_cmd.is_none());

    // Full object
    let json = r#"{
            "pre_start": "run-migrations",
            "post_start": "register-discovery",
            "pre_stop": "drain-connections",
            "post_stop": "cleanup-tmp",
            "drain_timeout_secs": 15,
            "stop_timeout_secs": 30,
            "checkpoint_cmd": "save-state"
        }"#;
    let hooks: LifecycleHooks = serde_json::from_str(json).unwrap();
    assert_eq!(hooks.pre_start.as_deref(), Some("run-migrations"));
    assert_eq!(hooks.post_start.as_deref(), Some("register-discovery"));
    assert_eq!(hooks.pre_stop.as_deref(), Some("drain-connections"));
    assert_eq!(hooks.post_stop.as_deref(), Some("cleanup-tmp"));
    assert_eq!(hooks.drain_timeout_secs, 15);
    assert_eq!(hooks.stop_timeout_secs, 30);
    assert_eq!(hooks.checkpoint_cmd.as_deref(), Some("save-state"));
}

#[test]
fn test_lifecycle_defaults() {
    perms_init();
    assert_eq!(default_drain_timeout(), 5);
    assert_eq!(default_stop_timeout(), 10);
}

#[test]
fn test_run_hook_success() {
    perms_init();
    let result = run_hook("test_hook", "echo hello", 10);
    assert_eq!(result["step"], "test_hook");
    assert_eq!(result["status"], "ok");
    assert_eq!(result["exit_code"], 0);
    assert!(result["duration_ms"].as_u64().is_some());
    // stdout should contain "hello"
    let stdout = result["stdout"].as_str().unwrap_or("");
    assert!(stdout.contains("hello"), "stdout was: {stdout}");
}

#[test]
fn test_run_hook_failure() {
    perms_init();
    // Use a command that will fail
    #[cfg(unix)]
    let cmd = "sh -c 'echo fail-output >&2; exit 1'";
    #[cfg(not(unix))]
    let cmd = "cmd /c \"echo fail-output 1>&2 && exit /b 1\"";

    let result = run_hook("failing_hook", cmd, 10);
    assert_eq!(result["step"], "failing_hook");
    assert_eq!(result["status"], "failed");
    assert_ne!(result["exit_code"], 0);
    assert!(result["duration_ms"].as_u64().is_some());
}

#[test]
fn test_stop_all_dispatch() {
    perms_init();
    // Verify the stop-all command is routed correctly
    let result = run("stop-all", &[]);
    // Should succeed (no running services in test env)
    assert!(result.is_ok());
    let val = result.unwrap();
    assert_eq!(val["command"], "stop-all");
    assert!(val["stopped"].as_u64().is_some());
    assert!(val["results"].is_array());
}

#[test]
fn test_register_with_lifecycle() {
    perms_init();
    // Use a temp services dir for this test
    let tmp_dir = std::env::temp_dir().join("cos-test-register-lifecycle");
    let _ = fs::remove_dir_all(&tmp_dir);
    let _ = fs::create_dir_all(&tmp_dir);
    std::env::set_var("COS_SERVICES_DIR", tmp_dir.to_string_lossy().as_ref());

    let result = cmd_register(&[
        "--name".into(),
        "lifecycle-test".into(),
        "--command".into(),
        "echo hi".into(),
        "--pre-start".into(),
        "echo pre-start".into(),
        "--post-start".into(),
        "echo post-start".into(),
        "--pre-stop".into(),
        "echo draining".into(),
        "--post-stop".into(),
        "echo cleanup".into(),
        "--drain-timeout".into(),
        "3".into(),
        "--stop-timeout".into(),
        "15".into(),
        "--checkpoint-cmd".into(),
        "echo saving".into(),
    ]);
    assert!(result.is_ok(), "register failed: {:?}", result);
    let val = result.unwrap();
    assert_eq!(val["registered"], "lifecycle-test");

    // Verify the written service.json includes lifecycle hooks
    let manifest_path = tmp_dir.join("lifecycle-test").join("service.json");
    let data = fs::read_to_string(&manifest_path).unwrap();
    let def: ServiceDef = serde_json::from_str(&data).unwrap();
    let lc = def.lifecycle.unwrap();
    assert_eq!(lc.pre_start.as_deref(), Some("echo pre-start"));
    assert_eq!(lc.post_start.as_deref(), Some("echo post-start"));
    assert_eq!(lc.pre_stop.as_deref(), Some("echo draining"));
    assert_eq!(lc.post_stop.as_deref(), Some("echo cleanup"));
    assert_eq!(lc.drain_timeout_secs, 3);
    assert_eq!(lc.stop_timeout_secs, 15);
    assert_eq!(lc.checkpoint_cmd.as_deref(), Some("echo saving"));

    // Cleanup
    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_reverse_dependency_order() {
    perms_init();
    let mut services = BTreeMap::new();
    services.insert(
        "db".into(),
        ServiceDef {
            name: "db".into(),
            description: String::new(),
            command: "echo db".into(),
            workdir: None,
            env: BTreeMap::new(),
            health: None,
            restart: "on-failure".into(),
            depends_on: vec![],
            credentials: Vec::new(),
            lifecycle: None,
        },
    );
    services.insert(
        "api".into(),
        ServiceDef {
            name: "api".into(),
            description: String::new(),
            command: "echo api".into(),
            workdir: None,
            env: BTreeMap::new(),
            health: None,
            restart: "on-failure".into(),
            depends_on: vec!["db".into()],
            credentials: Vec::new(),
            lifecycle: None,
        },
    );
    services.insert(
        "web".into(),
        ServiceDef {
            name: "web".into(),
            description: String::new(),
            command: "echo web".into(),
            workdir: None,
            env: BTreeMap::new(),
            health: None,
            restart: "on-failure".into(),
            depends_on: vec!["api".into()],
            credentials: Vec::new(),
            lifecycle: None,
        },
    );

    let order = reverse_dependency_order(&services);
    // web depends on api, api depends on db
    // Shutdown order should be: web, api, db
    let web_pos = order.iter().position(|n| n == "web").unwrap();
    let api_pos = order.iter().position(|n| n == "api").unwrap();
    let db_pos = order.iter().position(|n| n == "db").unwrap();
    assert!(
        web_pos < api_pos,
        "web should stop before api: web={web_pos}, api={api_pos}"
    );
    assert!(
        api_pos < db_pos,
        "api should stop before db: api={api_pos}, db={db_pos}"
    );
}

#[test]
fn test_service_def_with_lifecycle_field() {
    perms_init();
    let json = r#"{
            "name": "agent",
            "command": "python agent.py",
            "lifecycle": {
                "pre_start": "python migrate.py",
                "post_stop": "rm -rf /tmp/agent-*",
                "drain_timeout_secs": 10,
                "stop_timeout_secs": 20
            }
        }"#;
    let def: ServiceDef = serde_json::from_str(json).unwrap();
    assert_eq!(def.name, "agent");
    let lc = def.lifecycle.unwrap();
    assert_eq!(lc.pre_start.as_deref(), Some("python migrate.py"));
    assert!(lc.post_start.is_none());
    assert!(lc.pre_stop.is_none());
    assert_eq!(lc.post_stop.as_deref(), Some("rm -rf /tmp/agent-*"));
    assert_eq!(lc.drain_timeout_secs, 10);
    assert_eq!(lc.stop_timeout_secs, 20);
    assert!(lc.checkpoint_cmd.is_none());
}

#[test]
fn test_service_def_with_credentials() {
    perms_init();
    let json = r#"{
            "name": "my-agent",
            "command": "python agent.py",
            "credentials": ["OPENAI_KEY", "DB_URL"]
        }"#;
    let def: ServiceDef = serde_json::from_str(json).unwrap();
    assert_eq!(def.credentials, vec!["OPENAI_KEY", "DB_URL"]);
}

#[test]
fn test_service_def_without_credentials() {
    perms_init();
    let json = r#"{"name": "test", "command": "echo hi"}"#;
    let def: ServiceDef = serde_json::from_str(json).unwrap();
    assert!(def.credentials.is_empty());
}

// -----------------------------------------------------------------
// parse_command (shlex-style)
// -----------------------------------------------------------------

#[test]
fn parse_command_simple_whitespace() {
    perms_init();
    let argv = parse_command("echo hello world").unwrap();
    assert_eq!(argv, vec!["echo", "hello", "world"]);
}

#[test]
fn parse_command_collapses_runs_of_whitespace() {
    perms_init();
    let argv = parse_command("  a    b\tc \n d ").unwrap();
    assert_eq!(argv, vec!["a", "b", "c", "d"]);
}

#[test]
fn parse_command_double_quoted_string_is_one_arg() {
    perms_init();
    let argv = parse_command(r#"node server.js --msg "hello world""#).unwrap();
    assert_eq!(argv, vec!["node", "server.js", "--msg", "hello world"]);
}

#[test]
fn parse_command_single_quoted_string_is_literal() {
    perms_init();
    // Single quotes do NOT process escapes — \n inside '…'
    // stays as a literal backslash + n.
    let argv = parse_command(r#"echo 'a\nb $HOME "still literal"'"#).unwrap();
    assert_eq!(argv, vec!["echo", r#"a\nb $HOME "still literal""#]);
}

#[test]
fn parse_command_double_quote_escapes() {
    perms_init();
    // Inside dquotes only \", \\, \$, \` are special escapes; the
    // backslash is preserved before anything else.
    let argv = parse_command(r#"x "a\"b" "c\\d" "e\nf""#).unwrap();
    assert_eq!(argv, vec!["x", "a\"b", r"c\d", r"e\nf"]);
}

#[test]
fn parse_command_backslash_outside_quotes_escapes_one_char() {
    perms_init();
    let argv = parse_command(r"echo a\ b c").unwrap();
    // `\ ` should produce a literal space inside the arg.
    assert_eq!(argv, vec!["echo", "a b", "c"]);
}

#[test]
fn parse_command_rejects_unterminated_single_quote() {
    perms_init();
    let err = parse_command("echo 'unterminated").unwrap_err();
    assert!(err.contains("single quote"), "{err}");
}

#[test]
fn parse_command_rejects_unterminated_double_quote() {
    perms_init();
    let err = parse_command(r#"echo "unterminated"#).unwrap_err();
    assert!(err.contains("double quote"), "{err}");
}

#[test]
fn parse_command_rejects_trailing_backslash() {
    perms_init();
    let err = parse_command("echo hi \\").unwrap_err();
    assert!(err.contains("trailing backslash"), "{err}");
}

#[test]
fn parse_command_empty_input_yields_empty_argv() {
    perms_init();
    assert_eq!(parse_command("").unwrap(), Vec::<String>::new());
    assert_eq!(parse_command("   \t\n  ").unwrap(), Vec::<String>::new());
}

#[test]
fn parse_command_flag_value_with_quoted_path() {
    perms_init();
    // Regression: the audit's example
    //   --root-path "/api/v1 of things"
    // should yield argv=["--root-path", "/api/v1 of things"],
    // not 4 separate tokens.
    let argv = parse_command(r#"app --root-path "/api/v1 of things" --port 8080"#).unwrap();
    assert_eq!(
        argv,
        vec!["app", "--root-path", "/api/v1 of things", "--port", "8080"]
    );
}

// -----------------------------------------------------------------
// PID-identity: read_pid format + recycle detection
// -----------------------------------------------------------------

/// Parser regression for the new `pid:starttime` format.
/// Verifies both forms round-trip without depending on the
/// `COS_DATA_DIR` env var (which is concurrently mutated by
/// other tests in this crate and races with file-IO setup).
#[test]
fn parse_pid_file_contents_handles_both_formats() {
    perms_init();
    // New format with starttime.
    let parsed = parse_pid_file_contents("12345:6789012\n").unwrap();
    assert_eq!(parsed, (12345, Some(6789012)));

    // Legacy format: pid only.
    let parsed = parse_pid_file_contents("42\n").unwrap();
    assert_eq!(parsed, (42, None));

    // Surrounding whitespace tolerated.
    let parsed = parse_pid_file_contents("  4242:99 \t\n").unwrap();
    assert_eq!(parsed, (4242, Some(99)));

    // Empty / blank file → None.
    assert!(parse_pid_file_contents("").is_none());
    assert!(parse_pid_file_contents("   \n").is_none());

    // Malformed pid → None (don't panic, don't return garbage).
    assert!(parse_pid_file_contents("not-a-pid").is_none());
    assert!(parse_pid_file_contents("not-a-pid:42").is_none());

    // Malformed starttime → pid is still returned, starttime is None.
    let parsed = parse_pid_file_contents("123:not-a-starttime").unwrap();
    assert_eq!(parsed, (123, None));
}

/// Recycle regression: if the pid file records pid+starttime and
/// the pid is currently alive but with a DIFFERENT starttime,
/// `is_alive_with_start_time` must report exited. cmd_stop then
/// shortcircuits with status="pid_recycled" instead of
/// signalling.
#[cfg(target_os = "linux")]
#[test]
fn recycled_pid_must_not_be_treated_as_alive() {
    perms_init();
    let me = std::process::id();
    let real_start = crate::proc::read_start_time_ticks_pub(me)
        .expect("test must run on Linux with /proc readable");
    // Sanity: matching starttime passes.
    assert!(crate::proc::is_alive_with_start_time(me, Some(real_start)));
    // Recycled: starttime mismatch fails.
    assert!(
        !crate::proc::is_alive_with_start_time(me, Some(real_start.wrapping_add(1_000_000))),
        "mismatched starttime must be treated as exited"
    );
    // Legacy file (None): falls back to basic kill(pid,0) check
    // and reports alive for our own pid.
    assert!(crate::proc::is_alive_with_start_time(me, None));
}
