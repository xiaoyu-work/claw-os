use super::*;
use crate::approvals::system_review::{self as reviews, ReviewKind};
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::provenance::sign::{SignRequest, SigningKeyFile};
use crate::provenance::{PackageKind, TrustStore};
use crate::test_env::{TestEnvVarGuard, TestSessionGuard};
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;

const APP: &str = "opaque-cli-fixture";
const OWNER: u32 = 42424;
const GATE: &str = "abcdef0123456789abcdef0123456789";
const SOURCE: &str = r#"import json, os, struct, sys, time
assert os.environ["COS_APP_ID"] == "opaque-cli-fixture"
assert os.environ["COS_SESSION"].startswith("app-")
assert os.environ["COS_WORKER_SANDBOX"] == "1"
assert sys.argv[1:] == []
assert os.environ["COS_COMMAND"] in ("host", "hang")
assert json.loads(os.environ["COS_ARGS_JSON"]) in ([], ["--", "--json"])
def read_exact(length):
    result = b""
    while len(result) < length:
        part = os.read(0, length - len(result))
        if not part:
            if result:
                sys.exit(3)
            return None
        result += part
    return result
while True:
    header = read_exact(4)
    if header is None:
        break
    length, = struct.unpack("<I", header)
    assert 0 < length <= 262144
    payload = read_exact(length)
    assert payload is not None
    sys.stdout.buffer.write(header + payload)
    sys.stdout.buffer.flush()
if os.environ["COS_COMMAND"] == "hang":
    time.sleep(600)
"#;

fn private_tmpfs(path: &str) {
    let path = std::ffi::CString::new(path).unwrap();
    assert_eq!(
        unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                path.as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c"mode=755".as_ptr().cast(),
            )
        },
        0,
        "private mount {}: {}",
        path.to_string_lossy(),
        io::Error::last_os_error()
    );
}

fn isolate() -> tempfile::TempDir {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "fixture requires actual Root"
    );
    let binaries = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let loader_cache = fs::read("/etc/ld.so.cache").unwrap();
    assert_eq!(
        unsafe { libc::unshare(libc::CLONE_NEWNS | libc::CLONE_NEWNET) },
        0
    );
    assert_eq!(
        unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        },
        0
    );
    for path in ["/run", "/etc", "/root", "/var/lib", "/usr/local/bin"] {
        private_tmpfs(path);
    }
    if Path::new("/usr/lib/cos").is_dir() {
        private_tmpfs("/usr/lib/cos");
    }
    fs::write(
        "/etc/passwd",
        "root:x:0:0:Private fixture:/root:/bin/sh\nstdio:x:42424:42424:Private stdio owner:/run/stdio-home:/bin/sh\n",
    ).unwrap();
    fs::write("/etc/group", "root:x:0:\nstdio:x:42424:\n").unwrap();
    fs::write("/etc/ld.so.cache", loader_cache).unwrap();
    for binary in ["cos", "claw-app-runner"] {
        let destination = Path::new("/usr/local/bin").join(binary);
        fs::copy(binaries.join(binary), &destination)
            .unwrap_or_else(|error| panic!("build matching {binary} first: {error}"));
        fs::set_permissions(destination, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let root = tempfile::Builder::new()
        .prefix("cos-stdio-cli-")
        .tempdir_in("/run")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    for path in [
        PathBuf::from("/run/stdio-home"),
        root.path().join("user-data"),
    ] {
        fs::create_dir(&path).unwrap();
        let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::chown(path.as_ptr(), OWNER, OWNER) }, 0);
    }
    root
}

