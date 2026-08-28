use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());
static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(0);

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
    runtime: tempfile::TempDir,
}

impl TestEnvironment {
    fn new() -> Self {
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let runtime = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
            if fs::metadata(runtime.path()).unwrap().mode() & 0o077 != 0 {
                std::env::set_var("COS_ASK_CLAW_TEST_PERMISSIVE_FS", "1");
            }
        }
        std::env::set_var("XDG_RUNTIME_DIR", runtime.path());
        Self {
            _guard: guard,
            runtime,
        }
    }
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        for name in [
            "XDG_RUNTIME_DIR",
            "CLAW_COS_BIN",
            AGENT_UI_ENV,
            "ASK_CLAW_TEST_CAPTURE",
            "COS_ASK_CLAW_TEST_PERMISSIVE_FS",
        ] {
            std::env::remove_var(name);
        }
    }
}

fn write_context_file(contents: &[u8], mode: u32) -> PathBuf {
    let directory = context_directory(true).unwrap();
    let sequence = NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed);
    let path = directory.join(format!("{CONTEXT_PREFIX}test-{sequence}{CONTEXT_SUFFIX}"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let mut file = options.open(&path).unwrap();
    file.write_all(contents).unwrap();
    file.sync_all().unwrap();
    path
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
fn launcher_selects_executable_and_uses_only_context_file_in_arguments() {
    let _environment = TestEnvironment::new();
    std::env::set_var(AGENT_UI_ENV, "/opt/claw/custom-agent-ui");
    let activation = Activation::overlay_with_context_file(PathBuf::from(
        "/run/user/1000/claw-os-ask-claw/.context-test.json",
    ));
    let argv = launch_argv(&activation);

    assert_eq!(
        argv,
        [
            "/opt/claw/custom-agent-ui",
            "--overlay",
            "--context-file",
            "/run/user/1000/claw-os-ask-claw/.context-test.json",
        ]
    );
}

#[test]
fn launcher_uses_path_lookup_by_default() {
    let _environment = TestEnvironment::new();
    assert_eq!(agent_ui_executable(), DEFAULT_AGENT_UI);
}

#[test]
fn ui_argument_parser_and_activation_json_round_trip() {
    let parsed = parse_ui_arguments([
        "--overlay",
        "--voice",
        "--query",
        "explain this",
        "--context-file",
        "/run/user/1000/claw-os-ask-claw/.context-test.json",
        "--future",
    ]);
    assert_eq!(parsed.unknown, ["--future"]);

    let activation = parsed.activation().unwrap();
    let encoded = activation.to_string();
    assert_eq!(Activation::from_str(&encoded).unwrap(), activation);
    assert!(encoded.contains("context_file"));
    assert!(!encoded.contains("test-app"));
}

#[test]
fn legacy_inline_context_is_still_parsed() {
    let parsed = parse_ui_arguments(["--overlay", "--context", r#"{"app":"legacy"}"#]);
    let activation = parsed.activation().unwrap();
    assert_eq!(activation.context.as_deref(), Some(r#"{"app":"legacy"}"#));
    assert!(activation.context_file.is_none());
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
#[cfg(unix)]
fn private_context_file_is_mode_0600_and_read_once() {
    use std::os::unix::fs::PermissionsExt;

    let _environment = TestEnvironment::new();
    let context = serialize_context(&TestContext {
        mode: "inspect",
        path: Some("/private"),
    })
    .unwrap();
    let staged = stage_context_file(&context).unwrap();
    let path = staged.path.clone();
    staged.persist();

    use std::os::unix::fs::MetadataExt;
    assert_eq!(fs::metadata(&path).unwrap().uid(), current_uid());
    if !skip_test_mode_validation() {
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let mut activation = Activation::overlay_with_context_file(path.clone());
    activation.resolve_context_file().unwrap();
    assert_eq!(activation.context.as_deref(), Some(context.as_str()));
    assert!(!path.exists());

    let mut repeated = Activation::overlay_with_context_file(path);
    assert!(matches!(
        repeated.resolve_context_file(),
        Err(ContextFileError::Io(error)) if error.kind() == io::ErrorKind::NotFound
    ));
}

#[test]
#[cfg(unix)]
fn context_reader_rejects_symlink_without_reading_target() {
    use std::os::unix::fs::symlink;

    let environment = TestEnvironment::new();
    let target = environment.runtime.path().join("target.json");
    fs::write(&target, br#"{"app":"stolen","secret":"do-not-read"}"#).unwrap();
    let directory = context_directory(true).unwrap();
    let link = directory.join(format!("{CONTEXT_PREFIX}symlink{CONTEXT_SUFFIX}"));
    symlink(&target, &link).unwrap();

    let mut activation = Activation::overlay_with_context_file(link.clone());
    assert!(activation.resolve_context_file().is_err());
    assert_eq!(
        fs::read_to_string(target).unwrap(),
        r#"{"app":"stolen","secret":"do-not-read"}"#
    );
    assert!(!link.exists());
}

#[test]
#[cfg(unix)]
fn context_writer_rejects_symlinked_runtime_directory() {
    use std::os::unix::fs::symlink;

    let environment = TestEnvironment::new();
    let target = environment.runtime.path().join("redirected");
    fs::create_dir(&target).unwrap();
    let directory = environment.runtime.path().join(CONTEXT_DIRECTORY);
    symlink(&target, &directory).unwrap();

    assert!(matches!(
        stage_context_file(r#"{"app":"test-app"}"#),
        Err(ContextFileError::InsecureDirectory(path)) if path == directory
    ));
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
}

#[test]
fn context_reader_rejects_paths_outside_private_directory() {
    let environment = TestEnvironment::new();
    context_directory(true).unwrap();
    let outside = environment.runtime.path().join(".context-outside.json");
    fs::write(&outside, br#"{"app":"test-app"}"#).unwrap();
    let mut activation = Activation::overlay_with_context_file(outside.clone());

    assert!(matches!(
        activation.resolve_context_file(),
        Err(ContextFileError::InvalidPath(path)) if path == outside
    ));
    assert!(outside.exists());
}

#[test]
#[cfg(unix)]
fn context_reader_rejects_insecure_mode_and_deletes_file() {
    let _environment = TestEnvironment::new();
    if skip_test_mode_validation() {
        return;
    }
    let path = write_context_file(br#"{"app":"test-app"}"#, 0o644);
    let mut activation = Activation::overlay_with_context_file(path.clone());

    assert!(matches!(
        activation.resolve_context_file(),
        Err(ContextFileError::InsecureFile(_))
    ));
    assert!(!path.exists());
}

#[test]
#[cfg(unix)]
fn context_reader_rejects_oversize_and_malformed_files() {
    let _environment = TestEnvironment::new();
    let oversized = write_context_file(&vec![b'x'; MAX_CONTEXT_BYTES + 1], 0o600);
    let mut activation = Activation::overlay_with_context_file(oversized.clone());
    assert!(matches!(
        activation.resolve_context_file(),
        Err(ContextFileError::TooLarge { .. })
    ));
    assert!(!oversized.exists());

    let malformed = write_context_file(b"{not-json", 0o600);
    let mut activation = Activation::overlay_with_context_file(malformed.clone());
    assert!(matches!(
        activation.resolve_context_file(),
        Err(ContextFileError::InvalidJson(_))
    ));
    assert!(!malformed.exists());
}

#[test]
fn stale_context_files_are_removed_without_touching_other_files() {
    let _environment = TestEnvironment::new();
    let stale = write_context_file(br#"{"app":"test-app"}"#, 0o600);
    let directory = stale.parent().unwrap();
    let unrelated = directory.join("keep.txt");
    fs::write(&unrelated, "keep").unwrap();

    cleanup_stale_context_files(directory, SystemTime::now(), Duration::ZERO).unwrap();
    assert!(!stale.exists());
    assert!(unrelated.exists());
}

#[test]
fn process_spawn_failure_removes_staged_context() {
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
        LaunchError::Process(BridgeError::BinaryNotFound(_))
    ));
    let directory = context_directory(false).unwrap();
    assert_eq!(fs::read_dir(directory).unwrap().count(), 0);
}

#[test]
#[cfg(unix)]
fn reported_launch_timeout_removes_staged_context() {
    use std::os::unix::fs::PermissionsExt;

    let environment = TestEnvironment::new();
    let fake_cos = environment.runtime.path().join("fake-cos-timeout");
    fs::write(
        &fake_cos,
        "#!/bin/sh\nprintf '%s\\n' '{\"error\":\"command timed out\",\"code\":\"timeout\"}'\n",
    )
    .unwrap();
    fs::set_permissions(&fake_cos, fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("CLAW_COS_BIN", &fake_cos);

    assert!(matches!(
        launch(&TestContext {
            mode: "inspect",
            path: None,
        }),
        Err(LaunchError::Process(BridgeError::AppError { code, .. }))
            if code.as_deref() == Some("timeout")
    ));
    let directory = context_directory(false).unwrap();
    assert_eq!(fs::read_dir(directory).unwrap().count(), 0);
}

#[test]
#[cfg(unix)]
fn successful_exec_response_returns_handle_without_payload_in_request() {
    use std::os::unix::fs::PermissionsExt;

    let environment = TestEnvironment::new();
    let fake_cos = environment.runtime.path().join("fake-cos");
    let capture = environment.runtime.path().join("argv.txt");
    fs::write(
        &fake_cos,
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' \"$@\" > \"$ASK_CLAW_TEST_CAPTURE\"\n",
            "printf '%s\\n' '{\"pid\":4242,\"command\":[\"cos-agent-ui\",\"--overlay\",",
            "\"--context-file\",\"redacted\"]}'\n",
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_cos, fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("CLAW_COS_BIN", &fake_cos);
    std::env::set_var(AGENT_UI_ENV, "/opt/claw/cos-agent-ui");
    std::env::set_var("ASK_CLAW_TEST_CAPTURE", &capture);

    let secret = "nested-secret-value";
    let handle = launch(&TestContext {
        mode: secret,
        path: Some("/private/input"),
    })
    .unwrap();
    assert_eq!(handle.pid, 4242);
    assert_eq!(handle.command[0], "cos-agent-ui");

    let request = fs::read_to_string(capture).unwrap();
    assert!(!request.contains(secret));
    assert!(!request.contains("/private/input"));
    assert!(request.contains("--context-file"));

    let arguments = request.lines().collect::<Vec<_>>();
    let context_file_index = arguments
        .iter()
        .position(|argument| *argument == "--context-file")
        .unwrap();
    let path = PathBuf::from(arguments[context_file_index + 1]);
    let mut activation = Activation::overlay_with_context_file(path.clone());
    activation.resolve_context_file().unwrap();
    let context = activation.context.unwrap();
    assert!(context.contains(secret));
    assert!(!path.exists());
}
