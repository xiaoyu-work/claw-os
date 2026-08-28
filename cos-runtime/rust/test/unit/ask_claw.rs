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
        for name in ["CLAW_COS_BIN", "ASK_CLAW_TEST_CAPTURE"] {
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
fn descriptor_launcher_argv_is_fixed_and_contains_no_payload() {
    assert_eq!(
        launch_argv(),
        [
            "/proc/self/fd/10",
            "--overlay",
            "--context-socket",
            "--activation-fd",
            "3",
        ]
    );
}

#[test]
fn frames_round_trip_and_reject_declared_oversize_payloads() {
    let mut encoded = Vec::new();
    write_frame(&mut encoded, b"private context").unwrap();
    assert_eq!(
        read_frame(&mut Cursor::new(encoded), MAX_ACTIVATION_BYTES).unwrap(),
        b"private context"
    );

    let oversized = ((MAX_ACTIVATION_BYTES + 1) as u32).to_be_bytes();
    assert!(matches!(
        read_frame(&mut Cursor::new(oversized), MAX_ACTIVATION_BYTES),
        Err(FrameError::TooLarge(actual)) if actual == MAX_ACTIVATION_BYTES + 1
    ));

    let mut partial = 8_u32.to_be_bytes().to_vec();
    partial.extend_from_slice(b"short");
    assert!(matches!(
        read_frame(&mut Cursor::new(partial), MAX_ACTIVATION_BYTES),
        Err(FrameError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
    ));
}

#[test]
fn query_activation_is_bounded_and_contains_no_argv_payload() {
    let payload = serialize_activation(&Activation {
        query: Some("private query".into()),
        ..Activation::default()
    })
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Activation>(&payload)
            .unwrap()
            .query
            .as_deref(),
        Some("private query")
    );
    assert!(!launch_argv()
        .iter()
        .any(|argument| argument.contains("private query")));
}

#[test]
#[cfg(target_os = "linux")]
fn retained_executable_descriptor_survives_path_substitution() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let environment = TestEnvironment::new();
    let executable = environment.directory.path().join("agent");
    std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let trusted = open_executable(&executable, false).unwrap();
    let original = trusted.file.metadata().unwrap();

    let moved = environment.directory.path().join("agent-old");
    std::fs::rename(&executable, moved).unwrap();
    std::fs::write(&executable, "#!/bin/sh\nexit 99\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let replacement = std::fs::metadata(&executable).unwrap();
    let retained = trusted.file.metadata().unwrap();

    assert_eq!(
        (retained.dev(), retained.ino()),
        (original.dev(), original.ino())
    );
    assert_ne!(
        (retained.dev(), retained.ino()),
        (replacement.dev(), replacement.ino())
    );
}

#[test]
#[cfg(target_os = "linux")]
fn sdk_listener_is_abstract_and_peer_credentials_are_exact() {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::SocketAddr;

    let (listener, endpoint) = bind_sdk_listener().unwrap();
    assert!(listener.local_addr().unwrap().as_pathname().is_none());
    let address = SocketAddr::from_abstract_name(endpoint.as_bytes()).unwrap();
    let client = UnixStream::connect_addr(&address).unwrap();
    let (server, _) = listener.accept().unwrap();
    let actual = sdk_peer_credentials(&server).unwrap();
    let pid = std::process::id() as libc::pid_t;
    let uid = unsafe { libc::geteuid() };
    assert!(sdk_peer_is_expected(actual, pid, uid));
    assert!(!sdk_peer_is_expected(actual, pid + 1, uid));
    assert!(!sdk_peer_is_expected(actual, pid, uid.wrapping_add(1)));
    drop(client);
}

#[test]
#[cfg(target_os = "linux")]
fn sdk_listener_rejects_an_attacker_then_accepts_the_direct_peer() {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::SocketAddr;
    use std::process::Stdio;

    let (listener, endpoint) = bind_sdk_listener().unwrap();
    let expected_peer = std::process::id() as libc::pid_t;
    let expected_uid = unsafe { libc::geteuid() };
    let parent = unsafe { libc::getppid() };
    let acceptor = std::thread::spawn(move || {
        accept_sdk_peer(
            &listener,
            expected_peer,
            expected_uid,
            parent,
            Instant::now() + Duration::from_secs(2),
        )
    });
    let script = concat!(
        "import socket,sys,time\n",
        "s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)\n",
        "s.connect('\\0'+sys.argv[1])\n",
        "time.sleep(.2)\n",
    );
    let mut attacker = std::process::Command::new("python3")
        .args(["-c", script, &endpoint])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let address = SocketAddr::from_abstract_name(endpoint.as_bytes()).unwrap();
    let direct_peer = UnixStream::connect_addr(&address).unwrap();
    let accepted = acceptor.join().unwrap().unwrap();
    assert_eq!(
        sdk_peer_credentials(&accepted).unwrap(),
        (expected_peer, expected_uid)
    );
    drop(direct_peer);
    attacker.wait().unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn sdk_listener_fails_closed_when_the_captured_parent_changes() {
    let (listener, _) = bind_sdk_listener().unwrap();
    let result = accept_sdk_peer(
        &listener,
        std::process::id() as libc::pid_t,
        unsafe { libc::geteuid() },
        -1,
        Instant::now() + Duration::from_secs(1),
    );
    assert!(matches!(result, Err(LaunchError::Ready(_))));
}

#[test]
#[cfg(target_os = "linux")]
fn executable_validation_rejects_missing_and_non_regular_targets() {
    let environment = TestEnvironment::new();
    let missing = environment.directory.path().join("missing-agent");
    assert!(matches!(
        open_executable(&missing, false),
        Err(LaunchError::ExecutableUnavailable(_))
    ));

    let directory = environment.directory.path().join("agent-directory");
    std::fs::create_dir(&directory).unwrap();
    assert!(matches!(
        open_executable(&directory, false),
        Err(LaunchError::UntrustedExecutable(_))
    ));

    use std::os::unix::fs::symlink;
    let target = environment.directory.path().join("target");
    std::fs::write(&target, "target").unwrap();
    let link = environment.directory.path().join("agent-link");
    symlink(&target, &link).unwrap();
    assert!(matches!(
        open_executable(&link, false),
        Err(LaunchError::UntrustedExecutable(_))
    ));
}

#[test]
fn ui_socket_activation_decodes_without_an_argv_payload() {
    let expected = Activation::overlay_with_context(
        serialize_context(&TestContext {
            mode: "inspect",
            path: Some("/private/input"),
        })
        .unwrap(),
    );
    let payload = serde_json::to_vec(&expected).unwrap();
    let parsed = parse_ui_arguments(["--overlay", "--context-socket", "--future"]);
    assert_eq!(parsed.unknown, ["--future"]);

    let activation = parsed.activation(Cursor::new(payload)).unwrap().unwrap();
    assert_eq!(activation, expected);
}

#[test]
#[cfg(target_os = "linux")]
fn ui_socket_signals_ready_before_reading_a_bounded_frame() {
    let expected = Activation::overlay_with_context(r#"{"app":"socket-test"}"#.into());
    let payload = serde_json::to_vec(&expected).unwrap();
    let parsed = parse_ui_arguments(["--overlay", "--context-socket"]);
    let (mut parent, child) = UnixStream::pair().unwrap();
    let reader = std::thread::spawn(move || parsed.activation_from_socket(child));

    let mut ready = [0_u8; READY_MESSAGE.len()];
    parent.read_exact(&mut ready).unwrap();
    assert_eq!(&ready, READY_MESSAGE);
    write_frame(&mut parent, &payload).unwrap();
    assert_eq!(reader.join().unwrap().unwrap(), Some(expected));
}

#[test]
fn ui_socket_rejects_conflicting_context_without_inline_fallback() {
    let parsed = parse_ui_arguments([
        "--overlay",
        "--context-socket",
        "--context",
        r#"{"app":"legacy"}"#,
    ]);
    let socket_payload = serde_json::to_vec(&Activation::overlay_with_context(
        r#"{"app":"socket"}"#.to_string(),
    ))
    .unwrap();

    assert!(matches!(
        parsed.activation(Cursor::new(socket_payload)),
        Err(ActivationInputError::ConflictingContext)
    ));
}

#[test]
fn ui_socket_rejects_oversize_malformed_and_invalid_context() {
    let parsed = parse_ui_arguments(["--overlay", "--context-socket"]);
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
fn payload_bearing_argv_is_rejected_without_reading_the_private_channel() {
    struct PanicReader;

    impl Read for PanicReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("legacy activation must not read the private channel");
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
#[cfg(target_os = "linux")]
fn process_spawn_failures_are_preserved() {
    let environment = TestEnvironment::new();
    let missing = environment.directory.path().join("missing-agent");
    let payload = prepare_launch(&TestContext {
        mode: "inspect",
        path: None,
    })
    .unwrap();
    let error = launch_prepared_for_test(&payload, Duration::from_secs(1), &missing).unwrap_err();

    assert!(matches!(error, LaunchError::ExecutableUnavailable(_)));
}

#[test]
#[cfg(target_os = "linux")]
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
    assert!(matches!(
        launch_prepared_for_test(
            &prepare_launch(&TestContext {
                mode: "inspect",
                path: None,
            })
            .unwrap(),
            Duration::from_secs(2),
            &fake_cos,
        ),
        Err(LaunchError::Timeout(_))
    ));
    let pid = std::fs::read_to_string(pid_file).unwrap();
    assert!(!std::path::Path::new("/proc").join(pid).exists());
}

#[test]
#[cfg(target_os = "linux")]
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
    let payload = prepare_launch(&TestContext {
        mode: "inspect",
        path: None,
    })
    .unwrap();
    assert!(matches!(
        launch_prepared_for_test(&payload, Duration::from_secs(1), &fake_ui),
        Err(LaunchError::ChildExited(Some(7)))
    ));
    let pid = std::fs::read_to_string(pid_file).unwrap();
    assert!(!std::path::Path::new("/proc").join(pid).exists());
}

#[test]
#[cfg(target_os = "linux")]
fn parent_waits_for_ready_before_writing_private_context() {
    use std::os::unix::fs::PermissionsExt;

    let environment = TestEnvironment::new();
    let fake_ui = environment.directory.path().join("fake agent;not-a-shell");
    let capture_base = environment.directory.path().join("capture");
    std::fs::write(
        &fake_ui,
        concat!(
            "#!/usr/bin/python3\n",
            "import ctypes, json, os, select, socket, struct, sys\n",
            "base = os.environ['ASK_CLAW_TEST_CAPTURE']\n",
            "libc = ctypes.CDLL(None)\n",
            "libc.prctl(4, 0, 0, 0, 0)\n",
            "open(base + '.dumpable', 'w').write(str(libc.prctl(3, 0, 0, 0, 0)))\n",
            "open(base + '.stdin', 'wb').write(os.read(0, 1))\n",
            "channel = socket.socket(fileno=3)\n",
            "readable = bool(select.select([channel], [], [], 0)[0])\n",
            "open(base + '.before', 'w').write(str(readable))\n",
            "channel.sendall(b'READY\\n')\n",
            "length = struct.unpack('>I', channel.recv(4))[0]\n",
            "payload = b''\n",
            "while len(payload) < length:\n",
            "    payload += channel.recv(length - len(payload))\n",
            "open(base + '.socket', 'wb').write(payload)\n",
            "open(base + '.argv', 'w').write(json.dumps(sys.argv[1:]))\n",
            "open(base + '.env', 'w').write(json.dumps(dict(os.environ)))\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_ui, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("ASK_CLAW_TEST_CAPTURE", &capture_base);

    let secret = "nested-secret-value";
    let payload = prepare_launch(&TestContext {
        mode: secret,
        path: Some("/private/input"),
    })
    .unwrap();
    launch_prepared_for_test(&payload, Duration::from_secs(2), &fake_ui).unwrap();

    assert_eq!(
        std::fs::read_to_string(capture_base.with_extension("before")).unwrap(),
        "False"
    );
    assert_eq!(
        std::fs::read_to_string(capture_base.with_extension("dumpable")).unwrap(),
        "0"
    );
    assert!(std::fs::read(capture_base.with_extension("stdin"))
        .unwrap()
        .is_empty());
    let argv = std::fs::read_to_string(capture_base.with_extension("argv")).unwrap();
    let child_environment = std::fs::read_to_string(capture_base.with_extension("env")).unwrap();
    assert!(!argv.contains(secret));
    assert!(!argv.contains("/private/input"));
    assert!(!child_environment.contains(secret));
    assert!(!child_environment.contains("/private/input"));
    assert!(argv.contains("--context-socket"));
    assert!(argv.contains("--activation-fd"));

    let received = std::fs::read(capture_base.with_extension("socket")).unwrap();
    assert_eq!(received, payload);
    let activation = read_activation(Cursor::new(received)).unwrap();
    let context = activation.context.unwrap();
    assert!(context.contains(secret));
    assert!(context.contains("/private/input"));
    assert!(!environment.directory.path().join("not-a-shell").exists());
}

#[test]
#[cfg(target_os = "linux")]
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
            "cat <&3 >/dev/null\n",
            "sleep 1\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_cos, std::fs::Permissions::from_mode(0o700)).unwrap();
    let payload = prepare_launch(&TestContext {
        mode: "inspect",
        path: None,
    })
    .unwrap();
    let started = Instant::now();
    let worker = spawn_prepared_for_test(payload, fake_cos).unwrap();
    assert!(started.elapsed() < Duration::from_millis(500));
    worker.join().unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn partial_context_write_kills_and_reaps_child() {
    use std::os::unix::fs::PermissionsExt;

    let environment = TestEnvironment::new();
    let fake_ui = environment.directory.path().join("partial-reader");
    let pid_file = environment.directory.path().join("pid");
    std::fs::write(
        &fake_ui,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nprintf 'READY\\n' >&3\nexec 3>&-\nsleep 1\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_ui, std::fs::Permissions::from_mode(0o700)).unwrap();
    let payload = vec![b'x'; MAX_ACTIVATION_BYTES];
    assert!(matches!(
        launch_prepared_for_test(&payload, Duration::from_secs(2), &fake_ui),
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