struct RootBroker {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RootBroker {
    fn start(path: PathBuf) -> Self {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                use crate::clawd::transport::{
                    frame::PeerStream, peer, Admission, Limits, ReadOutcome,
                };
                let listener = tokio::net::UnixListener::bind(&path).unwrap();
                peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
                let state = crate::clawd::state::DaemonState::try_new().unwrap();
                let admission = Admission::new(Limits::default());
                ready_tx.send(()).unwrap();
                loop {
                    let (socket, _) = tokio::select! {
                        _ = &mut stop_rx => break,
                        socket = listener.accept() => socket.unwrap(),
                    };
                    let mut stream = PeerStream::new(socket).unwrap();
                    let frame = tokio::time::timeout(
                        Duration::from_secs(10),
                        stream.read_request(crate::clawd::wire::MAX_REQUEST_BYTES),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    let ReadOutcome::Frame(frame) = frame else {
                        panic!("fixture broker expected one actual CLI request");
                    };
                    assert!(!stream.has_pending_input());
                    let peer = peer::verify(frame.credentials).expect("kernel-verified CLI sender");
                    assert_eq!(peer.uid, OWNER);
                    let client = crate::clawd::client_identity::ClientIdentity::from_peer(peer);
                    let request = serde_json::from_slice(&frame.body).unwrap();
                    let response = crate::clawd::server::dispatch_verified_request(
                        request, &client, &state, &admission,
                    )
                    .await;
                    let response = crate::clawd::protocol::encode_response(&response).unwrap();
                    stream.write_response(&response).await.unwrap();
                }
            });
        });
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        Self {
            stop: Some(stop_tx),
            thread: Some(thread),
        }
    }
}

impl Drop for RootBroker {
    fn drop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.thread.take().unwrap().join().unwrap();
    }
}

fn session_rows(owner: u32) -> Vec<crate::proc::SessionInfo> {
    let path = PathBuf::from("/run/cos/caps")
        .join(owner.to_string())
        .join("proc/registry.json");
    let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    serde_json::from_value(value["sessions"].clone()).unwrap()
}

fn sign_app(root: &Path, key: &SigningKeyFile, version: &str, needs: Value) -> AppLaunch {
    let directory = root.join("apps").join(APP);
    fs::create_dir_all(&directory).unwrap();
    let operation = json!({
        "label": {"en": "Opaque fixture"},
        "stdin": true,
        "args": [{"name": "value", "kind": "text", "binding": "positional"}],
        "needs": needs,
    });
    fs::write(
        directory.join("app.json"),
        json!({
            "id": APP, "name": {"en": "Independent opaque fixture"}, "version": version,
            "runtime": "python", "entry": "host.py",
            "operations": {"host": operation, "hang": operation},
        })
        .to_string(),
    )
    .unwrap();
    fs::write(directory.join("host.py"), SOURCE).unwrap();
    for file in ["app.json", "host.py"] {
        fs::set_permissions(directory.join(file), fs::Permissions::from_mode(0o644)).unwrap();
    }
    crate::provenance::sign::sign_directory(
        &directory,
        &SignRequest {
            kind: PackageKind::App,
            id: APP.to_string(),
            version: version.to_string(),
            manifest_schema: "app/v1".into(),
            manifest_path: "app.json".into(),
            entrypoints: vec!["host.py".into()],
            resources: Vec::new(),
        },
        key,
    )
    .unwrap();
    let app = crate::apps::find_verified_fresh(&root.join("apps"), APP).unwrap();
    AppLaunch::new(std::sync::Arc::clone(app.require_verified().unwrap())).unwrap()
}

fn accept(owner: u32, launch: &AppLaunch) {
    let review = reviews::submit(
        owner,
        ReviewKind::AppActivation,
        launch.package(),
        "private actual CLI fixture".into(),
    )
    .unwrap();
    reviews::decide(owner, &review.id, true, Some(launch.package())).unwrap_or_else(|error| {
        panic!(
            "confirm fresh fixture review for owner {owner}, package {}, state {:?}: {error}",
            launch.package_ref().content_digest,
            review.state
        )
    });
}

struct Running {
    child: Child,
    stderr: PathBuf,
    session: String,
    owner: u32,
}

impl Running {
    fn start(root: &Path, parent: &crate::proc::SessionInfo, args: &[&str]) -> Self {
        Self::with_parent(root, Some(parent), args)
    }

    fn with_parent(root: &Path, parent: Option<&crate::proc::SessionInfo>, args: &[&str]) -> Self {
        Self::with_owner(root, parent, args, 0)
    }

    fn user(root: &Path, parent: &crate::proc::SessionInfo, args: &[&str]) -> Self {
        Self::with_owner(root, Some(parent), args, OWNER)
    }

