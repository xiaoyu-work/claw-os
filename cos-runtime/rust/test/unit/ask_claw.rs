use super::*;
use crate::BridgeError;
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
        ["/opt/claw/custom-agent-ui", "--overlay", "--context-stdin"]
    );
}

#[test]
fn launcher_uses_path_lookup_by_default() {
    let _environment = TestEnvironment::new();
    assert_eq!(agent_ui_executable(), DEFAULT_AGENT_UI);
}

#[test]
fn ui_stdin_activation_survives_single_instance_round_trip() {
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
    let dbus_payload = activation.to_string();
    assert_eq!(Activation::from_str(&dbus_payload).unwrap(), expected);
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
fn legacy_inline_context_is_still_parsed_without_reading_stdin() {
    struct PanicReader;

    impl Read for PanicReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("legacy activation must not read stdin");
        }
    }

    let parsed = parse_ui_arguments(["--overlay", "--context", r#"{"app":"legacy"}"#]);
    let activation = parsed.activation(PanicReader).unwrap().unwrap();
    assert_eq!(activation.context.as_deref(), Some(r#"{"app":"legacy"}"#));
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
        "CLAW_COS_BIN",
        "/nonexistent/definitely-not-a-real-cos-binary-issue-47",
    );
    let error = launch(&TestContext {
        mode: "inspect",
        path: None,
    })
    .unwrap_err();

    assert!(matches!(
        error,
        LaunchError::Process(exec::StartError::Bridge(BridgeError::BinaryNotFound(_)))
    ));
}

#[test]
#[cfg(unix)]
fn reported_launch_timeout_is_preserved() {
    use std::os::unix::fs::PermissionsExt;

    let environment = TestEnvironment::new();
    let fake_cos = environment.directory.path().join("fake-cos-timeout");
    std::fs::write(
        &fake_cos,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \
         '{\"error\":\"command timed out\",\"code\":\"timeout\"}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_cos, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("CLAW_COS_BIN", &fake_cos);

    assert!(matches!(
        launch(&TestContext {
            mode: "inspect",
            path: None,
        }),
        Err(LaunchError::Process(exec::StartError::Bridge(
            BridgeError::AppError { code, .. }
        ))) if code.as_deref() == Some("timeout")
    ));
}

#[test]
#[cfg(unix)]
fn successful_exec_response_captures_stdin_without_payload_in_request() {
    use std::os::unix::fs::PermissionsExt;

    let environment = TestEnvironment::new();
    let fake_cos = environment.directory.path().join("fake-cos");
    let capture_base = environment.directory.path().join("capture");
    std::fs::write(
        &fake_cos,
        concat!(
            "#!/bin/sh\n",
            "cat > \"${ASK_CLAW_TEST_CAPTURE}.stdin\"\n",
            "printf '%s\\n' \"$@\" > \"${ASK_CLAW_TEST_CAPTURE}.argv\"\n",
            "printf '%s\\n' '{\"pid\":4242,\"command\":[\"cos-agent-ui\",",
            "\"--overlay\",\"--context-stdin\"]}'\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_cos, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("CLAW_COS_BIN", &fake_cos);
    std::env::set_var(AGENT_UI_ENV, "/opt/claw/cos-agent-ui");
    std::env::set_var("ASK_CLAW_TEST_CAPTURE", &capture_base);

    let secret = "nested-secret-value";
    let handle = launch(&TestContext {
        mode: secret,
        path: Some("/private/input"),
    })
    .unwrap();
    assert_eq!(handle.pid, 4242);
    assert_eq!(
        handle.command,
        ["cos-agent-ui", "--overlay", "--context-stdin"]
    );

    let request = std::fs::read_to_string(capture_base.with_extension("argv")).unwrap();
    assert!(!request.contains(secret));
    assert!(!request.contains("/private/input"));
    assert!(request.contains("--context-stdin"));
    assert!(!request.contains("--context\n"));

    let payload = std::fs::read(capture_base.with_extension("stdin")).unwrap();
    let activation = read_activation(Cursor::new(payload)).unwrap();
    let context = activation.context.unwrap();
    assert!(context.contains(secret));
    assert!(context.contains("/private/input"));
}
