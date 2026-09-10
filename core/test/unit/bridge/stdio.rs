use super::*;
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::test_env::{TestEnvVarGuard, TestSessionGuard};
use std::fs;
use std::os::unix::fs::PermissionsExt;

const APP: &str = "independent-stdio";
const OPERATION: &str = "native-host";
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn stdio_cancellation_interrupts_review_and_eof_wait_without_releasing_a_gate() {
    let _lock = crate::test_env::lock_env();
    let _signals = StdioSignals::install().unwrap();
    assert!(StdioSignals::install().is_err());
    assert!(!cancelled());
    cancel_from_signal(libc::SIGINT);
    assert!(
        super::super::check_approval_wait(Instant::now() + Duration::from_secs(30))
            .unwrap_err()
            .contains("cancelled")
    );
    let mut gated = std::process::Command::new("/usr/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = fs::File::open("/dev/null").unwrap();
    assert!(relay_stdin(
        &mut gated,
        input.as_fd(),
        TOKEN,
        Duration::from_secs(1),
        || {}
    )
    .unwrap_err()
    .contains("cancelled"));
    assert!(
        gated.stdin.is_some(),
        "cancelled host took/released the gate"
    );
    gated.kill().unwrap();
    assert!(gated.wait_with_output().unwrap().stdout.is_empty());
    let mut child = std::process::Command::new("/usr/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let result = wait_after_eof(&mut child, Duration::from_secs(300), || {});
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(result.unwrap_err().contains("cancelled"));
    drop(_signals);
    assert!(!cancelled());
    StdioSignals::install().unwrap();
}

struct Fixture {
    root: PathBuf,
    apps: PathBuf,
    data: PathBuf,
    app: PathBuf,
    _data_env: TestEnvVarGuard,
}

impl Fixture {
    fn new(runtime: Runtime, entry: &str, source: &[u8], stdin: bool) -> Self {
        let root = crate::test_env::secure_scratch_dir("app-stdio");
        let apps = root.join("apps");
        let app = apps.join(APP);
        let data = root.join("data");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir(&data).unwrap();
        fs::write(
            app.join("app.json"),
            serde_json::json!({
                "id": APP, "version": "1.0.0", "name": {"en": "Independent stdio"},
                "runtime": runtime, "entry": entry,
                "operations": {
                    OPERATION: {"label": {"en": "Host"}, "stdin": stdin, "needs": []}
                }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(app.join(entry), source).unwrap();
        if matches!(runtime, Runtime::Binary) {
            fs::set_permissions(app.join(entry), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let data_env = TestEnvVarGuard::set("COS_DATA_DIR", &data);
        Self {
            root,
            apps,
            data,
            app,
            _data_env: data_env,
        }
    }

    fn launch(&self) -> AppLaunch {
        crate::test_env::app_launch(&self.app, APP)
    }

    fn input(&self, bytes: &[u8]) -> fs::File {
        let path = self.root.join("input");
        fs::write(&path, bytes).unwrap();
        fs::File::open(path).unwrap()
    }

    fn prepare(
        &self,
        launch: &AppLaunch,
        session: &AppIdentitySession,
        binding: &LaunchBindingRef,
    ) -> crate::worker::PreparedLaunch {
        let entry = stdio_entry(launch, OPERATION).unwrap();
        let (program, mut argv) =
            crate::bridge::session_program(launch.manifest().runtime, &entry).unwrap();
        if matches!(launch.manifest().runtime, Runtime::Python) {
            argv.insert(0, "-I".to_string());
        }
        let mut prepared = prepare_stdio_worker(
            launch,
            session,
            OPERATION,
            &[],
            program,
            argv,
            crate::bridge::app_runner_path(),
            TOKEN,
            self.data.to_str().unwrap(),
            self.apps.to_str().unwrap(),
            binding,
        )
        .unwrap();
        assert_eq!(prepared.facts["tier"], "app-operation");
        assert_eq!(prepared.facts["network"]["mode"], "denied");
        assert_eq!(prepared.facts["stdio"], "streamed");
        assert_eq!(prepared.facts["limits"]["runtime_secs"], 0);
        assert_eq!(prepared.facts["broker"], true);
        prepared
            .command
            .stdout(fs::File::create(self.root.join("output")).unwrap())
            .stderr(fs::File::create(self.root.join("stderr")).unwrap());
        prepared
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn stdio_requires_its_own_declared_stdin_operation() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new(Runtime::Python, "host.py", b"pass\n", false);
    let launch = fixture.launch();
    assert!(stdio_entry(&launch, OPERATION)
        .unwrap_err()
        .contains("does not declare stdin"));
    assert!(stdio_entry(&launch, "undeclared")
        .unwrap_err()
        .contains("no operation"));
}

#[test]
fn signed_resources_and_external_paths_cannot_replace_the_entry() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new(Runtime::Python, "host.py", b"pass\n", true);
    crate::test_env::sign_test_package_with_entrypoints(
        &fixture.app,
        crate::provenance::PackageKind::App,
        APP,
        &[],
    );
    let launch = crate::test_env::try_app_launch(&fixture.app, APP).unwrap();
    assert!(stdio_entry(&launch, OPERATION)
        .unwrap_err()
        .contains("not a declared, signed entrypoint"));

    let manifest_path = fixture.app.join("app.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["entry"] = serde_json::json!("/usr/bin/python3");
    fs::write(&manifest_path, manifest.to_string()).unwrap();
    let launch = fixture.launch();
    assert!(stdio_entry(&launch, OPERATION)
        .unwrap_err()
        .contains("package-relative"));
}

#[test]
fn unsigned_or_forged_packages_never_become_app_launches() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new(Runtime::Python, "host.py", b"pass\n", true);
    crate::test_env::install_test_trust();
    assert!(crate::test_env::try_app_launch(&fixture.app, APP).is_err());
    let _launch = fixture.launch();
    fs::write(fixture.app.join("host.py"), b"print('forged')\n").unwrap();
    assert!(crate::test_env::try_app_launch(&fixture.app, APP).is_err());
}

#[test]
fn runtime_integrity_is_separate_from_app_signature_admission() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new(Runtime::Binary, "program", b"#!/bin/sh\nexit 0\n", true);
    let launch = fixture.launch();
    let entry = stdio_entry(&launch, OPERATION).unwrap();
    assert!(
        program_is_trusted(&entry).is_err(),
        "App code became a trusted runtime"
    );
    program_is_trusted(&Path::new("/usr/bin/python3").canonicalize().unwrap()).unwrap();
}

#[test]
fn inherited_runtime_overrides_cannot_expose_caller_code_or_private_directories() {
    let _lock = crate::test_env::lock_env();
    let _cos = TestEnvVarGuard::remove("COS_BIN");
    let _sdk = TestEnvVarGuard::remove("COS_SDK_PYTHON_DIR");
    validate_runtime_overrides().unwrap();
    for (name, bad) in [
        ("COS_BIN", "/home/example/fake-cos"),
        ("COS_BIN", "/usr/bin/bash"),
        ("COS_SDK_PYTHON_DIR", "/home/example/.ssh"),
        (
            "COS_SDK_PYTHON_DIR",
            "/usr/lib/cos/python:/home/example/private",
        ),
    ] {
        let _override = TestEnvVarGuard::set(name, bad);
        assert!(validate_runtime_overrides().unwrap_err().contains(name));
    }
    let _cos = TestEnvVarGuard::set("COS_BIN", "/usr/local/bin/cos");
    let _sdk = TestEnvVarGuard::set("COS_SDK_PYTHON_DIR", "/usr/lib/cos/python");
    validate_runtime_overrides().unwrap();
}

#[test]
fn explicit_developer_trust_retains_the_long_lived_channel_ceiling() {
    use crate::provenance::{PackageKind, TrustStore, TrustTier, VerifyOptions};
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new(Runtime::Python, "host.py", b"pass\n", true);
    let manifest_path = fixture.app.join("app.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let tool = format!("{APP}.probe");
    manifest["schema_version"] = serde_json::json!(2);
    manifest["mcp"] = serde_json::json!({
        "transport": "stdio", "entry": "host.py",
        "tools": [{"name": tool, "summary": {"en": "Probe"}, "needs": []}]
    });
    fs::write(&manifest_path, manifest.to_string()).unwrap();
    let body = crate::provenance::sign::build_body(
        &fixture.app,
        &crate::provenance::sign::SignRequest {
            kind: PackageKind::App,
            id: APP.to_string(),
            version: "dev".to_string(),
            manifest_schema: "developer".to_string(),
            manifest_path: "app.json".to_string(),
            entrypoints: vec!["host.py".to_string()],
            resources: Vec::new(),
        },
    )
    .unwrap();
    let trust_dir = fixture.root.join("developer");
    fs::create_dir(&trust_dir).unwrap();
    let grants = trust_dir.join("grants.json");
    fs::write(
        &grants,
        serde_json::json!({
            "schema": crate::provenance::trust::DEV_TRUST_SCHEMA_V1,
            "grants": [{
                "kind": "app", "id": APP, "path": fixture.app,
                "content_digest": crate::provenance::envelope::content_digest(&body.files),
                "granted_at": "2026-01-01T00:00:00Z"
            }]
        })
        .to_string(),
    )
    .unwrap();
    fs::set_permissions(&grants, fs::Permissions::from_mode(0o600)).unwrap();
    let uid = crate::provenance::fsec::effective_uid();
    let roots = vec![crate::provenance::trust::TrustRootSpec {
        path: trust_dir,
        tier: TrustTier::Developer,
        allowed_uids: vec![uid],
        domain: crate::provenance::state::TrustDomain::Owner(uid),
    }];
    crate::test_env::record_trust_state(&roots);
    let trust = TrustStore::load_roots(&roots);
    let package = crate::provenance::verify::verify_package(
        &fixture.app,
        &VerifyOptions::new(PackageKind::App).expect_id(APP),
        &trust,
    )
    .unwrap();
    let launch = AppLaunch::new(std::sync::Arc::new(package)).unwrap();
    assert!(launch.ceiling().is_developer());
    assert!(stdio_entry(&launch, OPERATION)
        .unwrap_err()
        .contains("developer-trusted"));
    let error = match stdio_session(&launch, OPERATION, &[]) {
        Ok(_) => panic!("developer-only trust admitted a stdio session"),
        Err(error) => error,
    };
    assert!(error.contains("developer-trusted"), "{error}");
    let (_, refused) = launch.ceiling().clamp(&CapSet::from_caps([
        Cap::new(Verb::SYS_IDENTITY, Scope::name("accounts")),
        Cap::new(Verb::FS_READ, Scope::Wild),
    ]));
    assert_eq!(refused.len(), 2);
}

#[test]
fn broker_requests_preserve_declared_arguments_without_manufacturing_a_parent() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new(Runtime::Python, "host.py", b"pass\n", true);
    let launch = fixture.launch();
    let request = LaunchRequest::Operation {
        operation: OPERATION,
        args: &[],
    };
    let request_for = |parent_caps: Option<&CapSet>| {
        super::super::app_registration_params(APP, &request, parent_caps, &launch.package_ref())
            .unwrap()
    };
    let anonymous = request_for(None);
    assert_eq!(anonymous["app_id"], APP);
    assert_eq!(anonymous["kind"], "operation");
    assert_eq!(anonymous["operation"], OPERATION);
    assert_eq!(anonymous["args"], serde_json::json!([]));
    assert_eq!(
        anonymous["package"],
        serde_json::to_value(launch.package_ref()).unwrap()
    );
    assert!(anonymous.get("parent_caps").is_none());
    let parent = CapSet::from_caps([Cap::new(Verb::AGENT_INVOKE, Scope::name(APP))]);
    let mut narrowed = request_for(Some(&parent));
    assert_eq!(
        narrowed["parent_caps"],
        serde_json::to_value(&parent).unwrap()
    );
    narrowed.as_object_mut().unwrap().remove("parent_caps");
    assert_eq!(narrowed, anonymous);
}

#[test]
fn operation_authorization_never_borrows_another_tools_permissions() {
    let _lock = crate::test_env::lock_env();
    let _local = TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let fixture = Fixture::new(Runtime::Python, "host.py", b"pass\n", true);
    let _parent = TestSessionGuard::admin(&fixture.data);
    let launch = fixture.launch();
    let (session, args) = stdio_session(&launch, OPERATION, &[]).unwrap();
    assert!(args.is_empty());
    assert_eq!(session.granted_caps().iter().count(), 0);
    drop(session);

    let path = fixture.app.join("app.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["operations"][OPERATION]["needs"] = serde_json::json!([{
        "verb": "sys.identity", "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "accounts"}},
        "why": {"en": "Manage accounts only with explicit authorization"}
    }]);
    fs::write(path, manifest.to_string()).unwrap();
    let launch = fixture.launch();
    let mut parent = crate::proc::current_session_info_for_caps().unwrap();
    parent.caps = Some(CapSet::from_caps([Cap::new(
        Verb::AGENT_INVOKE,
        Scope::name(APP),
    )]));
    crate::proc::register_session(parent).unwrap();
    let error = match stdio_session(&launch, OPERATION, &[]) {
        Ok(_) => panic!("a required capability was silently granted"),
        Err(error) => error,
    };
    assert!(error.contains("sys.identity"), "{error}");
}

#[test]
fn launch_gate_does_not_buffer_away_native_message_bytes() {
    let _lock = crate::test_env::lock_env();
    let _runner = crate::test_env::use_stripped_app_runner();
    let fixture = Fixture::new(Runtime::Python, "host.py", b"pass\n", true);
    let payload: Vec<u8> = (0..65_536).map(|index| (index % 256) as u8).collect();
    let mut gated = TOKEN.as_bytes().to_vec();
    gated.extend(&payload);
    let output = std::process::Command::new(crate::bridge::app_runner_path())
        .args(["--launch-gate", TOKEN, "--", "/usr/bin/cat"])
        .stdin(fixture.input(&gated))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, payload);
}

#[cfg(target_os = "linux")]
#[test]
fn nonlisted_app_preserves_native_frames_inside_the_complete_sandbox() {
    let _lock = crate::test_env::lock_env();
    let _runner = crate::test_env::use_stripped_app_runner();
    let _local = TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let source = format!(
        r#"import os, pathlib, socket, struct, sys
assert os.environ["COS_APP_ID"] == "{APP}"
assert os.environ["COS_SESSION"].startswith("app-")
assert os.environ["COS_COMMAND"] == "{OPERATION}"
assert os.environ["COS_ARGS_JSON"] == "[]"
assert os.environ["CLAW_COS_BIN"] == "/usr/local/bin/cos"
assert os.environ["COS_WORKER_SANDBOX"] == "1"
assert "NoNewPrivs:\t1" in pathlib.Path("/proc/self/status").read_text()
data = pathlib.Path(os.environ["COS_DATA_DIR"])
assert data.name == "{APP}" and data.parent.name == "apps"
assert data.stat().st_mode & 0o777 == 0o700
assert pathlib.Path.cwd() == data
assert not (data.parent / "sibling" / "secret").exists()
assert not (data.parent.parent / "credentials").exists()
for name in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XAUTHORITY",
             "COS_PROC_DATA_DIR", "CLAW_APP_RUNNER_BIN"):
    assert name not in os.environ, name
try:
    socket.socket(socket.AF_INET, socket.SOCK_STREAM)
except PermissionError:
    pass
else:
    raise AssertionError("host network is available")
socket.socket(socket.AF_UNIX, socket.SOCK_STREAM).close()
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as broker:
    broker.connect("/run/cos/clawd.sock")
    _, peer_uid, _ = struct.unpack("3i", broker.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
    assert peer_uid == os.getuid(), "worker reached the primary root broker"
while True:
    chunk = os.read(0, 4096)
    if not chunk:
        break
    sys.stdout.buffer.write(chunk)
    sys.stdout.buffer.flush()
"#
    );
    let fixture = Fixture::new(Runtime::Python, "host.py", source.as_bytes(), true);
    fs::create_dir_all(fixture.data.join("apps").join("sibling")).unwrap();
    fs::write(
        fixture.data.join("apps").join("sibling").join("secret"),
        "not exposed",
    )
    .unwrap();
    fs::write(fixture.data.join("credentials"), "not exposed").unwrap();
    let _parent = TestSessionGuard::admin(&fixture.data);
    let _display = TestEnvVarGuard::set("DISPLAY", ":untrusted");
    let _bus = TestEnvVarGuard::set("DBUS_SESSION_BUS_ADDRESS", "unix:path=/untrusted");
    let launch = fixture.launch();
    let (mut session, _) = stdio_session(&launch, OPERATION, &[]).unwrap();
    let binding = launch.bind(&["host.py".to_string()]).unwrap();
    let prepared = fixture.prepare(&launch, &session, &binding);
    let mut frames = Vec::new();
    for id in 0..3 {
        let body = serde_json::to_vec(&serde_json::json!({
            "id": id, "verb": "echo", "args": {"text": "x".repeat(24_000)}
        }))
        .unwrap();
        frames.extend((body.len() as u32).to_le_bytes());
        frames.extend(body);
    }
    let input = fixture.input(&frames);
    let result = execute_stdio(
        &launch,
        &mut session,
        prepared,
        TOKEN,
        input.as_fd(),
        "host.py",
    );
    assert!(
        result.is_ok(),
        "{result:?}: {}",
        fs::read_to_string(fixture.root.join("stderr")).unwrap()
    );
    assert_eq!(fs::read(fixture.root.join("output")).unwrap(), frames);
    assert!(!crate::provenance::runtime::running_instances(
        crate::provenance::runtime::current_owner()
    )
    .unwrap()
    .contains_key(session.id()));
}

#[cfg(target_os = "linux")]
#[test]
fn package_local_binary_uses_the_same_authorized_stdio_sandbox() {
    let _lock = crate::test_env::lock_env();
    let _runner = crate::test_env::use_stripped_app_runner();
    let _local = TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let fixture = Fixture::new(Runtime::Binary, "program", b"", true);
    let source = fixture.root.join("stdio-fixture.rs");
    fs::write(
        &source,
        "fn main() { std::io::copy(&mut std::io::stdin().lock(), &mut std::io::stdout().lock()).unwrap(); }\n",
    )
    .unwrap();
    let compiled = std::process::Command::new("rustc")
        .args([
            "--crate-name",
            "stdio_fixture",
            "-C",
            "opt-level=s",
            "-C",
            "strip=symbols",
        ])
        .arg(&source)
        .arg("-o")
        .arg(fixture.app.join("program"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let _parent = TestSessionGuard::admin(&fixture.data);
    let launch = fixture.launch();
    let (mut session, _) = stdio_session(&launch, OPERATION, &[]).unwrap();
    let binding = launch.bind(&["program".to_string()]).unwrap();
    let prepared = fixture.prepare(&launch, &session, &binding);
    let bytes = b"\x04\x00\x00\x00null\x02\x00\x00\x00{}";
    let input = fixture.input(bytes);
    let result = execute_stdio(
        &launch,
        &mut session,
        prepared,
        TOKEN,
        input.as_fd(),
        "program",
    );
    assert!(
        result.is_ok(),
        "{result:?}: {}",
        fs::read_to_string(fixture.root.join("stderr")).unwrap()
    );
    assert_eq!(fs::read(fixture.root.join("output")).unwrap(), bytes);
}

#[cfg(target_os = "linux")]
#[test]
fn denied_process_binding_never_releases_package_code() {
    let _lock = crate::test_env::lock_env();
    let _runner = crate::test_env::use_stripped_app_runner();
    let _local = TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let fixture = Fixture::new(
        Runtime::Python,
        "host.py",
        b"print('must not execute')\n",
        true,
    );
    let _parent = TestSessionGuard::admin(&fixture.data);
    let launch = fixture.launch();
    let (mut session, _) = stdio_session(&launch, OPERATION, &[]).unwrap();
    let binding = launch.bind(&["host.py".to_string()]).unwrap();
    let prepared = fixture.prepare(&launch, &session, &binding);
    crate::proc::deregister_session(session.id());
    let input = fixture.input(b"");
    assert!(execute_stdio(
        &launch,
        &mut session,
        prepared,
        TOKEN,
        input.as_fd(),
        "host.py"
    )
    .is_err());
    assert!(fs::read(fixture.root.join("output")).unwrap().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn changed_package_bytes_are_rechecked_before_gate_release() {
    let _lock = crate::test_env::lock_env();
    let _runner = crate::test_env::use_stripped_app_runner();
    let _local = TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let fixture = Fixture::new(Runtime::Python, "host.py", b"print('original')\n", true);
    let _parent = TestSessionGuard::admin(&fixture.data);
    let launch = fixture.launch();
    let (mut session, _) = stdio_session(&launch, OPERATION, &[]).unwrap();
    let binding = launch.bind(&["host.py".to_string()]).unwrap();
    let prepared = fixture.prepare(&launch, &session, &binding);
    fs::write(fixture.app.join("host.py"), b"print('forged')\n").unwrap();
    let input = fixture.input(b"");
    let error = execute_stdio(
        &launch,
        &mut session,
        prepared,
        TOKEN,
        input.as_fd(),
        "host.py",
    )
    .unwrap_err();
    assert!(
        error.contains("entrypoint changed after verification"),
        "{error}"
    );
    assert!(fs::read(fixture.root.join("output")).unwrap().is_empty());
}

#[test]
fn a_host_that_ignores_closed_input_has_a_bounded_shutdown() {
    let _lock = crate::test_env::lock_env();
    let _runner = crate::test_env::use_stripped_app_runner();
    let fixture = Fixture::new(Runtime::Python, "host.py", b"pass\n", true);
    let mut child = std::process::Command::new(crate::bridge::app_runner_path())
        .args(["--launch-gate", TOKEN, "--", "/usr/bin/sleep", "30"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let input = fixture.input(b"");
    let result = relay_stdin(
        &mut child,
        input.as_fd(),
        TOKEN,
        Duration::from_millis(25),
        || {},
    );
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let (status, timed_out) = result.unwrap();
    assert!(timed_out);
    assert!(!status.success());
}
