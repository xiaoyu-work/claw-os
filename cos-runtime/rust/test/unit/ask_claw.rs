use super::*;
use std::io::Cursor;
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize)]
struct TestContext<'a> {
    mode: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
}

impl Context for TestContext<'_> {
    const APP_ID: &'static str = "test-app";
}

struct TestEnvironment {
    _guard: MutexGuard<'static, ()>,
    directory: tempfile::TempDir,
}

impl TestEnvironment {
    fn new() -> Self {
        Self {
            _guard: ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            directory: tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap(),
        }
    }
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        for name in ["CLAW_COS_BIN", AGENT_UI_ENV, "ASK_CLAW_TEST_CAPTURE"] {
            std::env::remove_var(name);
        }
    }
}

#[test]
fn context_round_trips_and_matches_wire_shape() {
    let serialized = serialize_context(&TestContext {
        mode: "inspect",
        path: Some("C:\\quoted \"name\"\nfile"),
    })
    .unwrap();
    assert_eq!(
        serialized,
        r#"{"app":"test-app","mode":"inspect","path":"C:\\quoted \"name\"\nfile"}"#
    );

    let decoded: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    assert_eq!(decoded["app"], "test-app");
    assert_eq!(decoded["mode"], "inspect");
    assert_eq!(decoded["path"], "C:\\quoted \"name\"\nfile");
}

#[test]
fn context_at_limit_is_accepted_and_larger_context_is_rejected() {
    let baseline = serialize_context(&TestContext {
        mode: "",
        path: None,
    })
    .unwrap()
    .len();
    let at_limit = "x".repeat(MAX_CONTEXT_BYTES - baseline);
    assert_eq!(
        serialize_context(&TestContext {
            mode: &at_limit,
            path: None,
        })
        .unwrap()
        .len(),
        MAX_CONTEXT_BYTES
    );

    let over_limit = format!("{at_limit}x");
    assert!(matches!(
        serialize_context(&TestContext {
            mode: &over_limit,
            path: None,
        }),
        Err(ContextError::TooLarge {
            actual,
            limit: MAX_CONTEXT_BYTES
        }) if actual == MAX_CONTEXT_BYTES + 1
    ));
}

#[test]
fn launcher_selects_executable_and_uses_only_stdin_flag_in_arguments() {
    let _environment = TestEnvironment::new();
    std::env::set_var(AGENT_UI_ENV, "/opt/claw/custom-agent-ui");
    assert_eq!(
        launch_argv(),
        [
            "/opt/claw/custom-agent-ui",
            "--overlay",
            "--context-stdin",
            "--ready-fd",
            "3",
        ]
    );
}

#[test]
fn launcher_uses_path_lookup_by_default() {
    let _environment = TestEnvironment::new();
    assert_eq!(agent_ui_executable(), DEFAULT_AGENT_UI);
}

#[test]
fn ui_stdin_activation_decodes_without_an_argv_payload() {
    let expected = Activation::overlay_with_context(
        serialize_context(&TestContext {
            mode: "inspect",
            path: Some("/private/input"),
        })
        .unwrap(),
    );
    let payload = serde_json::to_vec(&expected).unwrap();
    let parsed = parse_ui_arguments(["--overlay", "--context-stdin", "--future"]);
    assert_eq!(parsed.unknown, ["--future"]);

    let activation = parsed.activation(Cursor::new(payload)).unwrap().unwrap();
    assert_eq!(activation, expected);
}

#[test]
fn ui_stdin_rejects_conflicting_context_without_inline_fallback() {
    let parsed = parse_ui_arguments([
        "--overlay",
        "--context-stdin",
        "--context",
        r#"{"app":"legacy"}"#,
    ]);
    let stdin = serde_json::to_vec(&Activation::overlay_with_context(
        r#"{"app":"stdin"}"#.to_string(),
    ))
    .unwrap();

    assert!(matches!(
        parsed.activation(Cursor::new(stdin)),
        Err(ActivationInputError::ConflictingContext)
    ));
}

#[test]
fn ui_stdin_rejects_oversize_malformed_and_invalid_context() {
    let parsed = parse_ui_arguments(["--overlay", "--context-stdin"]);
    assert!(matches!(
        parsed.activation(Cursor::new(vec![b'x'; MAX_ACTIVATION_BYTES + 1])),
        Err(ActivationInputError::TooLarge { .. })
    ));
    assert!(matches!(
        parsed.activation(Cursor::new(b"{not-json")),
        Err(ActivationInputError::Malformed(_))
    ));

    let invalid_context = serde_json::to_vec(&Activation::overlay_with_context(
        r#"{"mode":"missing-app"}"#.to_string(),
    ))
    .unwrap();
    assert!(matches!(
        parsed.activation(Cursor::new(invalid_context)),
        Err(ActivationInputError::InvalidContext(_))
    ));
}

