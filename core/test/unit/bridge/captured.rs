use super::*;

#[test]
fn gated_command_preserves_program_and_literal_arguments() {
    let _environment = crate::test_env::lock_env();
    let executable = std::env::current_exe().unwrap();
    let _runner = crate::test_env::TestEnvVarGuard::set("CLAW_APP_RUNNER_BIN", &executable);
    let arguments = vec![
        "".into(),
        "two words".into(),
        "quoted\"value".into(),
        "$HOME;".into(),
    ];
    let program = executable.clone();
    let first = GatedCommand::new(program.clone(), arguments.clone()).unwrap();
    let second = GatedCommand::new(program.clone(), arguments.clone()).unwrap();
    assert_eq!(first.runner, executable.canonicalize().unwrap());
    assert_eq!(first.program, program);
    assert_eq!(first.argv[0], "--launch-gate");
    assert_eq!(first.argv[1].as_bytes(), first.token);
    assert_eq!(first.argv[2], "--");
    assert_eq!(first.argv[3], program.to_str().unwrap());
    assert_eq!(first.argv[4..], arguments);
    assert_ne!(first.token, second.token);
    assert!(first.token.iter().all(u8::is_ascii_hexdigit));
}

#[test]
fn missing_runner_is_an_error_not_an_ungated_fallback() {
    let _environment = crate::test_env::lock_env();
    let directory = tempfile::tempdir().unwrap();
    let _runner = crate::test_env::TestEnvVarGuard::set(
        "CLAW_APP_RUNNER_BIN",
        directory.path().join("absent"),
    );
    let error = GatedCommand::new(std::env::current_exe().unwrap(), Vec::new())
        .err()
        .expect("missing gate runner");
    assert!(error.contains("launch-gate runner"), "{error}");
}

#[cfg(target_os = "linux")]
mod process {
    use super::*;
    use crate::caps::CapSet;
    use crate::worker::{Limits, StdioPlan, WorkerLaunch};
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn prepare(
        root: &Path,
        program: PathBuf,
        arguments: Vec<String>,
        input_bytes: u64,
    ) -> (crate::worker::PreparedLaunch, [u8; 32], Limits) {
        let package = root.join("package");
        std::fs::create_dir_all(&package).unwrap();
        let command = GatedCommand::new(program, arguments).unwrap();
        let mut policy = crate::worker::derive::wrapped_app_operation(
            crate::worker::derive::AppOperationInput {
                app_id: "gate-probe",
                app_dir: &package,
                operation: "run",
                program: command.runner,
                argv: command.argv,
                caps: &CapSet::from_caps([]),
                session_id: "gate-probe-session",
                data_dir: root.to_str().unwrap(),
                apps_dir: root.to_str().unwrap(),
                extra_env: BTreeMap::new(),
                stdio: StdioPlan::Captured,
                desktop: false,
                package_identity: None,
                pinned_entries: vec![],
                developer: false,
            },
            &command.program,
        )
        .unwrap();
        policy.limits.input_bytes = input_bytes;
        let limits = policy.limits;
        let prepared = crate::worker::prepare(&WorkerLaunch::new(policy).with_authority(
            crate::worker::BrokerAuthority::new(
                "gate-probe-session",
                Some("gate-probe".into()),
                CapSet::from_caps([]),
                crate::worker::relay_slot(),
            ),
        ))
        .unwrap();
        (prepared, command.token, limits)
    }

    #[test]
    #[ignore = "requires Linux worker sandbox and COS_CAPTURED_TEST_RUNNER pointing at the real runner"]
    fn real_runner_waits_for_binding_and_preserves_app_input() {
        let _environment = crate::test_env::lock_env();
        assert!(crate::worker::availability().is_available());
        let runner = std::env::var_os("COS_CAPTURED_TEST_RUNNER").expect("real runner input");
        let _runner = crate::test_env::TestEnvVarGuard::set("CLAW_APP_RUNNER_BIN", &runner);
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let marker = root.path().join("apps/gate-probe/started");
        let script = "import pathlib,sys\npathlib.Path(sys.argv[1]).write_bytes(b'started')\nsys.stdout.buffer.write(sys.stdin.buffer.read())\n";
        let arguments = vec!["-c".into(), script.into(), marker.to_str().unwrap().into()];
        let (prepared, token, limits) = prepare(
            root.path(),
            "/usr/bin/python3".into(),
            arguments.clone(),
            4096,
        );
        let refused =
            crate::worker::exec::run_captured_gated(prepared, None, limits, &token, |_| {
                std::thread::sleep(Duration::from_millis(150));
                assert!(!marker.exists(), "App code ran before its binding decision");
                Err("binding refused".into())
            });
        assert_eq!(refused.err().unwrap(), "binding refused");
        assert!(!marker.exists(), "refused App code was executed");

        let payload = (0..4096)
            .map(|index| (index % 256) as u8)
            .collect::<Vec<_>>();
        let (prepared, token, limits) = prepare(
            root.path(),
            "/usr/bin/python3".into(),
            arguments.clone(),
            4096,
        );
        let output = crate::worker::exec::run_captured_gated(
            prepared,
            Some(payload.clone()),
            limits,
            &token,
            |_| {
                std::thread::sleep(Duration::from_millis(150));
                assert!(!marker.exists(), "App escaped a delayed binding decision");
                Ok(())
            },
        )
        .unwrap();
        assert!(output.status.success(), "{}", output.stderr_string());
        assert_eq!(std::fs::read(&marker).unwrap(), b"started");
        assert_eq!(output.stdout, payload, "gate bytes must not reach the App");
        std::fs::remove_file(&marker).unwrap();

        let (prepared, token, limits) = prepare(
            root.path(),
            "/usr/bin/python3".into(),
            arguments.clone(),
            4096,
        );
        let bound = AtomicBool::new(false);
        let error = crate::worker::exec::run_captured_gated(
            prepared,
            Some(vec![0; 4097]),
            limits,
            &token,
            |_| {
                bound.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .err()
        .unwrap();
        assert!(error.contains("input ceiling"), "{error}");
        assert!(!bound.load(Ordering::SeqCst));
        assert!(!marker.exists());

        for input in [None, Some(Vec::new())] {
            let (prepared, token, limits) =
                prepare(root.path(), "/usr/bin/python3".into(), arguments.clone(), 0);
            let output = crate::worker::exec::run_captured_gated(
                prepared,
                input,
                limits,
                &token,
                |_| Ok(()),
            )
            .unwrap();
            assert!(output.status.success(), "{}", output.stderr_string());
            assert!(output.stdout.is_empty());
            assert!(
                marker.exists(),
                "the gate itself is not charged to App input"
            );
            std::fs::remove_file(&marker).unwrap();
        }

        let runtime_directory = root.path().join("custom-runtime");
        std::fs::create_dir(&runtime_directory).unwrap();
        let external_runtime = runtime_directory.join("cat");
        std::fs::copy("/usr/bin/cat", &external_runtime).unwrap();
        std::fs::set_permissions(&external_runtime, std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let (prepared, token, limits) = prepare(root.path(), external_runtime, vec![], 4096);
        let output = crate::worker::exec::run_captured_gated(
            prepared,
            Some(payload.clone()),
            limits,
            &token,
            |_| Ok(()),
        )
        .unwrap();
        assert!(output.status.success(), "{}", output.stderr_string());
        assert_eq!(
            output.stdout, payload,
            "wrapped non-system runtimes need their own read-only mount"
        );
    }
}