    fn with_owner(
        root: &Path,
        parent: Option<&crate::proc::SessionInfo>,
        args: &[&str],
        owner: u32,
    ) -> Self {
        let session = format!("stdio-caller-{}", uuid::Uuid::new_v4().simple());
        let stderr = root.join(format!("{session}.stderr"));
        let mut child = std::process::Command::new("/usr/local/bin/claw-app-runner")
            .args(["--launch-gate", GATE, "--", "/usr/local/bin/cos"])
            .args(args)
            .current_dir(root)
            .env_clear()
            .env(
                "HOME",
                if owner == 0 {
                    "/root"
                } else {
                    "/run/stdio-home"
                },
            )
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("COS_APPS_DIR", root.join("apps"))
            .env(
                "COS_DATA_DIR",
                root.join(if owner == 0 { "data" } else { "user-data" }),
            )
            .env("COS_CAPS_DATA_DIR", root.join("caps"))
            .env("COS_PROC_DATA_DIR", format!("/run/cos/caps/{owner}"))
            .env("COS_RUNTIME_DIR", root.join("no-daemon"))
            .env("CLAWD_SOCKET", root.join("no-daemon/clawd.sock"))
            .env(
                "COS_SESSION",
                if parent.is_some() {
                    session.as_str()
                } else {
                    ""
                },
            )
            .env("COS_APP_STDIN_MAX_BYTES", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(&stderr).unwrap())
            .uid(owner)
            .gid(owner)
            .spawn()
            .unwrap();
        if let Some(parent) = parent {
            let mut caller = parent.clone();
            caller.session_id = session.clone();
            caller.parent = None;
            caller.pid = child.id();
            caller.start_time_ticks = crate::proc::read_start_time_ticks_pub(child.id());
            caller.command = args.iter().map(|arg| arg.to_string()).collect();
            crate::proc::register_session_for_owner(caller, owner).unwrap();
        }
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(GATE.as_bytes())
            .unwrap();
        Self {
            child,
            stderr,
            session,
            owner,
        }
    }

    fn frame(&mut self, payload: &[u8]) {
        let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(payload);
        let mut input = self.child.stdin.take().unwrap();
        let writer = std::thread::spawn({
            let bytes = bytes.clone();
            move || {
                for chunk in bytes.chunks(2) {
                    input.write_all(chunk).unwrap();
                    input.flush().unwrap();
                }
                input
            }
        });
        let output = self.child.stdout.as_mut().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut actual = vec![0; bytes.len()];
        let mut count = 0;
        while count < actual.len() {
            assert!(
                Instant::now() < deadline,
                "no incremental frame: {}",
                fs::read_to_string(&self.stderr).unwrap()
            );
            let mut poll = libc::pollfd {
                fd: output.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert!(unsafe { libc::poll(&mut poll, 1, 100) } >= 0);
            if poll.revents != 0 {
                let read = output.read(&mut actual[count..]).unwrap();
                assert_ne!(
                    read,
                    0,
                    "premature EOF: {}",
                    fs::read_to_string(&self.stderr).unwrap()
                );
                count += read;
            }
        }
        self.child.stdin = Some(writer.join().unwrap());
        assert_eq!(actual, bytes, "opaque bytes were rewritten");
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "stdin was eagerly closed"
        );
    }

    fn diagnostics(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap()
    }

    fn finish(&mut self, timeout: Duration) -> (ExitStatus, Vec<u8>, String) {
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "CLI did not finish: {}",
                self.diagnostics()
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut remaining = Vec::new();
        let stdout = self.child.stdout.as_mut().unwrap();
        let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let drain_deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match stdout.read_to_end(&mut remaining) {
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < drain_deadline,
                        "protocol stdout survived CLI teardown"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("read remaining CLI output: {error}"),
            }
            assert!(remaining.len() <= 65536, "unexpected trailing CLI output");
        }
        assert!(
            !session_rows(self.owner).iter().any(|row| {
                row.parent.as_deref() == Some(self.session.as_str()) && row.app_id.is_some()
            }),
            "App session survived CLI teardown"
        );
        (status, remaining, self.diagnostics())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.child.try_wait().unwrap().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if self.child.try_wait().unwrap().is_none() {
                self.child.kill().unwrap();
                self.child.wait().unwrap();
            }
        }
        crate::proc::deregister_session_for_owner(&self.session, self.owner);
    }
}