#[test]
fn payload_bearing_argv_is_rejected_without_reading_stdin() {
    struct PanicReader;

    impl Read for PanicReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("legacy activation must not read stdin");
        }
    }

    let parsed = parse_ui_arguments(["--overlay", "--context", r#"{"app":"legacy"}"#]);
    assert!(matches!(
        parsed.activation(PanicReader),
        Err(ActivationInputError::ProhibitedArgument("--context"))
    ));

    let parsed = parse_ui_arguments(["--overlay", "--query", "private prompt"]);
    assert!(matches!(
        parsed.activation(PanicReader),
        Err(ActivationInputError::ProhibitedArgument("--query"))
    ));
}

#[test]
fn reserved_app_field_is_rejected() {
    #[derive(Serialize)]
    struct InvalidContext {
        app: &'static str,
    }

    impl Context for InvalidContext {
        const APP_ID: &'static str = "test-app";
    }

    assert!(matches!(
        serialize_context(&InvalidContext { app: "spoofed" }),
        Err(ContextError::ReservedAppField { app: "test-app" })
    ));
}

#[test]
fn process_spawn_failures_are_preserved() {
    let _environment = TestEnvironment::new();
    std::env::set_var(
        AGENT_UI_ENV,
        "/nonexistent/definitely-not-a-real-agent-ui-issue-47",
    );
    let payload = prepare_launch(&TestContext {
        mode: "inspect",
        path: None,
    })
    .unwrap();
    let error = launch_prepared(&payload).unwrap_err();

    assert!(matches!(error, LaunchError::Spawn(_)));
}

#[test]
#[cfg(unix)]
fn missing_ready_signal_times_out_and_reaps_child() {
    use std::os::unix::fs::PermissionsExt;

    let environment = TestEnvironment::new();
    let fake_cos = environment.directory.path().join("fake-agent-timeout");
    let pid_file = environment.directory.path().join("pid");
    std::fs::write(
        &fake_cos,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 30\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_cos, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var(AGENT_UI_ENV, &fake_cos);

    assert!(matches!(
        launch_prepared_with_timeout(
            &prepare_launch(&TestContext {
                mode: "inspect",
                path: None,
            })
            .unwrap(),
            Duration::from_secs(2),
        ),
        Err(LaunchError::Timeout(_))
    ));
    let pid = std::fs::read_to_string(pid_file).unwrap();
    assert!(!std::path::Path::new("/proc").join(pid).exists());
}

#[test]
#[cfg(unix)]
fn child_crash_before_ready_is_reaped() {
    use std::os::unix::fs::PermissionsExt;

    let environment = TestEnvironment::new();
    let fake_ui = environment.directory.path().join("fake-agent-crash");
    let pid_file = environment.directory.path().join("pid");
    std::fs::write(
        &fake_ui,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexit 7\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_ui, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var(AGENT_UI_ENV, &fake_ui);

    let payload = prepare_launch(&TestContext {
        mode: "inspect",
        path: None,
    })
    .unwrap();
    assert!(matches!(
        launch_prepared_with_timeout(&payload, Duration::from_secs(1)),
        Err(LaunchError::ChildExited(Some(7)))
    ));
    let pid = std::fs::read_to_string(pid_file).unwrap();
    assert!(!std::path::Path::new("/proc").join(pid).exists());
}

#[test]
#[cfg(unix)]
fn parent_waits_for_ready_before_writing_private_context() {
    use std::os::unix::fs::PermissionsExt;

    let environment = TestEnvironment::new();
    let fake_ui = environment.directory.path().join("fake agent;not-a-shell");
    let capture_base = environment.directory.path().join("capture");
    std::fs::write(
        &fake_ui,
        concat!(
            "#!/usr/bin/python3\n",
            "import ctypes, json, os, select, sys\n",
            "base = os.environ['ASK_CLAW_TEST_CAPTURE']\n",
            "libc = ctypes.CDLL(None)\n",
            "libc.prctl(4, 0, 0, 0, 0)\n",
            "open(base + '.dumpable', 'w').write(str(libc.prctl(3, 0, 0, 0, 0)))\n",
            "readable = bool(select.select([sys.stdin], [], [], 0)[0])\n",
            "open(base + '.before', 'w').write(str(readable))\n",
            "os.write(3, b'READY\\n')\n",
            "payload = sys.stdin.buffer.read()\n",
            "open(base + '.stdin', 'wb').write(payload)\n",
            "open(base + '.argv', 'w').write(json.dumps(sys.argv[1:]))\n",
            "open(base + '.env', 'w').write(json.dumps(dict(os.environ)))\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_ui, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var(AGENT_UI_ENV, &fake_ui);
    std::env::set_var("ASK_CLAW_TEST_CAPTURE", &capture_base);

    let secret = "nested-secret-value";
    let payload = prepare_launch(&TestContext {
        mode: secret,
        path: Some("/private/input"),
    })
    .unwrap();
    launch_prepared_with_timeout(&payload, Duration::from_secs(2)).unwrap();

    assert_eq!(
        std::fs::read_to_string(capture_base.with_extension("before")).unwrap(),
        "False"
    );
    assert_eq!(
        std::fs::read_to_string(capture_base.with_extension("dumpable")).unwrap(),
        "0"
    );
    let argv = std::fs::read_to_string(capture_base.with_extension("argv")).unwrap();
    let child_environment = std::fs::read_to_string(capture_base.with_extension("env")).unwrap();
    assert!(!argv.contains(secret));
    assert!(!argv.contains("/private/input"));
    assert!(!child_environment.contains(secret));
    assert!(!child_environment.contains("/private/input"));
    assert!(argv.contains("--context-stdin"));
    assert!(argv.contains("--ready-fd"));

    let received = std::fs::read(capture_base.with_extension("stdin")).unwrap();
    assert_eq!(received, payload);
    let activation = read_activation(Cursor::new(received)).unwrap();
    let context = activation.context.unwrap();
    assert!(context.contains(secret));
    assert!(context.contains("/private/input"));
    assert!(!environment.directory.path().join("not-a-shell").exists());
}

#[test]
#[cfg(unix)]
fn launcher_worker_does_not_block_the_calling_thread() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let environment = TestEnvironment::new();
    let fake_cos = environment.directory.path().join("slow-success-agent");
    std::fs::write(
        &fake_cos,
        concat!(
            "#!/bin/sh\n",
            "printf 'READY\\n' >&3\n",
            "cat >/dev/null\n",
            "sleep 1\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_cos, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var(AGENT_UI_ENV, &fake_cos);

    let payload = prepare_launch(&TestContext {
        mode: "inspect",
        path: None,
    })
    .unwrap();
    let started = Instant::now();
    let worker = spawn_prepared(payload).unwrap();
    assert!(started.elapsed() < Duration::from_millis(500));
    worker.join().unwrap();
}

#[test]
#[cfg(unix)]
fn partial_context_write_kills_and_reaps_child() {
    use std::os::unix::fs::PermissionsExt;

    let environment = TestEnvironment::new();
    let fake_ui = environment.directory.path().join("partial-reader");
    let pid_file = environment.directory.path().join("pid");
    std::fs::write(
        &fake_ui,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec 0<&-\nprintf 'READY\\n' >&3\nsleep 1\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_ui, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var(AGENT_UI_ENV, &fake_ui);

    let payload = vec![b'x'; MAX_ACTIVATION_BYTES];
    assert!(matches!(
        launch_prepared_with_timeout(&payload, Duration::from_secs(2)),
        Err(LaunchError::Write(_))
    ));
    let pid = std::fs::read_to_string(pid_file).unwrap();
    assert!(!std::path::Path::new("/proc").join(pid).exists());
}

#[test]
fn ptrace_policy_fails_closed_below_level_two() {
    assert!(matches!(
        validate_ptrace_scope("0"),
        Err(IsolationError::InsufficientPtraceScope)
    ));
    assert!(matches!(
        validate_ptrace_scope("1\n"),
        Err(IsolationError::InsufficientPtraceScope)
    ));
    validate_ptrace_scope("2").unwrap();
    validate_ptrace_scope("3").unwrap();
    assert!(matches!(
        validate_ptrace_scope("unknown"),
        Err(IsolationError::InvalidPtraceScope(_))
    ));
}

#[test]
#[cfg(target_os = "linux")]
fn process_can_be_marked_non_dumpable_before_handoff() {
    // SAFETY: PR_GET_DUMPABLE and PR_SET_DUMPABLE have no pointer arguments.
    let original = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
    assert!(original >= 0);
    set_current_process_non_dumpable().unwrap();
    let current = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
    assert_eq!(current, 0);
    if original != 0 {
        let restored = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, original, 0, 0, 0) };
        assert_eq!(restored, 0);
    }
}