fn refused(root: &Path, parent: &crate::proc::SessionInfo) -> Value {
    let mut process = Running::start(root, parent, &["app", "stdio", APP, "host"]);
    let (status, stdout, stderr) = process.finish(Duration::from_secs(10));
    assert!(!status.success());
    assert!(stdout.is_empty(), "refusal polluted protocol stdout");
    let error: Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(error["code"], "not_authorized");
    assert_eq!(error["details"]["status"], "system_review_required");
    assert_eq!(error["details"]["app_id"], APP);
    error
}

#[test]
#[ignore = "actual Root, private mount/network namespaces, matching cos/runner; includes 300s EOF deadline"]
fn root_stdio_cli_preserves_opaque_transport_and_authority() {
    let _lock = crate::test_env::lock_env();
    let root = isolate();
    let _home = TestEnvVarGuard::set("HOME", "/root");
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path().join("caps"));
    let _fixture = TestEnvVarGuard::remove("COS_TEST_LOCAL_APP_SESSIONS");
    let _runtime = TestEnvVarGuard::set("COS_RUNTIME_DIR", root.path().join("no-daemon"));
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", root.path().join("apps"));
    let _parent = TestSessionGuard::admin(Path::new("/run/cos/caps/0"));
    let mut parent = crate::proc::current_session_info_for_caps().unwrap();
    parent.client.source = crate::session::SessionSource::LocalCli;
    let key = SigningKeyFile::generate(Some("private stdio CLI test publisher".into())).unwrap();
    fs::create_dir_all("/etc/cos/trust/publishers.d").unwrap();
    fs::write(
        "/etc/cos/trust/publishers.d/fixture.json",
        key.trust_entry(&[PackageKind::App]).to_string(),
    )
    .unwrap();
    let roots = TrustStore::default_roots();
    crate::test_env::record_trust_state(&roots);
    crate::provenance::set_trust_store_for_roots(TrustStore::load_roots(&roots), roots);
    let launch = sign_app(root.path(), &key, "1.0.0", json!([]));

    for args in [
        vec!["app", "stdio", "--schema"],
        vec!["app", "stdio", APP, "host", "--schema"],
    ] {
        let mut process = Running::start(root.path(), &parent, &args);
        let (status, stdout, stderr) = process.finish(Duration::from_secs(10));
        assert!(status.success() && stderr.is_empty(), "{stderr}");
        let schema: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(schema["model_callable"], false);
        assert_eq!(schema["output_format"], "opaque");
        assert_eq!(schema["stdin"], true);
    }
    let before = crate::proc::registry_sessions().len();
    let mut unregistered = Running::with_parent(root.path(), None, &["app", "stdio", APP, "host"]);
    let (status, stdout, stderr) = unregistered.finish(Duration::from_secs(10));
    assert!(!status.success() && stdout.is_empty() && !stderr.is_empty());
    assert_eq!(
        crate::proc::registry_sessions().len(),
        before,
        "CLI manufactured a parent"
    );

    let missing = refused(root.path(), &parent);
    let missing_id = missing["details"]["system_review_id"].as_str().unwrap();
    reviews::decide(0, missing_id, false, None).unwrap();
    refused(root.path(), &parent);
    accept(42425, &launch);
    refused(root.path(), &parent);
    accept(0, &launch);
    accept(OWNER, &launch);
    let _broker_proc = TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker_session = TestEnvVarGuard::remove("COS_SESSION");
    let _broker = RootBroker::start(crate::paths::clawd_socket_path());

    let mut app_parent = parent.clone();
    app_parent.client.source = crate::session::SessionSource::App;
    let mut nested = Running::start(root.path(), &app_parent, &["app", "stdio", APP, "host"]);
    let (status, stdout, stderr) = nested.finish(Duration::from_secs(10));
    assert!(!status.success() && stdout.is_empty());
    assert!(stderr.contains("cannot orchestrate App calls"), "{stderr}");

    for wire in ["--wire=1", "--wire=2"] {
        let mut process =
            Running::start(root.path(), &parent, &[wire, "app", "stdio", APP, "host"]);
        let (status, stdout, stderr) = process.finish(Duration::from_secs(10));
        assert!(!status.success() && stdout.is_empty());
        assert!(stderr.contains("wire"), "{stderr}");
    }
    let mut active = Running::user(
        root.path(),
        &parent,
        &["app", "stdio", APP, "host", "--", "--json"],
    );
    let binary: Vec<u8> = (0..65536).map(|index| (index % 256) as u8).collect();
    active.frame(&binary);
    let rows = session_rows(OWNER);
    let session = rows
        .iter()
        .find(|row| {
            row.parent.as_deref() == Some(active.session.as_str())
                && row.app_id.as_deref() == Some(APP)
        })
        .expect("actual worker App session");
    assert!(
        session.caps.as_ref().unwrap().is_empty(),
        "caller Admin leaked into the App"
    );
    active.frame(b"\0\xffopaque\nnot JSON");
    active.child.stdin.take();
    let (status, remaining, stderr) = active.finish(Duration::from_secs(10));
    assert!(status.success(), "{stderr}");
    assert!(remaining.is_empty() && stderr.is_empty(), "{stderr}");

    for close_input in [false, true] {
        let mut process = Running::user(root.path(), &parent, &["app", "stdio", APP, "hang"]);
        process.frame(b"cancel-me");
        if close_input {
            process.child.stdin.take();
        }
        assert_eq!(
            unsafe { libc::kill(process.child.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        let (status, stdout, stderr) = process.finish(Duration::from_secs(10));
        assert!(!status.success() && stdout.is_empty());
        assert!(stderr.contains("cancelled"), "{stderr}");
    }

    let mut held = Running::user(root.path(), &parent, &["app", "stdio", APP, "host"]);
    held.frame(b"held signed version");
    fs::rename(
        root.path().join("apps").join(APP),
        root.path().join("previous-package"),
    )
    .unwrap();
    let replacement = sign_app(root.path(), &key, "1.0.1", json!([]));
    held.frame(b"original stream survives atomic package replacement");
    held.child.stdin.take();
    assert!(held.finish(Duration::from_secs(10)).0.success());
    refused(root.path(), &parent);
    accept(0, &replacement);
    crate::approvals::generations::revoke(&crate::approvals::RevocationScope::Owner {
        uid: Some(0),
    })
    .unwrap();
    refused(root.path(), &parent);

    assert!(
        reviews::has_accepted(OWNER, replacement.package()).unwrap(),
        "the other owner's consumed same-publisher/permission-contract review was lost"
    );
    let mut expired = Running::user(root.path(), &parent, &["app", "stdio", APP, "hang"]);
    expired.frame(b"EOF is bounded");
    let start = Instant::now();
    expired.child.stdin.take();
    let (status, stdout, stderr) = expired.finish(Duration::from_secs(330));
    assert!(start.elapsed() >= Duration::from_secs(300));
    assert!(!status.success() && stdout.is_empty());
    assert!(
        stderr.contains("within 300s after its input closed"),
        "{stderr}"
    );
    eprintln!(
        "actual opaque CLI EOF deadline elapsed: {:?}",
        start.elapsed()
    );

    fs::rename(
        root.path().join("apps").join(APP),
        root.path().join("replaced-package"),
    )
    .unwrap();
    let cap = Cap::new(Verb::SYS_OBSERVE, Scope::name("hardware"));
    let denied = sign_app(
        root.path(),
        &key,
        "1.0.2",
        json!([{
            "verb": cap.verb,
            "scope": {"kind": "fixed", "scope": cap.scope},
            "why": {"en": "Fixture hardware observation requires its own decision"},
        }]),
    );
    accept(0, &denied);
    crate::approvals::app_policy::revoke(0, APP, cap.clone()).unwrap();
    let expected =
        crate::approvals::app_policy::require(0, APP, &CapSet::from_caps([cap])).unwrap_err();
    let mut process = Running::start(root.path(), &parent, &["app", "stdio", APP, "host"]);
    let (status, stdout, stderr) = process.finish(Duration::from_secs(10));
    assert!(!status.success() && stdout.is_empty());
    assert_eq!(stderr.trim(), expected);
}
