// Actual APT/dpkg acceptance after synthetic catalog selection and publisher trust setup.
// The signed repository uses private loopback HTTP, not production HTTPS/catalog keys.
// Retained cases exercise ordinary/MCP custody and installed-product browser/HTTP, not GUI consent.
// No live accounts or publication readiness: Root supplies synthetic owner decisions;
// neither presentation flags nor digests approve. Run each main case in separate Root
// mount/PID/network namespaces through `scripts/ci/test-boundaries.sh app-apt`.

use super::*;
use crate::clawd::app_store as controller;
use crate::clawd::protocol::BrokerErrorKind;
use crate::clawd::state::DaemonState;
use crate::clawd::transport::{frame::PeerStream, peer, ReadOutcome};
use crate::clawd::wire::Request;
use crate::provenance::{sign, PackageKind, TrustStore};
use std::io::{BufRead, Read};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

mod app_sources {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/support/app_sources.rs"));
}
mod app_stage {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/support/app_stage.rs"));
}

const BASE: &str = "/run/cos-app-apt-fixture";
const TRUST_ROOT: &str = crate::provenance::trust::SYSTEM_TRUST_ROOT;
const OWNER: u32 = 61994;
const FOREIGN_UID: u32 = 61995;
const BROKER_GID: u32 = 61996;
const APPS: [&str; 2] = ["apt-fixture-alpha", "apt-fixture-beta"];
const BUNDLE: &str = "claw-app-acceptance-bundle";
const SUPPORT: &str = "claw-app-acceptance-support";
const SIDE_EFFECT: &str = "claw-package-acceptance-side-effect";
const SUPPORT_PATH: &str = "/usr/lib/cos/python/fixture_support.py";
const SUPPORT_BYTES: &[u8] = b"VALUE = 'separately dpkg-owned fixture support'\n";
const PROBE: &str = "clawd::app_store::apt::tests::private_apt_foreign_uid_probe";
const RETAINED_PEER: &str = "clawd::app_store::apt::tests::private_apt_retained_peer";
const ORDINARY_ACTOR: &str = "clawd::app_store::apt::tests::private_apt_ordinary_actor";
const PLUMBING: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/test/unit/clawd/app_store/repository.py"
);

fn checked(command: &mut std::process::Command) -> Vec<u8> {
    let output = command.output().unwrap_or_else(|error| {
        panic!("spawn {}: {error}", command.get_program().to_string_lossy())
    });
    assert!(
        output.status.success(),
        "{command:?}: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn plumbing(action: &str) {
    let mut command = std::process::Command::new("/usr/bin/python3");
    command
        .args(["-I", "-B", PLUMBING, action])
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    private_namespace_environment(&mut command);
    checked(&mut command);
}

fn private_namespace_environment(command: &mut std::process::Command) {
    for namespace in ["MNT", "PID", "NET"] {
        let name = format!("COS_APP_APT_PARENT_{namespace}");
        command.env(
            &name,
            std::env::var(&name).expect("private namespace runner"),
        );
    }
}

fn copy_executable(source: &Path, destination: &Path) {
    std::fs::copy(source, destination).unwrap_or_else(|error| {
        panic!(
            "copy actual binary {} ({} bytes): {error}",
            source.display(),
            source.metadata().unwrap().len()
        )
    });
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(destination.metadata().unwrap().uid(), 0);
}

fn parent(pid: u32) -> u32 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn sign_app(directory: &Path, id: &str, key: &sign::SigningKeyFile) {
    std::fs::create_dir_all(directory.join("web")).unwrap();
    std::fs::write(
        directory.join("main.py"),
        format!(
            r#"
import json, os, pathlib, subprocess, time

def run(command, args):
    if command == "probe":
        return {{"fixture": True}}
    data = pathlib.Path(os.environ["COS_DATA_DIR"])
    (data / "ready.json").write_text(json.dumps({{"ready": True}}))
    if command == "catalog":
        queries = []
        for args in (
            ["search", "{BUNDLE}", "25"], ["show", "{BUNDLE}"],
            ["installed"], ["show", "{SIDE_EFFECT}"],
        ):
            result = subprocess.run(
                ["/usr/local/bin/cos", "--wire=1", "__package_query", *args],
                capture_output=True, text=True, timeout=25,
            )
            queries.append({{"args": args, "code": result.returncode,
                            "reply": json.loads(result.stdout)}})
        temporary = data / "queries.next"
        temporary.write_text(json.dumps({{
            "queries": queries,
            "raw_dpkg_visible": pathlib.Path("/var/lib/dpkg/status").exists(),
            "raw_apt_visible": pathlib.Path("/var/lib/apt/lists").exists(),
        }}))
        os.replace(temporary, data / "queries.json")
    if command == "handoff":
        while not (data / "go").exists():
            time.sleep(0.01)
        result = subprocess.run(
            ["/usr/local/bin/cos", "__package", "--retained", "remove", "{BUNDLE}"],
            capture_output=True, text=True,
        )
        (data / "reply.json").write_text(json.dumps({{
            "code": result.returncode, "stdout": result.stdout, "stderr": result.stderr,
        }}))
    while True:
        time.sleep(1)
"#
        ),
    )
    .unwrap();
    std::fs::write(
        directory.join("server.py"),
        format!(
            r#"
import json, os, subprocess, sys

tools = [
    {{"name": name, "description": name,
      "inputSchema": {{"type": "object", "properties": {{}}, "additionalProperties": False}}}}
    for name in ("fixture.probe", "fixture.remove")
]
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {{"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {{"tools": {{}}}},
                  "serverInfo": {{"name": "private-apt-fixture", "version": "1.0.0"}}}}
    elif method == "tools/list":
        result = {{"tools": tools}}
    elif method == "tools/call":
        context = request["params"]["_meta"]["claw-os.dev/call-context"]
        assert context["caller"]["kind"] == "cli"
        if request["params"]["name"] == "fixture.remove":
            completed = subprocess.run(
                ["/usr/local/bin/cos", "__package", "--retained", "remove", "{BUNDLE}"],
                capture_output=True, text=True,
            )
            result = {{"content": [{{"type": "text", "text": completed.stdout}}],
                      "isError": completed.returncode != 0}}
        else:
            result = {{"content": [{{"type": "text", "text": "real persistent MCP"}}]}}
    else:
        assert method == "ping"
        result = {{}}
    print(json.dumps({{"jsonrpc": "2.0", "id": request["id"], "result": result}}), flush=True)
"#
        ),
    )
    .unwrap();
    std::fs::write(
        directory.join("app.json"),
        json!({
            "schema_version": 2, "id": id, "version": "1.0.0", "runtime": "python",
            "name": {"en": id}, "summary": {"en": "Private APT acceptance App"},
            "mcp": {
                "entry": "server.py", "transport": "stdio", "lifecycle": "lazy",
                "access": {"system_agent": true},
                "tools": [
                    {"name": "fixture.probe", "summary": {"en": "Probe"}, "args": [], "needs": []},
                    {
                        "name": "fixture.remove", "summary": {"en": "Remove own package"},
                        "args": [], "needs": [{
                            "verb": "sys.package",
                            "scope": {"kind": "fixed", "scope": {"kind": "name", "value": BUNDLE}},
                            "why": {"en": "Private persistent MCP self-handoff acceptance"}
                        }]
                    }
                ]
            },
            "operations": {
                "probe": {"label": {"en": "Probe"}, "needs": [], "args": []},
                "hold": {"label": {"en": "Hold"}, "needs": [], "args": []},
                "catalog": {
                    "label": {"en": "Read private package metadata"},
                    "needs": [
                        {
                            "verb": "sys.observe",
                            "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "packages"}},
                            "why": {"en": "Read the private catalog and installed package list"}
                        },
                        {
                            "verb": "sys.observe",
                            "scope": {"kind": "fixed", "scope": {"kind": "name", "value": BUNDLE}},
                            "why": {"en": "Read only this private package's details"}
                        }
                    ],
                    "args": []
                },
                "handoff": {
                    "label": {"en": "Remove own package"},
                    "needs": [{
                        "verb": "sys.package",
                        "scope": {"kind": "fixed", "scope": {"kind": "name", "value": BUNDLE}},
                        "why": {"en": "Private self-handoff acceptance"}
                    }],
                    "args": []
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.join("web-ui.json"),
        json!({
            "schema": "claw.web-app/v1", "app_id": id,
            "source_kind": "product", "source_name": "apt-fixture",
            "title": id, "description": "Private APT acceptance App",
            "entry": "web/index.html"
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.join("web/index.html"),
        "<!doctype html><title>Private APT fixture</title>",
    )
    .unwrap();
    sign::sign_directory(
        directory,
        &sign::SignRequest {
            kind: PackageKind::App,
            id: id.into(),
            version: "1.0.0".into(),
            manifest_schema: "cos.app/v2".into(),
            manifest_path: "app.json".into(),
            entrypoints: vec!["main.py".into(), "server.py".into()],
            resources: vec!["web-ui.json".into(), "web/index.html".into()],
        },
        key,
    )
    .unwrap();
}

fn record(directory: &Path, package_sha256: &str) -> store::Record {
    let package = crate::provenance::verify::verify_package(
        directory,
        &crate::provenance::VerifyOptions::new(PackageKind::App),
        &crate::provenance::trust_store(),
    )
    .unwrap();
    let manifest = std::fs::read(directory.join("app.json")).unwrap();
    let descriptor = std::fs::read(directory.join("web-ui.json")).unwrap();
    let envelope = std::fs::read(directory.join(".provenance.json")).unwrap();
    let mut snapshot = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    snapshot.append_dir_all("package", directory).unwrap();
    let snapshot = snapshot.into_inner().unwrap().finish().unwrap();
    let checksum = store::sha256(&snapshot);
    let record = store::Record {
        app_id: package.id().into(),
        app_version: "1.0.0".into(),
        architecture: "all".into(),
        artifact: store::Artifact {
            file: format!("claw-app-snapshot-{checksum}.tar.gz"),
            size: snapshot.len() as u64,
            sha256: checksum,
            media_type: "application/gzip".into(),
        },
        manifest: store::ManifestBinding {
            path: "app.json".into(),
            size: manifest.len() as u64,
            sha256: store::sha256(&manifest),
        },
        provenance: store::ProvenanceBinding {
            path: ".provenance.json".into(),
            sha256: store::sha256(&envelope),
            content_digest: package.content_digest().into(),
            publisher_key_id: package.audit_facts()["publisher_key_id"]
                .as_str()
                .unwrap()
                .into(),
        },
        release_version: "1.0.0".into(),
        release_tag: "app-apt-fixture-v1.0.0".into(),
        display: store::Display {
            name: package.id().into(),
            description: "Private APT acceptance App".into(),
            source_kind: "product".into(),
            source_name: "apt-fixture".into(),
        },
        web_ui: store::WebBinding {
            path: "web-ui.json".into(),
            sha256: store::sha256(&descriptor),
            entry: "web/index.html".into(),
        },
        delivery: store::AptDelivery {
            kind: "apt".into(),
            package_sha256: package_sha256.into(),
        },
    };
    record.verify_package(&package).unwrap();
    record
}

struct RootGroup(PathBuf);

impl Drop for RootGroup {
    fn drop(&mut self) {
        fn remove_owned(path: &Path) {
            let fd = claw_display_control::workload::open_directory(path).unwrap();
            claw_display_control::workload::check_cgroup(fd.as_fd()).unwrap();
            claw_display_control::workload::retire_cgroup(
                fd.as_fd(),
                std::time::Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    remove_owned(&entry.path());
                }
            }
            std::fs::remove_dir(path).unwrap();
        }
        remove_owned(&self.0);
    }
}

struct LocalRepository(std::process::Child);

impl LocalRepository {
    fn start() -> Self {
        let log = std::fs::File::create(Path::new(BASE).join("http.log")).unwrap();
        let mut command = std::process::Command::new("/usr/bin/python3");
        command
            .args(["-I", "-B", PLUMBING, "serve"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        private_namespace_environment(&mut command);
        let parent = std::process::id() as libc::pid_t;
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != parent
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let mut repository = Self(child);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let address = "127.0.0.1:38081".parse().unwrap();
        loop {
            assert!(
                repository.0.try_wait().unwrap().is_none(),
                "private HTTP repository exited"
            );
            if let Ok(mut stream) =
                std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100))
            {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .write_all(b"GET /dists/stable/InRelease HTTP/1.0\r\nHost: localhost\r\n\r\n")
                    .unwrap();
                let mut response = Vec::new();
                stream.take(16384).read_to_end(&mut response).unwrap();
                assert!(response.starts_with(b"HTTP/1.0 200"), "{response:?}");
                assert!(String::from_utf8_lossy(&response).contains("BEGIN PGP SIGNED MESSAGE"));
                return repository;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "private HTTP repository not ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for LocalRepository {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

#[derive(Clone, Copy, Debug)]
enum Substitution {
    Archive,
    Plan,
    Status,
}

fn substituted_status() -> &'static [u8] {
    b"Package: private-status-substitution\nStatus: install ok installed\nArchitecture: all\nVersion: 1.0.0\nDescription: Fixture concurrent status generation\n\n"
}

fn substitute(id: &str, substitution: Substitution) {
    let mut pending = controller::prepared(id, OWNER).unwrap();
    match substitution {
        Substitution::Archive => {
            let file = &pending.apt.as_ref().unwrap().packages[0].file;
            let path = directory(id).unwrap().join("archives").join(file);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[0] ^= 1;
            std::fs::write(path, bytes).unwrap();
        }
        Substitution::Plan => {
            pending.apt.as_mut().unwrap().digest = store::sha256(b"substituted delivery plan");
            controller::write_json(&controller::request_path(id).unwrap(), &pending).unwrap();
        }
        Substitution::Status => {
            std::fs::write("/var/lib/dpkg/status", substituted_status()).unwrap();
        }
    }
}

struct HeldHook {
    pid: u32,
    files: Vec<String>,
}

enum HookAction {
    Substitute(Substitution),
    HoldAndLoseReply {
        arrived: tokio::sync::oneshot::Sender<HeldHook>,
        release: tokio::sync::oneshot::Receiver<()>,
    },
}

struct LiveCheckBarrier {
    arrived: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
    completed: tokio::sync::oneshot::Sender<(bool, Option<String>)>,
}

#[derive(Debug)]
struct Observation {
    uid: u32,
    pid: u32,
    parent: u32,
    executable: PathBuf,
    ok: bool,
    code: Option<String>,
    message: Option<String>,
}

#[derive(Clone, Debug)]
struct RetainedCall {
    uid: u32,
    command: crate::clawd::routes::Command,
    params: Value,
    result: Option<Value>,
    error: Option<String>,
}

struct Broker {
    action: Arc<Mutex<Option<HookAction>>>,
    live_check: Arc<Mutex<Option<LiveCheckBarrier>>>,
    observed: Arc<Mutex<Vec<Observation>>>,
    calls: Arc<Mutex<Vec<RetainedCall>>>,
    lose_mutation_reply: Arc<std::sync::atomic::AtomicBool>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Broker {
    fn start(
        state: DaemonState,
        admission: Arc<crate::clawd::transport::limits::Admission>,
    ) -> Self {
        let listener = tokio::net::UnixListener::bind("/run/cos/clawd.sock").unwrap();
        peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
        std::fs::set_permissions(
            "/run/cos/clawd.sock",
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        let action: Arc<Mutex<Option<HookAction>>> = Arc::new(Mutex::new(None));
        let live_check = Arc::new(Mutex::new(None::<LiveCheckBarrier>));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let lose_mutation_reply = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let next = action.clone();
        let next_live_check = live_check.clone();
        let captured = observed.clone();
        let captured_calls = calls.clone();
        let lose_next = lose_mutation_reply.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            let (draining, _) = tokio::sync::watch::channel(false);
            loop {
                let accepted = tokio::select! {
                    _ = &mut stopped => break,
                    completed = connections.join_next(), if !connections.is_empty() => {
                        completed.unwrap().unwrap();
                        continue;
                    }
                    accepted = listener.accept() => accepted.unwrap().0,
                };
                let state = state.clone();
                let admission = admission.clone();
                let next = next.clone();
                let next_live_check = next_live_check.clone();
                let captured = captured.clone();
                let captured_calls = captured_calls.clone();
                let lose_next = lose_next.clone();
                let mut drain = draining.subscribe();
                connections.spawn(async move {
                    let mut stream = PeerStream::new(accepted).unwrap();
                    let outcome = tokio::select! {
                        _ = drain.changed() => return,
                        outcome = stream.read_request(crate::clawd::wire::MAX_REQUEST_BYTES) => outcome.unwrap(),
                    };
                    let ReadOutcome::Frame(frame) = outcome
                    else {
                        panic!("expected a real hook/probe request");
                    };
                    let credentials = peer::verify(frame.credentials).unwrap();
                    let uid = credentials.uid;
                    let pid = credentials.pid;
                    let parent = parent(pid);
                    let executable = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
                    let client = ClientIdentity::from_peer(credentials);
                    let request: Request = serde_json::from_slice(&frame.body).unwrap();
                    let command = request.command;
                    let params = request.params.clone();
                    let mut live_check_completed = None;
                    if command == crate::clawd::routes::Command::AppSessionCheckLive && uid == OWNER {
                        let barrier = next_live_check.lock().unwrap().take();
                        if let Some(barrier) = barrier {
                            barrier.arrived.send(()).unwrap();
                            barrier.release.await.unwrap();
                            live_check_completed = Some(barrier.completed);
                        }
                    }
                    let hook = command == crate::clawd::routes::Command::AppStoreAptHook;
                    let action = if hook {
                        next.lock().unwrap().take()
                    } else {
                        None
                    };
                    let mut lose_reply = false;
                    match action {
                        Some(HookAction::Substitute(substitution)) => {
                            substitute(
                                request.params["request_id"].as_str().unwrap(),
                                substitution,
                            );
                        }
                        Some(HookAction::HoldAndLoseReply { arrived, release }) => {
                            arrived
                                .send(HeldHook {
                                    pid,
                                    files: serde_json::from_value(request.params["files"].clone())
                                        .unwrap(),
                                })
                                .unwrap_or_else(|_| panic!("fixture stopped before hook arrival"));
                            release.await.unwrap();
                            lose_reply = true;
                        }
                        None => {}
                    }
                    let dispatch = crate::clawd::server::dispatch_verified_request(
                        request, &client, &state, &admission,
                    );
                    // Closing the frontend may leave its read-only notification poll.
                    // Admitted mutations still run to completion before fixture shutdown.
                    let response = if command == crate::clawd::routes::Command::NotificationSubscribe {
                        tokio::select! {
                            _ = drain.changed() => return,
                            response = dispatch => response,
                        }
                    } else {
                        dispatch.await
                    };
                    if let Some(completed) = live_check_completed {
                        completed.send((response.ok, response.error.as_ref().map(|error| error.code.clone())))
                            .unwrap();
                    }
                    if hook {
                        captured.lock().unwrap().push(Observation {
                            uid,
                            pid,
                            parent,
                            executable,
                            ok: response.ok,
                            code: response.error.as_ref().map(|error| error.code.clone()),
                            message: response.error.as_ref().map(|error| error.message.clone()),
                        });
                    } else {
                        let retained_mutation = command
                            == crate::clawd::routes::Command::SystemPackageControl
                            || command
                                == crate::clawd::routes::Command::ProvenanceTrustWithdrawal
                            || (command == crate::clawd::routes::Command::AppSessionRelay
                                && params["command"]
                                    == crate::clawd::routes::Command::SystemPackageControl
                                        .as_str());
                        captured_calls.lock().unwrap().push(RetainedCall {
                            uid,
                            command,
                            params,
                            result: response.result.clone(),
                            error: response.error.as_ref().map(|error| error.message.clone()),
                        });
                        if retained_mutation
                            && response.ok
                            && lose_next.swap(false, std::sync::atomic::Ordering::SeqCst)
                        {
                            lose_reply = true;
                        }
                    }
                    if !lose_reply {
                        let written = stream
                            .write_response(
                                &crate::clawd::protocol::encode_response(&response).unwrap(),
                            )
                            .await;
                        if hook {
                            written.unwrap();
                        }
                    }
                });
            }
            draining.send_replace(true);
            while let Some(completed) = connections.join_next().await {
                completed.unwrap();
            }
        });
        Self {
            action,
            live_check,
            observed,
            calls,
            lose_mutation_reply,
            stop: Some(stop),
            task: Some(task),
        }
    }

    fn count(&self) -> usize {
        self.observed.lock().unwrap().len()
    }

    async fn shutdown(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

struct Fixture {
    state: DaemonState,
    catalog: store::Catalog,
    server: Broker,
    containment: PathBuf,
    _repository: LocalRepository,
    services: Arc<crate::clawd::app_services::AppServiceManager>,
    _group: RootGroup,
    _environment: Vec<crate::test_env::TestEnvVarGuard>,
}

impl Fixture {
    fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_test_writer()
            .with_env_filter("warn,cos::extension_host::client=debug")
            .try_init();
        assert_eq!(unsafe { libc::getuid() }, 0);
        assert_eq!(unsafe { libc::geteuid() }, 0);
        assert!(std::process::id() > 1, "PID 1 must supervise the private controller");
        for uid in [OWNER, FOREIGN_UID] {
            assert!(
                unsafe { libc::getpwuid(uid) }.is_null(),
                "never borrow a real account"
            );
        }
        assert!(unsafe { libc::getgrgid(BROKER_GID) }.is_null());
        let identities = crate::extension_host::identity::expected_identities(
            crate::extension_host::identity::GROUP_GID,
        );
        assert!(unsafe {
            libc::getgrgid(crate::extension_host::identity::GROUP_GID)
        }.is_null());
        let group = std::ffi::CString::new(crate::extension_host::identity::GROUP_NAME).unwrap();
        assert!(unsafe { libc::getgrnam(group.as_ptr()) }.is_null());
        for identity in &identities {
            let name = std::ffi::CString::new(identity.username.as_str()).unwrap();
            assert!(unsafe { libc::getpwuid(identity.uid) }.is_null());
            assert!(unsafe { libc::getpwnam(name.as_ptr()) }.is_null());
        }
        let helper = PathBuf::from(
            std::env::var_os("COS_APP_APT_TEST_HOOK")
                .expect("compiled claw-app-apt-hook is required"),
        );
        plumbing("isolate");
        Self::prepare_accounts();
        copy_executable(&helper, Path::new(HOOK));
        copy_executable(
            &std::env::current_exe().unwrap(),
            &Path::new(BASE).join("probe"),
        );
        std::fs::write(
            "/usr/lib/cos/app-apt-metadata.py",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../tools/app_apt_metadata.py"
            )),
        )
        .unwrap();
        std::fs::create_dir_all("/run/cos").unwrap();
        std::fs::create_dir_all("/run/lock").unwrap();
        std::fs::set_permissions("/run/lock", std::fs::Permissions::from_mode(0o1777)).unwrap();
        let group = Path::new("/sys/fs/cgroup").join(format!(
            "cos-app-apt-fixture-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&group).unwrap();
        let group = RootGroup(group);
        let environment = vec![
            crate::test_env::TestEnvVarGuard::set("CLAWD_EXTENSION_CGROUP_ROOT", &group.0),
            crate::test_env::TestEnvVarGuard::set(
                "COS_EXTENSION_EXEC_GROUP", crate::extension_host::identity::GROUP_NAME,
            ),
            crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", format!("{BASE}/state")),
            crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", format!("{BASE}/state")),
            crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", "/run/cos"),
            crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", "/var/log/cos"),
            crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", "/usr/lib/cos/apps"),
            crate::test_env::TestEnvVarGuard::set("CLAWD_SOCKET", "/run/cos/clawd.sock"),
        ];
        let roots = TrustStore::default_roots();
        for id in APPS {
            let key = sign::SigningKeyFile::generate(Some(format!("isolated {id}"))).unwrap();
            std::fs::write(
                Path::new(TRUST_ROOT).join(format!("{id}.json")),
                serde_json::to_vec(&key.trust_entry(&[PackageKind::App])).unwrap(),
            )
            .unwrap();
            sign_app(
                &Path::new(BASE)
                    .join("payloads/bundle/usr/lib/cos/apps")
                    .join(id),
                id,
                &key,
            );
        }
        crate::test_env::record_trust_state(&roots);
        let trust = TrustStore::load_roots(&roots);
        assert!(trust.diagnostics().is_empty(), "{:?}", trust.diagnostics());
        crate::provenance::set_trust_store_for_roots(trust, roots);
        let support = Path::new(BASE).join("payloads/support/usr/lib/cos/python");
        std::fs::create_dir_all(&support).unwrap();
        std::fs::write(support.join("fixture_support.py"), SUPPORT_BYTES).unwrap();
        plumbing("repository");
        let repository = LocalRepository::start();
        plumbing("update");
        let inventory = [BUNDLE, SUPPORT]
            .into_iter()
            .map(|name| {
                let path = Path::new(BASE)
                    .join("repo/pool")
                    .join(format!("{name}_1.0.0_all.deb"));
                let (size, sha256) = debian::file_binding(&path).unwrap();
                store::AptPackage {
                    package: name.into(),
                    version: "1.0.0".into(),
                    architecture: "all".into(),
                    size,
                    sha256,
                    apps: if name == BUNDLE {
                        APPS.into_iter().map(str::to_string).collect()
                    } else {
                        vec![]
                    },
                }
            })
            .collect::<Vec<_>>();
        let now = chrono::Utc::now();
        let catalog = store::Catalog {
            schema: "claw.app-store/v2".into(),
            repository: store::REPOSITORY.into(),
            runtime_abi: 1,
            generation: "1".into(),
            issued_at: now,
            expires_at: now + chrono::Duration::hours(1),
            apps: APPS
                .into_iter()
                .map(|id| {
                    record(
                        &Path::new(BASE)
                            .join("payloads/bundle/usr/lib/cos/apps")
                            .join(id),
                        &inventory[0].sha256,
                    )
                })
                .collect(),
            packages: inventory,
        };
        let catalog = store::Catalog::decode(&serde_json::to_vec(&catalog).unwrap(), now).unwrap();
        controller::initialize().unwrap();
        let state = DaemonState::try_new().unwrap();
        let admission = crate::clawd::transport::limits::Admission::new(Default::default());
        let broker = crate::agentd::supervisor::BrokerContext::new(
            state.clone(),
            admission.clone(),
            Path::new("/run/cos/clawd.sock").into(),
        )
        .unwrap();
        let containment = broker.extension_containment().unwrap().path().to_path_buf();
        crate::clawd::app_execution::initialize(&broker);
        crate::clawd::gui::Manager::start(state.clone(), admission.clone()).unwrap();
        let services = crate::clawd::app_services::AppServiceManager::new(broker);
        state.install_app_service_manager(&services).unwrap();
        let server = Broker::start(state.clone(), admission);
        Self {
            state,
            catalog,
            server,
            containment,
            _repository: repository,
            services,
            _group: group,
            _environment: environment,
        }
    }

    async fn prepare(&self) -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let record = self.catalog.apps[0].clone();
        let plan = super::prepare(OWNER, &id, &record, &self.catalog)
            .await
            .unwrap();
        assert_eq!(plan.reviews.len(), 2);
        let support = crate::clawd::packages::query_package_state(SUPPORT).await.unwrap();
        let expected = if support.installed {
            assert_eq!(support.version.as_deref(), Some("1.0.0"));
            BTreeSet::from([BUNDLE])
        } else {
            BTreeSet::from([BUNDLE, SUPPORT])
        };
        assert_eq!(
            plan.packages
                .iter()
                .map(|package| package.identity.package.as_str())
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert!(plan
            .packages
            .iter()
            .all(|package| package.inventory.is_some()));
        let pending = Prepared {
            owner_uid: OWNER,
            request_id: id.clone(),
            review_id: plan.reviews[0].review_id.clone(),
            record,
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(30),
            phase: "ready".into(),
            apt: Some(plan),
        };
        controller::write_json(&controller::request_path(&id).unwrap(), &pending).unwrap();
        id
    }

    fn pending(&self, id: &str) -> Prepared {
        controller::prepared(id, OWNER).unwrap()
    }

    fn approve(&self, id: &str, count: usize) {
        for review in self.pending(id).apt.unwrap().reviews.iter().take(count) {
            let package = verify_source(review).unwrap();
            let approved = reviews::decide(OWNER, &review.review_id, true, Some(&package)).unwrap();
            assert_eq!(
                approved.effective_state().unwrap(),
                reviews::ReviewState::Approved
            );
        }
    }

    fn review_states(&self, id: &str) -> Vec<reviews::ReviewState> {
        self.pending(id)
            .apt
            .unwrap()
            .reviews
            .iter()
            .map(|review| {
                reviews::get(OWNER, &review.review_id)
                    .unwrap()
                    .effective_state()
                    .unwrap()
            })
            .collect()
    }

    async fn commit(&self, id: &str) -> Result<Value, BrokerError> {
        controller::commit_apt_fixture(&self.state, OWNER, id.into(), self.catalog.clone()).await
    }

    fn assert_installed(&self) {
        let discovery = crate::apps::discover_all(Path::new("/usr/lib/cos/apps"));
        assert!(discovery.quarantined.is_empty());
        assert_eq!(
            discovery
                .verified
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            APPS
        );
        for record in &self.catalog.apps {
            let app = discovery.verified.get(&record.app_id).unwrap();
            record
                .verify_package(app.require_verified().unwrap())
                .unwrap();
            for file in [
                "app.json",
                ".provenance.json",
                "main.py",
                "server.py",
                "web-ui.json",
                "web/index.html",
            ] {
                assert_eq!(
                    std::fs::read(app.dir.join(file)).unwrap(),
                    std::fs::read(
                        Path::new(BASE)
                            .join("payloads/bundle/usr/lib/cos/apps")
                            .join(&record.app_id)
                            .join(file)
                    )
                    .unwrap()
                );
            }
            let manifest = format!("/usr/lib/cos/apps/{}/app.json", record.app_id);
            assert_eq!(
                checked(std::process::Command::new("/usr/bin/dpkg-query").args(["-S", &manifest])),
                format!("{BUNDLE}: {manifest}\n").as_bytes()
            );
        }
        assert_eq!(std::fs::read(SUPPORT_PATH).unwrap(), SUPPORT_BYTES);
        assert_eq!(
            checked(std::process::Command::new("/usr/bin/dpkg-query").args(["-S", SUPPORT_PATH])),
            format!("{SUPPORT}: {SUPPORT_PATH}\n").as_bytes()
        );
        let bundle_files =
            checked(std::process::Command::new("/usr/bin/dpkg-query").args(["-L", BUNDLE]));
        assert!(!String::from_utf8(bundle_files)
            .unwrap()
            .lines()
            .any(|line| line == SUPPORT_PATH));
        for package in [BUNDLE, SUPPORT] {
            assert_eq!(
                checked(std::process::Command::new("/usr/bin/dpkg-query").args([
                    "--show",
                    "--showformat=${Status}\\t${Version}\\t${Architecture}",
                    "--",
                    package,
                ])),
                b"install ok installed\t1.0.0\tall"
            );
        }
        assert!(checked(std::process::Command::new("/usr/bin/dpkg").arg("--audit")).is_empty());
    }

    fn assert_no_installation(&self) {
        assert!(crate::apps::discover_all(Path::new("/usr/lib/cos/apps"))
            .verified
            .is_empty());
        assert!(!Path::new(SUPPORT_PATH).exists());
        for package in [BUNDLE, SUPPORT] {
            assert!(!Path::new("/var/lib/dpkg/info")
                .join(format!("{package}.list"))
                .exists());
        }
    }

    async fn finish(mut self) {
        self.server.shutdown().await;
    }

    fn prepare_accounts() {
        use crate::extension_host::identity;
        let mut passwd = std::fs::read_to_string("/etc/passwd").unwrap();
        let mut group = std::fs::read_to_string("/etc/group").unwrap();
        let mut shadow = String::from("root:!:20000:0:99999:7:::\n");
        for uid in [OWNER, FOREIGN_UID] {
            assert!(unsafe { libc::getpwuid(uid) }.is_null());
            assert!(unsafe { libc::getgrgid(uid) }.is_null());
            let home = fixture_home(uid);
            owned_directory(&home, uid);
            for directory in ["data", "state", "log", "cache", "runtime"] {
                owned_directory(&home.join(directory), uid);
            }
            passwd.push_str(&format!(
                "apt-fixture-{uid}:x:{uid}:{uid}:private fixture:{}:/usr/sbin/nologin\n",
                home.display(),
            ));
            group.push_str(&format!("apt-fixture-{uid}:x:{uid}:\n"));
            shadow.push_str(&format!("apt-fixture-{uid}:!:20000:0:99999:7:::\n"));
        }
        group.push_str(&format!(
            "apt-fixture-broker:x:{BROKER_GID}:apt-fixture-{OWNER},apt-fixture-{FOREIGN_UID}\n"
        ));
        group.push_str(&format!("{}:x:{}:\n", identity::GROUP_NAME, identity::GROUP_GID));
        for (index, account) in identity::expected_identities(identity::GROUP_GID).iter().enumerate() {
            passwd.push_str(&format!(
                "{}:x:{}:{}:Claw OS extension slot {index}:{}:{}\n",
                account.username, account.uid, account.gid, identity::IDENTITY_HOME,
                identity::IDENTITY_SHELL,
            ));
            shadow.push_str(&format!("{}:!:20000:0:99999:7:::\n", account.username));
        }
        for (name, body, mode) in [
            ("passwd", passwd, 0o644),
            ("group", group, 0o644),
            ("shadow", shadow, 0o600),
            ("subuid", String::new(), 0o644),
            ("subgid", String::new(), 0o644),
        ] {
            if !Path::new("/etc").join(name).exists() {
                assert!(matches!(name, "subuid" | "subgid"));
                continue;
            }
            let source = Path::new(BASE).join(name);
            std::fs::write(&source, body).unwrap();
            std::fs::set_permissions(&source, std::fs::Permissions::from_mode(mode)).unwrap();
            let source = std::ffi::CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
            let target = std::ffi::CString::new(format!("/etc/{name}")).unwrap();
            assert_eq!(
                unsafe {
                    libc::mount(
                        source.as_ptr(),
                        target.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND,
                        std::ptr::null(),
                    )
                },
                0,
                "private account bind: {}",
                std::io::Error::last_os_error()
            );
        }
        std::fs::create_dir_all("/var/lib/cos").unwrap();
        std::fs::write(
            identity::RESERVATION_MANIFEST,
            identity::reservation_manifest(identity::GROUP_GID),
        )
        .unwrap();
        std::fs::set_permissions(
            identity::RESERVATION_MANIFEST,
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        identity::ExtensionIdentityPool::load(identity::GROUP_GID)
            .expect("complete private package-reserved identity policy");
    }

    fn enable_processes(&self) {
        for (variable, destination) in [
            ("COS_APP_APT_TEST_COS", "/usr/local/bin/cos"),
            ("COS_APP_APT_TEST_RUNNER", "/usr/local/bin/claw-app-runner"),
            ("COS_APP_APT_TEST_HOST", "/usr/local/bin/claw-extension-host"),
        ] {
            copy_executable(
                &PathBuf::from(std::env::var_os(variable).expect("matching compiled OS binary")),
                Path::new(destination),
            );
        }
        assert_eq!(
            unsafe { libc::chown(c"/run/cos/clawd.sock".as_ptr(), 0, BROKER_GID) },
            0
        );
        std::fs::set_permissions(
            "/run/cos/clawd.sock",
            std::fs::Permissions::from_mode(0o660),
        )
        .unwrap();
    }

    async fn install_bundle(&self) {
        let id = self.prepare().await;
        self.approve(&id, 2);
        assert_eq!(self.commit(&id).await.unwrap()["status"], "installed");
        self.assert_installed();
    }
}

fn fixture_home(uid: u32) -> PathBuf {
    Path::new(BASE).join("home").join(uid.to_string())
}

fn owned_directory(path: &Path, uid: u32) {
    std::fs::create_dir_all(path).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(name.as_ptr(), uid, uid) }, 0);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn peer_command(uid: u32, test: &str) -> std::process::Command {
    let mut command = owner_command(uid, &Path::new(BASE).join("probe"));
    command.args(["--exact", test, "--ignored", "--nocapture", "--test-threads=1"]);
    command
}

fn owner_command(uid: u32, program: &Path) -> std::process::Command {
    let home = fixture_home(uid);
    let mut command = std::process::Command::new(program);
    command
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", &home)
        .env("COS_DATA_DIR", home.join("state"))
        .env("COS_CAPS_DATA_DIR", home.join("state"))
        .env("COS_USER_DATA_DIR", home.join("data"))
        .env("COS_LOG_DIR", home.join("log"))
        .env("COS_CACHE_DIR", home.join("cache"))
        .env("COS_RUNTIME_DIR", home.join("runtime"))
        .env("XDG_RUNTIME_DIR", home.join("runtime"))
        .env("COS_PROC_DATA_DIR", format!("/run/cos/caps/{uid}"))
        .env("COS_APPS_DIR", "/usr/lib/cos/apps")
        .env("COS_BIN", "/usr/local/bin/cos")
        .env("CLAWD_SOCKET", "/run/cos/clawd.sock")
        .env("COS_APP_APT_PROCESS", "1")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let parent = std::process::id() as libc::pid_t;
    unsafe {
        command.pre_exec(move || {
            let groups = [BROKER_GID];
            if libc::setgroups(groups.len(), groups.as_ptr()) != 0
                || libc::setresgid(uid, uid, uid) != 0
                || libc::setresuid(uid, uid, uid) != 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                || libc::getppid() != parent
            {
                return Err(std::io::Error::last_os_error());
            }
            crate::agentd::spawn::mark_inherited_descriptors_cloexec(3);
            Ok(())
        });
    }
    command
}

fn register_process_session(
    uid: u32,
    pid: u32,
    id: &str,
    parent: Option<String>,
    caps: crate::caps::CapSet,
) {
    crate::proc::register_session_for_owner(
        crate::proc::SessionInfo {
            session_id: id.into(),
            pid,
            command: vec!["private-apt-fixture".into()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some("user-cli".into()),
            parent,
            workdir: None,
            exit_code: None,
            ended_at: None,
            tier: Some(crate::caps::Role::Automator.credential_tier()),
            scope: None,
            priority: None,
            caps: Some(caps),
            transient_caps: None,
            role: Some(crate::caps::Role::Automator.name().into()),
            app_id: None,
            pending_bind: false,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            client: crate::session::SessionClient::new(
                crate::session::SessionSource::LocalCli,
                false,
                true,
            ),
        },
        uid,
    )
    .unwrap();
}

#[test]
#[ignore = "private real peer of the retained APT fixtures"]
fn private_apt_retained_peer() {
    assert_eq!(std::env::var("COS_APP_APT_PROCESS").unwrap(), "1");
    assert_ne!(unsafe { libc::geteuid() }, 0);
    if let Some(path) = std::env::var_os("COS_APP_APT_CHILD_PID") {
        let parent = std::process::id() as libc::pid_t;
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", RETAINED_PEER, "--ignored", "--nocapture", "--test-threads=1"])
            .env_remove("COS_APP_APT_CHILD_PID")
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != parent
                {
                    return Err(std::io::Error::last_os_error());
                }
                crate::agentd::spawn::mark_inherited_descriptors_cloexec(3);
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        crate::agent::util::atomic_write_with_fsync(
            Path::new(&path), child.id().to_string().as_bytes(),
        ).unwrap();
        assert!(child.wait().unwrap().success());
        return;
    }
    let reply = PathBuf::from(std::env::var_os("COS_APP_APT_REPLY").unwrap());
    for line in std::io::stdin().lock().lines() {
        let request: Request = serde_json::from_str(&line.unwrap()).unwrap();
        let result = if request.command
            == crate::clawd::routes::Command::ProvenanceTrustWithdrawal
            && request.params["private_policy_key_id"].is_string()
        {
            let policy = crate::provenance::cli::run(
                "trust",
                &[
                    "revoke".into(),
                    "--key-id".into(),
                    request.params["private_policy_key_id"].as_str().unwrap().into(),
                ],
            )
            .unwrap();
            json!({"policy": policy})
        } else {
            match crate::clawd::client::request_blocking("/run/cos/clawd.sock", request) {
                Ok(response) => json!({"response": response}),
                Err(error) => json!({"transport_error": error.to_string()}),
            }
        };
        crate::agent::util::atomic_write_with_fsync(&reply, &serde_json::to_vec(&result).unwrap())
            .unwrap();
    }
}

struct RetainedPeer {
    child: std::process::Child,
    peer_pid: u32,
    uid: u32,
    reply: PathBuf,
    session: String,
    parent: String,
}

impl RetainedPeer {
    fn start(uid: u32) -> Self {
        let home = fixture_home(uid);
        let reply = home.join(format!("reply-{}.json", uuid::Uuid::new_v4().simple()));
        let (parent, session) = crate::paths::RoutedPathContext::for_owner(uid, home)
            .scope_sync(|| {
                (
                    crate::session::create("Private retained package parent").unwrap(),
                    crate::session::create("Private retained package caller").unwrap(),
                )
            });
        let parent = parent.as_str().to_string();
        let session = session.as_str().to_string();
        let pid_file = reply.with_extension("pid");
        let mut child = peer_command(uid, RETAINED_PEER)
            .env("COS_APP_APT_REPLY", &reply)
            .env("COS_APP_APT_CHILD_PID", &pid_file)
            .spawn()
            .unwrap();
        register_process_session(
            uid,
            child.id(),
            &parent,
            None,
            crate::caps::CapSet::from_caps([]),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !pid_file.exists() {
            assert!(child.try_wait().unwrap().is_none(), "private task parent exited");
            assert!(std::time::Instant::now() < deadline, "private caller never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        let peer_pid = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        std::fs::remove_file(pid_file).unwrap();
        assert_eq!(self::parent(peer_pid), child.id());
        register_process_session(
            uid,
            peer_pid,
            &session,
            Some(parent.clone()),
            crate::caps::CapSet::from_caps([]),
        );
        Self {
            child,
            peer_pid,
            uid,
            reply,
            session,
            parent,
        }
    }

    fn authorize(&mut self, package: &str) {
        self.authorize_caps(crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::SYS_PACKAGE,
            crate::caps::Scope::name(package),
        )]));
    }

    fn authorize_caps(&mut self, caps: crate::caps::CapSet) {
        use crate::clawd::authority::{
            authority, Audience, AudienceSet, Binding, Issuance, Issuer, Principal, Subject, Uses,
        };
        authority().revoke_session(&self.session);
        register_process_session(
            self.uid,
            self.peer_pid,
            &self.session,
            Some(self.parent.clone()),
            caps.clone(),
        );
        authority()
            .issue(Issuance {
                issuer: Issuer::TrustedSession,
                principal: Principal::of_process(self.uid, self.peer_pid).unwrap(),
                binding: Binding::ProcessTree,
                subject: Subject::session(&self.session),
                audience: AudienceSet::one(Audience::SystemService),
                caps,
                lifetime: Duration::from_secs(120),
                uses: Uses::Budget(1),
                index_session: true,
            })
            .unwrap();
    }

    async fn exchange(&mut self, command: crate::clawd::routes::Command, params: Value) -> Value {
        if self.reply.exists() {
            std::fs::remove_file(&self.reply).unwrap();
        }
        let request = Request::build(command, params);
        writeln!(
            self.child.stdin.as_mut().unwrap(),
            "{}",
            serde_json::to_string(&request).unwrap()
        )
        .unwrap();
        let wait = if command == crate::clawd::routes::Command::AppServiceCliCall {
            Duration::from_millis(crate::extension_host::protocol::READY_TIMEOUT_MS)
                + Duration::from_secs(10)
        } else {
            Duration::from_secs(15)
        };
        let deadline = std::time::Instant::now() + wait;
        while !self.reply.exists() {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "private peer exited"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "private broker request {} did not return",
                command.as_str(),
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        serde_json::from_slice(&std::fs::read(&self.reply).unwrap()).unwrap()
    }

    async fn call(
        &mut self,
        command: crate::clawd::routes::Command,
        params: Value,
    ) -> crate::clawd::protocol::Response {
        let reply = self.exchange(command, params).await;
        serde_json::from_value(
            reply
                .get("response")
                .cloned()
                .expect("authenticated broker reply"),
        )
        .unwrap()
    }

    async fn result(&mut self, command: crate::clawd::routes::Command, params: Value) -> Value {
        let response = self.call(command, params).await;
        assert!(response.ok, "{:?}", response.error);
        response.result.unwrap()
    }

    async fn status(&mut self, id: &str) -> Value {
        self.result(
            crate::clawd::routes::Command::SystemPackageStatus,
            json!({"request_id": id}),
        )
        .await
    }

    async fn wait_phase(&mut self, id: &str, phase: &str) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let status = self.status(id).await;
            if status["phase"] == phase {
                return status;
            }
            assert!(
                !matches!(
                    status["phase"].as_str(),
                    Some("applied" | "not_applied" | "indeterminate")
                ),
                "expected {phase}, got {status}",
            );
            assert!(
                std::time::Instant::now() < deadline,
                "Root controller did not reach {phase}: {status}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn accept(&mut self, action: &str, package: &str) -> String {
        self.authorize(package);
        let receipt = self
            .result(
                crate::clawd::routes::Command::SystemPackageControl,
                json!({
                    "session": self.session, "action": action, "package": package,
                    "invocation_id": uuid::Uuid::new_v4().simple().to_string(),
                }),
            )
            .await;
        assert_eq!(receipt["status"], "accepted");
        assert_eq!(receipt["accepted"], true);
        assert_eq!(receipt["completion"], "query_required");
        receipt["request_id"].as_str().unwrap().into()
    }
}

impl Drop for RetainedPeer {
    fn drop(&mut self) {
        self.child.stdin.take();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while self.child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
        }
        self.child.wait().unwrap();
        crate::clawd::authority::revoke_session(&self.session);
        crate::proc::deregister_session_for_owner(&self.session, self.uid);
        crate::proc::deregister_session_for_owner(&self.parent, self.uid);
    }
}

#[test]
#[ignore = "private real ordinary Host of the retained APT fixtures"]
fn private_apt_ordinary_actor() {
    assert_eq!(std::env::var("COS_APP_APT_PROCESS").unwrap(), "1");
    assert_ne!(unsafe { libc::geteuid() }, 0);
    let roots = TrustStore::default_roots();
    let trust = TrustStore::load_roots(&roots);
    assert!(trust.diagnostics().is_empty(), "{:?}", trust.diagnostics());
    crate::provenance::set_trust_store_for_roots(trust, roots);
    std::io::stdin().read_exact(&mut [0]).unwrap();
    let id = std::env::var("COS_APP_APT_APP").unwrap();
    let operation = std::env::var("COS_APP_APT_OPERATION").unwrap();
    let app = crate::apps::find_verified(Path::new("/usr/lib/cos/apps"), &id).unwrap();
    let launch = crate::bridge::AppLaunch::new(app.require_verified().unwrap().clone()).unwrap();
    let result = crate::bridge::run_app(
        &launch,
        &operation,
        &[],
        fixture_home(unsafe { libc::geteuid() })
            .join("data")
            .to_str()
            .unwrap(),
        "/usr/lib/cos/apps",
    );
    eprintln!("real ordinary fixture returned after retirement: {result:?}");
    std::io::stdin().read_exact(&mut [0]).unwrap();
}

struct OrdinaryActor {
    child: std::process::Child,
    uid: u32,
    app: String,
    parent: String,
    process: Option<OwnedFd>,
    group: PathBuf,
    socket: PathBuf,
}

fn activate_app(uid: u32, app: &str) {
    let installed = crate::apps::find_verified(Path::new("/usr/lib/cos/apps"), app).unwrap();
    let package = installed.require_verified().unwrap();
    let activation = reviews::submit(
        uid,
        reviews::ReviewKind::AppActivation,
        package,
        "Private App acceptance".into(),
    )
    .unwrap();
    if activation.effective_state().unwrap() == reviews::ReviewState::Pending {
        reviews::decide(uid, &activation.id, true, Some(package)).unwrap();
    }
    reviews::consume(uid, &activation.id, package).unwrap();
}

fn stage_real_web_client() -> PathBuf {
    let source = app_sources::app_dir("kv");
    let source_root = app_sources::source_root();
    let _count = crate::test_env::TestEnvVarGuard::set("GIT_CONFIG_COUNT", "1");
    let _key = crate::test_env::TestEnvVarGuard::set("GIT_CONFIG_KEY_0", "safe.directory");
    let _value = crate::test_env::TestEnvVarGuard::set("GIT_CONFIG_VALUE_0", source_root);
    let stage = Path::new(BASE).join("web-client-stage");
    let app = app_stage::app(&source, "capability", "storage-sdk", "kv", &stage);
    let runtime = app_stage::python_runtime(&source, &stage);
    let key = sign::SigningKeyFile::generate(Some("private installed Web client".into())).unwrap();
    std::fs::write(
        Path::new(TRUST_ROOT).join("web-client.json"),
        serde_json::to_vec(&key.trust_entry(&[PackageKind::App])).unwrap(),
    )
    .unwrap();
    let roots = TrustStore::default_roots();
    crate::test_env::record_trust_state(&roots);
    crate::provenance::set_trust_store_for_roots(TrustStore::load_roots(&roots), roots);
    let manifest: Value = serde_json::from_slice(&std::fs::read(app.join("app.json")).unwrap()).unwrap();
    sign::sign_directory(
        &app,
        &sign::SignRequest {
            kind: PackageKind::App,
            id: "kv".into(),
            version: manifest["version"].as_str().unwrap().into(),
            manifest_schema: "cos.app/v2".into(),
            manifest_path: "app.json".into(),
            entrypoints: vec![manifest["mcp"]["entry"].as_str().unwrap().into()],
            resources: vec!["web-ui.json".into(), "web/index.html".into()],
        },
        &key,
    )
    .unwrap();
    checked(std::process::Command::new("cp").arg("-a").arg(&app).arg("/usr/lib/cos/apps"));
    checked(std::process::Command::new("cp").arg("-a").arg(runtime.join(".")).arg("/usr/lib/cos/python"));
    let installed = PathBuf::from("/usr/lib/cos/apps/kv");
    crate::apps::find_verified(Path::new("/usr/lib/cos/apps"), "kv").unwrap();
    activate_app(OWNER, "kv");
    installed
}

const WEB_ORIGIN: &str = "http://127.0.0.1:38082";

struct WebOwner {
    child: std::process::Child,
    session: String,
}

impl WebOwner {
    async fn start() -> (Self, reqwest::Client, String) {
        use crate::clawd::authority::{
            authority, Audience, AudienceSet, Binding, Issuance, Issuer, Principal, Subject, Uses,
        };
        let session = crate::paths::RoutedPathContext::for_owner(OWNER, fixture_home(OWNER))
            .scope_sync(|| crate::session::create("Private installed Web App acceptance"))
            .unwrap().as_str().to_string();
        let log = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .open(Path::new(BASE).join("web-host.log")).unwrap();
        let child = owner_command(OWNER, Path::new("/usr/local/bin/cos"))
            .args(["agent", "serve", "--bind", "127.0.0.1", "--port", "38082"])
            .env("COS_SESSION", &session)
            .stdin(Stdio::null()).stderr(Stdio::from(log)).spawn().unwrap();
        let caps = crate::caps::CapSet::from_caps([
            crate::agent::tools::app_gateway::invoke_cap("kv", "kv.get").unwrap(),
            crate::agent::tools::app_gateway::invoke_cap("kv", "kv.set").unwrap(),
            crate::caps::Cap::new(crate::caps::Verb::DATA_KV_READ, crate::caps::Scope::Wild),
            crate::caps::Cap::new(crate::caps::Verb::DATA_KV_WRITE, crate::caps::Scope::name("web-proof")),
        ]);
        register_process_session(OWNER, child.id(), &session, None, caps.clone());
        authority().issue(Issuance {
            issuer: Issuer::TrustedSession,
            principal: Principal::of_process(OWNER, child.id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session),
            audience: AudienceSet::one(Audience::SystemService),
            caps,
            lifetime: Duration::from_secs(120),
            uses: Uses::Unbounded,
            index_session: true,
        }).unwrap();
        let mut actor = Self { child, session };
        let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if client.get(format!("{WEB_ORIGIN}/")).send().await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            assert!(actor.child.try_wait().unwrap().is_none(), "real Web server exited before readiness");
            assert!(std::time::Instant::now() < deadline, "real Web server did not become ready");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let bootstrap = std::fs::read_to_string(fixture_home(OWNER).join("state/agent/web/serve.token")).unwrap();
        let response = client.post(format!("{WEB_ORIGIN}/api/auth/token"))
            .header("Origin", WEB_ORIGIN).header("Authorization", format!("Bootstrap {}", bootstrap.trim()))
            .json(&json!({"ttl_seconds": 120})).send().await.unwrap();
        assert!(response.status().is_success(), "real owner token exchange failed");
        let token = response.json::<Value>().await.unwrap()["access_token"].as_str().unwrap().to_string();
        let meta = client.get(format!("{WEB_ORIGIN}/api/meta")).bearer_auth(&token)
            .send().await.unwrap().error_for_status().unwrap().json::<Value>().await.unwrap();
        assert_eq!(meta["owner_uid"], OWNER);
        (actor, client, token)
    }
}

impl Drop for WebOwner {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
        }
        self.child.wait().unwrap();
        crate::clawd::authority::revoke_session(&self.session);
        crate::proc::deregister_session_for_owner(&self.session, OWNER);
    }
}

async fn web_post(client: &reqwest::Client, token: &str, path: &str, body: Value) -> Value {
    let response = client.post(format!("{WEB_ORIGIN}{path}"))
        .header("Origin", WEB_ORIGIN).bearer_auth(token).json(&body).send().await.unwrap();
    let status = response.status();
    let value = response.json::<Value>().await.unwrap();
    assert!(status.is_success(), "{path}: {status}: {value}");
    value
}

fn installed_product_browser(token: &str, value: &str) {
    let script = Path::new(BASE).join("installed-app.browser.mjs");
    std::fs::write(&script, include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"), "/src/agent/web/ui/test/app-store.browser.mjs",
    ))).unwrap();
    let token_file = fixture_home(OWNER).join("runtime/installed-browser.token");
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
        .open(&token_file).unwrap();
    file.write_all(token.as_bytes()).unwrap();
    let name = std::ffi::CString::new(token_file.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(name.as_ptr(), OWNER, OWNER) }, 0);
    drop(file);
    let mut command = owner_command(OWNER, Path::new("/usr/local/bin/node"));
    command.arg("--experimental-websocket").arg(&script)
        .env("APP_STORE_INSTALLED_ORIGIN", WEB_ORIGIN)
        .env("APP_STORE_INSTALLED_TOKEN_FILE", &token_file)
        .env("APP_STORE_INSTALLED_VALUE", value)
        .stdout(Stdio::piped());
    let output = checked(&mut command);
    assert!(String::from_utf8_lossy(&output).contains("PASS installed signed product UI"));
    std::fs::remove_file(token_file).unwrap();
}

impl OrdinaryActor {
    fn start(uid: u32, app: &str, operation: &str) -> Self {
        activate_app(uid, app);
        let parent = uuid::Uuid::new_v4().to_string();
        let mut child = peer_command(uid, ORDINARY_ACTOR)
            .env("COS_SESSION", &parent)
            .env("COS_APP_APT_APP", app)
            .env("COS_APP_APT_OPERATION", operation)
            .spawn()
            .unwrap();
        let mut caps = crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::AGENT_INVOKE,
            crate::caps::Scope::name(app),
        )]);
        if operation == "handoff" {
            caps.insert(crate::caps::Cap::new(
                crate::caps::Verb::SYS_PACKAGE,
                crate::caps::Scope::name(BUNDLE),
            ));
        } else if operation == "catalog" {
            for name in ["packages", BUNDLE] {
                caps.insert(crate::caps::Cap::new(
                    crate::caps::Verb::SYS_OBSERVE,
                    crate::caps::Scope::name(name),
                ));
            }
        }
        register_process_session(uid, child.id(), &parent, None, caps);
        child.stdin.as_mut().unwrap().write_all(&[1]).unwrap();
        Self {
            child,
            uid,
            app: app.into(),
            parent,
            process: None,
            group: PathBuf::new(),
            socket: PathBuf::new(),
        }
    }

    fn data(&self) -> PathBuf {
        fixture_home(self.uid).join("data/apps").join(&self.app)
    }

    async fn ready(&mut self, fixture: &Fixture) {
        let ready = self.data().join("ready.json");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !ready.exists() {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "ordinary Host exited before bind"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "ordinary App never crossed Root custody gate"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(std::fs::metadata(ready).unwrap().uid(), self.uid);
        let calls = fixture.server.calls.lock().unwrap();
        let registration = calls
            .iter()
            .find(|call| {
                call.uid == self.uid
                    && call.command == crate::clawd::routes::Command::AppSessionRegister
            })
            .unwrap();
        assert_eq!(registration.result.as_ref().unwrap()["root_custody"], true);
        let bind = calls
            .iter()
            .find(|call| {
                call.uid == self.uid
                    && call.command == crate::clawd::routes::Command::AppSessionBind
            })
            .unwrap();
        assert!(bind.result.is_some());
        self.socket = bind.params["custody_socket"].as_str().unwrap().into();
        self.group = fixture
            .containment
            .join(bind.params["session_id"].as_str().unwrap());
        let pid = bind.params["pid"].as_u64().unwrap() as u32;
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        assert!(raw >= 0);
        self.process = Some(unsafe { OwnedFd::from_raw_fd(raw as i32) });
        self.assert_running();
    }

    fn assert_running(&self) {
        assert!(!claw_display_control::identity::pidfd_exited(
            self.process.as_ref().unwrap().as_fd()
        )
        .unwrap());
        assert!(self.group.is_dir());
        assert!(self.socket.exists());
    }

    fn assert_retired(&self) {
        assert!(claw_display_control::identity::pidfd_exited(
            self.process.as_ref().unwrap().as_fd()
        )
        .unwrap());
        assert!(!self.group.exists(), "Root left an affected App cgroup");
        assert!(
            !self.socket.parent().unwrap().exists(),
            "Host resources lack checked cleanup"
        );
    }

    async fn finish(mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.stdin.as_mut().unwrap().write_all(&[2]).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while self.child.try_wait().unwrap().is_none() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "ordinary Host failed checked retirement"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for OrdinaryActor {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
        }
        self.child.wait().unwrap();
        crate::proc::deregister_session_for_owner(&self.parent, self.uid);
    }
}

fn dpkg_state() -> BTreeMap<PathBuf, Vec<u8>> {
    fn read(path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                read(&entry.path(), result);
            } else {
                result.insert(entry.path(), std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut result = BTreeMap::new();
    read(Path::new("/var/lib/dpkg"), &mut result);
    if Path::new("/var/log/dpkg.log").exists() {
        result.insert(
            "/var/log/dpkg.log".into(),
            std::fs::read("/var/log/dpkg.log").unwrap(),
        );
    }
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires actual Root mount/network namespaces, cgroups, python3-apt and COS_APP_APT_TEST_HOOK"]
async fn private_apt_bundle_requires_both_reviews_and_owns_shared_dependency() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let id = fixture.prepare().await;
    let before = dpkg_state();
    fixture.approve(&id, 1);
    let refusal = fixture.commit(&id).await.unwrap_err();
    assert_eq!(refusal.kind, BrokerErrorKind::Unauthorized);
    assert!(refusal.message.contains("Every App"));
    assert_eq!(
        fixture.server.count(),
        0,
        "one review must not even dispatch APT"
    );
    assert_eq!(dpkg_state(), before);
    fixture.assert_no_installation();
    let pending = fixture.pending(&id);
    let second = &pending.apt.as_ref().unwrap().reviews[1];
    let package = verify_source(second).unwrap();
    reviews::decide(OWNER, &second.review_id, true, Some(&package)).unwrap();
    assert_eq!(fixture.commit(&id).await.unwrap()["status"], "installed");
    fixture.assert_installed();
    assert_eq!(
        fixture.review_states(&id),
        vec![reviews::ReviewState::Consumed; 2]
    );
    let observed = fixture.server.observed.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].uid, 0);
    assert_eq!(observed[0].executable, Path::new(HOOK));
    assert!(observed[0].ok, "{observed:?}");
    drop(observed);
    let installed = dpkg_state();
    assert_eq!(fixture.commit(&id).await.unwrap()["recovered"], true);
    assert_eq!(dpkg_state(), installed);
    assert_eq!(
        fixture.server.count(),
        1,
        "checked recovery must not dispatch APT"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires actual Root mount/network namespaces, cgroups, python3-apt and COS_APP_APT_TEST_HOOK"]
async fn private_apt_refuses_substituted_bytes_plan_and_status_before_dpkg() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    for (substitution, expected) in [
        (Substitution::Archive, "archive changed"),
        (Substitution::Plan, "delivery plan differs"),
        (Substitution::Status, "APT state"),
    ] {
        let id = fixture.prepare().await;
        fixture.approve(&id, 2);
        let mut before = dpkg_state();
        *fixture.server.action.lock().unwrap() = Some(HookAction::Substitute(substitution));
        assert!(fixture.commit(&id).await.is_err(), "{substitution:?}");
        if matches!(substitution, Substitution::Status) {
            before.insert("/var/lib/dpkg/status".into(), substituted_status().to_vec());
        }
        assert_eq!(
            dpkg_state(),
            before,
            "{substitution:?}: dpkg must not have run"
        );
        fixture.assert_no_installation();
        assert!(!fixture.pending(&id).apt.unwrap().hook_consumed);
        assert_eq!(
            fixture.review_states(&id),
            vec![reviews::ReviewState::Approved; 2]
        );
        let observed = fixture.server.observed.lock().unwrap();
        let refusal = observed.last().unwrap();
        assert!(!refusal.ok, "{observed:?}");
        assert_eq!(refusal.code.as_deref(), Some("not_authorized"));
        assert!(
            refusal.message.as_deref().unwrap().contains(expected),
            "{observed:?}"
        );
    }
    fixture.finish().await;
}

#[test]
#[ignore = "private dropped-UID actor of private_apt_hook_identity_and_lost_reply_never_replay"]
fn private_apt_foreign_uid_probe() {
    assert_eq!(std::env::var("COS_APP_APT_UID_PROBE").unwrap(), "1");
    assert_eq!(unsafe { libc::getuid() }, FOREIGN_UID);
    assert_eq!(unsafe { libc::geteuid() }, FOREIGN_UID);
    let response = crate::clawd::client::request_blocking(
        crate::clawd::config::socket_path(),
        Request::build(
            crate::clawd::routes::Command::AppStoreAptHook,
            json!({
                "request_id": std::env::var("COS_APP_APT_PROBE_REQUEST").unwrap(),
                "files": serde_json::from_str::<Value>(
                    &std::env::var("COS_APP_APT_PROBE_FILES").unwrap()
                ).unwrap()
            }),
        ),
    )
    .unwrap();
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "not_authorized");
}

async fn child_output(
    command: std::process::Command,
    input: Option<Vec<u8>>,
) -> std::process::Output {
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let mut child = command.spawn().unwrap();
    if let Some(bytes) = input {
        child.stdin.take().unwrap().write_all(&bytes).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires actual Root mount/network namespaces, cgroups, python3-apt and COS_APP_APT_TEST_HOOK"]
async fn private_apt_hook_identity_and_lost_reply_never_replay() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let id = fixture.prepare().await;
    fixture.approve(&id, 2);
    let before = dpkg_state();
    let (arrived, waiting) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    *fixture.server.action.lock().unwrap() = Some(HookAction::HoldAndLoseReply {
        arrived,
        release: released,
    });
    let state = fixture.state.clone();
    let catalog = fixture.catalog.clone();
    let request = id.clone();
    let commit = tokio::spawn(async move {
        controller::commit_apt_fixture(&state, OWNER, request, catalog).await
    });
    let held = tokio::time::timeout(Duration::from_secs(30), waiting)
        .await
        .unwrap()
        .unwrap();
    let context = hooks().lock().unwrap().get(&id).unwrap().clone();
    assert_eq!(parent(held.pid), context.pid);
    assert_eq!(
        std::fs::read_link(format!("/proc/{}/exe", context.pid)).unwrap(),
        Path::new("/usr/bin/apt-get")
    );
    assert_eq!(
        crate::proc::read_start_time_ticks_pub(context.pid),
        Some(context.start)
    );
    assert!(
        crate::clawd::app_mutation::admit("apt-fixture-unrelated").is_err(),
        "the actual APT frontend lock must block unrelated new App admissions"
    );
    let mut probe = std::process::Command::new(Path::new(BASE).join("probe"));
    probe
        .args([
            "--exact",
            PROBE,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", BASE)
        .env("CLAWD_SOCKET", "/run/cos/clawd.sock")
        .env("COS_APP_APT_UID_PROBE", "1")
        .env("COS_APP_APT_PROBE_REQUEST", &id)
        .env(
            "COS_APP_APT_PROBE_FILES",
            serde_json::to_string(&held.files).unwrap(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        probe.pre_exec(|| {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(FOREIGN_UID, FOREIGN_UID, FOREIGN_UID) != 0
                || libc::setresuid(FOREIGN_UID, FOREIGN_UID, FOREIGN_UID) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            crate::agentd::spawn::mark_inherited_descriptors_cloexec(3);
            Ok(())
        });
    }
    let output = child_output(probe, None).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut foreign_parent = std::process::Command::new(HOOK);
    foreign_parent
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", BASE)
        .env("CLAWD_SOCKET", "/run/cos/clawd.sock")
        .env("CLAW_APP_STORE_REQUEST", &id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = child_output(
        foreign_parent,
        Some(format!("{}\n", held.files.join("\n")).into_bytes()),
    )
    .await;
    assert!(
        !output.status.success(),
        "a Root helper with the wrong parent must fail"
    );
    assert_eq!(dpkg_state(), before);
    fixture.assert_no_installation();
    assert!(!fixture.pending(&id).apt.unwrap().hook_consumed);
    {
        let observed = fixture.server.observed.lock().unwrap();
        assert_eq!(observed.len(), 2, "{observed:?}");
        assert_eq!(observed[0].uid, FOREIGN_UID);
        assert_eq!(observed[0].code.as_deref(), Some("not_authorized"));
        assert_eq!(observed[1].uid, 0);
        assert_eq!(observed[1].executable, Path::new(HOOK));
        assert_ne!(observed[1].parent, context.pid);
        assert!(observed[1]
            .message
            .as_deref()
            .unwrap()
            .contains("transaction child"));
    }
    release.send(()).unwrap();
    let refusal = tokio::time::timeout(Duration::from_secs(30), commit)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(refusal.kind, BrokerErrorKind::Indeterminate);
    let pending = fixture.pending(&id);
    assert_eq!(pending.phase, "indeterminate");
    assert!(pending.apt.unwrap().hook_consumed);
    assert_eq!(
        fixture.review_states(&id),
        vec![reviews::ReviewState::Consumed; 2]
    );
    assert_eq!(
        dpkg_state(),
        before,
        "lost hook acknowledgement must stop dpkg"
    );
    fixture.assert_no_installation();
    for app in APPS {
        assert!(crate::clawd::app_mutation::admit(app).is_err());
    }
    let observed = fixture.server.count();
    assert_eq!(
        fixture.commit(&id).await.unwrap_err().kind,
        BrokerErrorKind::Indeterminate
    );
    assert_eq!(
        fixture.server.count(),
        observed,
        "recovery cannot replay consumed reviews"
    );
    assert_eq!(dpkg_state(), before);
    let observed = fixture.server.observed.lock().unwrap();
    let accepted = observed.last().unwrap();
    assert_eq!(accepted.pid, held.pid);
    assert!(accepted.ok, "{observed:?}");
    drop(observed);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires actual Root mount/network namespaces, cgroups, python3-apt and COS_APP_APT_TEST_HOOK"]
async fn private_apt_completed_dispatch_failure_recovers_read_only() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let id = fixture.prepare().await;
    fixture.approve(&id, 2);
    // This is private OS APT configuration, not an App maintainer script or an approval hook.
    std::fs::write(
        "/etc/apt/apt.conf.d/99fixture-post-dpkg",
        "DPkg::Post-Invoke { \"/usr/bin/false\"; };\n",
    )
    .unwrap();
    assert_eq!(
        fixture.commit(&id).await.unwrap_err().kind,
        BrokerErrorKind::Indeterminate
    );
    let pending = fixture.pending(&id);
    assert_eq!(pending.phase, "indeterminate");
    assert!(pending.apt.unwrap().hook_consumed);
    assert_eq!(
        fixture.review_states(&id),
        vec![reviews::ReviewState::Consumed; 2]
    );
    fixture.assert_installed();
    for app in APPS {
        assert!(crate::clawd::app_mutation::admit(app).is_err());
    }
    let installed = dpkg_state();
    let dispatched = fixture.server.count();
    assert_eq!(dispatched, 1);
    let recovered = fixture.commit(&id).await.unwrap();
    assert_eq!(recovered["status"], "installed");
    assert_eq!(recovered["recovered"], true);
    assert_eq!(dpkg_state(), installed, "exact recovery is read-only");
    assert_eq!(fixture.server.count(), dispatched);
    assert_eq!(fixture.pending(&id).phase, "installed");
    for app in APPS {
        assert!(crate::clawd::app_mutation::admit(app).is_ok());
    }
    fixture.finish().await;
}

fn retained_record(id: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(
            Path::new("/var/lib/cos/package-transactions/requests").join(format!("{id}.json")),
        )
        .unwrap(),
    )
    .unwrap()
}

fn assert_children(id: &str, count: usize, success: bool) {
    let record = retained_record(id);
    let children = record["children"].as_array().unwrap();
    assert_eq!(children.len(), count, "{record}");
    for child in children {
        assert_eq!(child["ended"], true);
        let code = child["exit_code"]
            .as_i64()
            .expect("actual normal child exit was not retained");
        assert_eq!(code == 0, success, "{child}");
    }
}

async fn decide_global(
    owner: u32,
    view: &clawd_client::system_review::SystemReview,
    action: clawd_client::system_review::ReviewAction,
) -> crate::clawd::protocol::Response {
    crate::clawd::client::request(
        "/run/cos/clawd.sock",
        Request::build(
            crate::clawd::routes::Command::SystemReviewDecide,
            json!({"owner_uid": owner, "review": {
                "id": view.id, "revision": view.revision, "action": action, "choices": [],
            }}),
        ),
    )
    .await
    .unwrap()
}

async fn global_view(
    peer: &mut RetainedPeer,
    id: &str,
) -> clawd_client::system_review::SystemReview {
    serde_json::from_value(
        peer.result(
            crate::clawd::routes::Command::SystemReviewShow,
            json!({"id": id}),
        )
        .await,
    )
    .unwrap()
}

async fn refused_recovery(peer: &mut RetainedPeer, id: &str) -> crate::clawd::protocol::Response {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let response = peer
            .call(
                crate::clawd::routes::Command::SystemPackageRecover,
                json!({"request_id": id}),
            )
            .await;
        if !response.ok {
            return response;
        }
        assert_eq!(response.result.unwrap()["phase"], "indeterminate");
        assert!(
            std::time::Instant::now() < deadline,
            "Root controller never released its completed failure"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn persistent_mcp_self_handoff(fixture: &Fixture, peer: &mut RetainedPeer) {
    use crate::clawd::routes::Command;
    fixture.install_bundle().await;
    activate_app(OWNER, APPS[0]);
    peer.authorize_caps(crate::caps::CapSet::from_caps([
        crate::caps::Cap::new(
            crate::caps::Verb::SYS_PACKAGE,
            crate::caps::Scope::name(BUNDLE),
        ),
        crate::agent::tools::app_gateway::invoke_cap(APPS[0], "fixture.probe").unwrap(),
        crate::agent::tools::app_gateway::invoke_cap(APPS[0], "fixture.remove").unwrap(),
    ]));
    peer.result(
        Command::AppServiceCliCall,
        json!({"app_id": APPS[0], "tool": "fixture.probe", "arguments": {}}),
    )
    .await;
    let bindings = fixture.services.retained_trust_bindings(Some(OWNER)).await.unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].owner, OWNER);
    assert_eq!(bindings[0].app, APPS[0]);
    let session = crate::proc::try_registry_sessions_for_owner(OWNER)
        .unwrap()
        .into_iter()
        .find(|session| {
            session.app_id.as_deref() == Some(APPS[0])
                && session.group.as_deref() == Some("app-mcp")
        })
        .expect("a real bound persistent App MCP process");
    assert!(!session.pending_bind);
    let status = std::fs::read_to_string(format!("/proc/{}/status", session.pid)).unwrap();
    let execution_uid = status.lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap().parse::<u32>().unwrap();
    let first = crate::extension_host::identity::FIRST_UID;
    assert!((first..first + crate::extension_host::identity::IDENTITY_COUNT).contains(&execution_uid));
    assert_ne!(execution_uid, OWNER);
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, session.pid, 0) };
    assert!(raw >= 0);
    let process = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let before = peer.result(Command::SystemPackageStatus, json!({})).await;
    let previous: BTreeSet<_> = before["transactions"].as_array().unwrap().iter()
        .map(|record| record["request_id"].as_str().unwrap().to_string()).collect();
    let reply = peer.call(
        Command::AppServiceCliCall,
        json!({"app_id": APPS[0], "tool": "fixture.remove", "arguments": {}}),
    ).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let id = loop {
        let current = peer.result(Command::SystemPackageStatus, json!({})).await;
        if let Some(record) = current["transactions"].as_array().unwrap().iter()
            .find(|record| !previous.contains(record["request_id"].as_str().unwrap()))
        {
            assert_eq!(record["app_id"], APPS[0]);
            assert_eq!(record["session"], session.session_id);
            break record["request_id"].as_str().unwrap().to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "MCP did not hand off to Root: {reply:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let applied = peer.wait_phase(&id, "applied").await;
    assert_eq!(applied["completion"]["confirmed"], true);
    assert_children(&id, 1, true);
    assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
    assert!(fixture.services.retained_trust_bindings(Some(OWNER)).await.unwrap().is_empty());
    assert!(!crate::clawd::packages::query_package_state(BUNDLE).await.unwrap().installed);
    for app in APPS {
        assert!(crate::clawd::app_mutation::admit(app).is_ok());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/PID/network namespaces and matching App Host executables"]
async fn private_apt_owner_withdrawal_denies_live_calls_and_retires_only_owner() {
    use crate::clawd::routes::Command;
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    fixture.enable_processes();
    fixture.install_bundle().await;
    let mut owner_app = OrdinaryActor::start(OWNER, APPS[0], "handoff");
    let mut foreign_app = OrdinaryActor::start(FOREIGN_UID, APPS[0], "hold");
    owner_app.ready(&fixture).await;
    foreign_app.ready(&fixture).await;
    let mut owner = RetainedPeer::start(OWNER);
    let mut foreign = RetainedPeer::start(FOREIGN_UID);
    let invoke = crate::agent::tools::app_gateway::invoke_cap(APPS[0], "fixture.probe").unwrap();
    for peer in [&mut owner, &mut foreign] {
        peer.authorize_caps(crate::caps::CapSet::from_caps([invoke.clone()]));
        peer.result(
            Command::AppServiceCliCall,
            json!({"app_id": APPS[0], "tool": "fixture.probe", "arguments": {}}),
        )
        .await;
    }
    let bindings = fixture.services.retained_trust_bindings(None).await.unwrap();
    assert_eq!(bindings.len(), 2);
    assert!(bindings.iter().any(|binding| binding.owner == OWNER));
    assert!(bindings.iter().any(|binding| binding.owner == FOREIGN_UID));
    let session = crate::proc::try_registry_sessions_for_owner(OWNER)
        .unwrap()
        .into_iter()
        .find(|session| session.group.as_deref() == Some("app-mcp"))
        .unwrap();
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, session.pid, 0) };
    assert!(raw >= 0);
    let process = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let package = crate::apps::find_verified(Path::new("/usr/lib/cos/apps"), APPS[0]).unwrap();
    let key_id = package.require_verified().unwrap().audit_facts()["publisher_key_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (arrived, waiting) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    let (completed, denied) = tokio::sync::oneshot::channel();
    *fixture.server.live_check.lock().unwrap() = Some(LiveCheckBarrier { arrived, release: held, completed });
    std::fs::write(owner_app.data().join("go"), b"probe withdrawn live authority").unwrap();
    tokio::time::timeout(Duration::from_secs(10), waiting).await.unwrap().unwrap();
    let policy = owner
        .exchange(
            Command::ProvenanceTrustWithdrawal,
            json!({"private_policy_key_id": key_id}),
        )
        .await;
    assert_eq!(policy["policy"]["policy_withdrawn"], true);
    assert_eq!(
        policy["policy"]["root_retirement"]["root_retirement_confirmed"],
        false
    );
    release.send(()).unwrap();
    // Checked retirement may reject the captured peer before publisher evaluation.
    let (allowed, error) = tokio::time::timeout(Duration::from_secs(10), denied).await.unwrap().unwrap();
    assert!(!allowed && error.is_some(), "Root admitted the exact held call after owner withdrawal");
    assert_eq!(fixture.server.count(), 1, "withdrawn authority must not dispatch APT");
    owner.authorize_caps(crate::caps::CapSet::from_caps([invoke.clone()]));
    assert!(
        !owner
            .call(
                Command::AppServiceCliCall,
                json!({"app_id": APPS[0], "tool": "fixture.probe", "arguments": {}}),
            )
            .await
            .ok
    );
    fixture
        .server
        .lose_mutation_reply
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let lost = owner
        .exchange(
            Command::ProvenanceTrustWithdrawal,
            json!({"action": "reconcile", "target": {"type": "revocation", "key_id": key_id}}),
        )
        .await;
    assert!(lost["transport_error"].is_string());
    let id = fixture
        .server
        .calls
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|call| call.uid == OWNER && call.command == Command::ProvenanceTrustWithdrawal)
        .unwrap()
        .result
        .as_ref()
        .unwrap()["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    let status = owner
        .result(
            Command::ProvenanceTrustWithdrawal,
            json!({"action": "status", "request_id": id}),
        )
        .await;
    assert_eq!(status["state"], "retired", "{status}");
    assert_eq!(status["root_retirement_confirmed"], true);
    assert_eq!(status["non_app_retirement"]["confirmed"], false);
    for app in status["apps"].as_array().unwrap() {
        assert_eq!(app["owner_uid"], OWNER);
        assert_eq!(app["app_id"], APPS[0]);
        assert_eq!(app["retired"], true);
    }
    owner_app.assert_retired();
    assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
    assert!(fixture.services.retained_trust_bindings(Some(OWNER)).await.unwrap().is_empty());
    foreign_app.assert_running();
    foreign.authorize_caps(crate::caps::CapSet::from_caps([invoke]));
    foreign
        .result(
            Command::AppServiceCliCall,
            json!({"app_id": APPS[0], "tool": "fixture.probe", "arguments": {}}),
        )
        .await;
    assert!(
        !foreign
            .call(
                Command::ProvenanceTrustWithdrawal,
                json!({"action": "status", "request_id": id}),
            )
            .await
            .ok
    );
    fixture.assert_installed();
    let fence = Arc::new(
        crate::clawd::app_mutation::fence_owner(APPS[0], FOREIGN_UID).await.unwrap(),
    );
    crate::clawd::app_store::retire_for_owner(&fixture.state, APPS[0], FOREIGN_UID, fence)
        .await
        .unwrap();
    foreign_app.assert_retired();
    owner_app.finish().await;
    foreign_app.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/PID/network namespaces, cgroup v2, ptrace and matching COS_APP_APT_TEST_COS/RUNNER/HOOK"]
async fn private_apt_package_queries_use_ordinary_app_custody_and_exact_read_scopes() {
    use crate::clawd::routes::Command;
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    fixture.enable_processes();
    fixture.install_bundle().await;
    let before = dpkg_state();
    let mut caller = OrdinaryActor::start(OWNER, APPS[0], "catalog");
    caller.ready(&fixture).await;
    let path = caller.data().join("queries.json");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        caller.assert_running();
        assert!(std::time::Instant::now() < deadline, "ordinary read-only package queries did not complete");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let result: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().uid(), OWNER);
    assert_eq!(result["raw_dpkg_visible"], false);
    assert_eq!(result["raw_apt_visible"], false);
    let queries = result["queries"].as_array().unwrap();
    assert_eq!(queries.len(), 4);
    if queries[..3].iter().any(|query| query["code"] != 0) {
        let output = owner_command(OWNER, Path::new("/usr/bin/timeout"))
            .args([
                "20", "/usr/bin/apt-cache",
                "-o", "Dir::Cache::pkgcache=", "-o", "Dir::Cache::srcpkgcache=",
                "search", "--names-only", "--", BUNDLE,
            ])
            .env("HOME", "/").env("LC_ALL", "C").current_dir("/")
            .output().unwrap();
        let metadata = ["/etc/apt/apt.conf.d/00private-fixture",
            "/etc/apt/sources.list.d/fixture.sources", "/var/lib/dpkg/status",
            "/var/lib/apt/lists"].into_iter().map(|path| {
                let info = std::fs::symlink_metadata(path).unwrap();
                json!({"path": path, "uid": info.uid(), "mode": info.mode() & 0o777})
            }).collect::<Vec<_>>();
        eprintln!("private package reader failure evidence {}", json!({
            "status": output.status.to_string(), "inputs": metadata,
            "direct_owner_stdout": String::from_utf8_lossy(&output.stdout[..output.stdout.len().min(8192)]),
            "direct_owner_stderr": String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(8192)]),
            "queries": queries,
        }));
    }
    for query in &queries[..3] {
        assert_eq!(query["code"], 0, "{query}");
        assert_eq!(query["reply"]["wire_version"], 1);
        assert_eq!(query["reply"]["ok"], true, "{query}");
    }
    assert!(queries[0]["reply"]["data"]["results"].as_array().unwrap()
        .iter().any(|entry| entry["name"] == BUNDLE));
    assert_eq!(queries[1]["reply"]["data"]["name"], BUNDLE);
    assert_eq!(queries[1]["reply"]["data"]["found"], true);
    assert_eq!(queries[1]["reply"]["data"]["version"], "1.0.0");
    let installed = queries[2]["reply"]["data"]["packages"].as_array().unwrap();
    for package in [BUNDLE, SUPPORT] {
        assert!(installed.iter().any(|entry| entry == package));
        assert_eq!(queries[2]["reply"]["data"]["versions"][package], "1.0.0");
    }
    assert_ne!(queries[3]["code"], 0);
    assert_eq!(queries[3]["reply"]["code"], "PERMISSION_DENIED");
    {
        let calls = fixture.server.calls.lock().unwrap();
        let requests = calls.iter().filter(|call| {
            call.command == Command::AppSessionRelay
                && call.params["command"] == Command::SystemPackageQuery.as_str()
        }).collect::<Vec<_>>();
        assert_eq!(requests.len(), 4);
        assert!(requests.iter().all(|call| call.uid == OWNER));
        assert!(!calls.iter().any(|call| {
            call.command == Command::AppSessionRelay
                && matches!(call.params["command"].as_str(),
                    Some("system.package.control" | "system.package.install" | "system.package.restore"))
        }));
    }
    assert_eq!(dpkg_state(), before, "metadata queries mutated installed package state");
    let process = caller.process.as_ref().unwrap().try_clone().unwrap();
    let group = caller.group.clone();
    let runtime = caller.socket.parent().unwrap().to_path_buf();
    caller.finish().await;
    assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
    assert!(!group.exists() && !runtime.exists(), "read-only App custody was not retired");
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/PID/network namespaces, cgroup v2, ptrace and matching COS_APP_APT_TEST_COS/RUNNER/HOOK"]
async fn private_apt_ordinary_self_handoff_retains_receipt() {
    use crate::clawd::routes::Command;
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    fixture.enable_processes();
    fixture.install_bundle().await;
    let mut caller = OrdinaryActor::start(OWNER, APPS[0], "handoff");
    let mut other_owner = OrdinaryActor::start(FOREIGN_UID, APPS[1], "hold");
    caller.ready(&fixture).await;
    other_owner.ready(&fixture).await;
    let mut peer = RetainedPeer::start(OWNER);

    // A real no-effect request must not retire either authenticated App owner.
    let no_effect = peer.accept("install", BUNDLE).await;
    let unchanged = peer.wait_phase(&no_effect, "applied").await;
    assert!(unchanged["plan"]["actions"].as_array().unwrap().is_empty());
    assert!(unchanged["checked_retired_apps"]
        .as_array()
        .unwrap()
        .is_empty());
    caller.assert_running();
    other_owner.assert_running();
    assert_eq!(fixture.server.count(), 1);

    fixture
        .server
        .lose_mutation_reply
        .store(true, std::sync::atomic::Ordering::SeqCst);
    std::fs::write(
        caller.data().join("go"),
        b"remove own signed Debian package",
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let accepted = loop {
        let call = fixture
            .server
            .calls
            .lock()
            .unwrap()
            .iter()
            .find(|call| {
                call.uid == OWNER
                    && call.command == Command::AppSessionRelay
                    && call.params["command"] == Command::SystemPackageControl.as_str()
                    && call.params["params"]["action"] == "remove"
                    && call.result.is_some()
            })
            .cloned();
        if let Some(call) = call {
            break call;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ordinary caller never handed off its real provider; child reply: {:?}; calls: {:?}",
            std::fs::read_to_string(caller.data().join("reply.json")),
            fixture.server.calls.lock().unwrap().iter().map(|call| (
                call.uid, call.command.as_str(), call.result.is_some(), &call.error,
            )).collect::<Vec<_>>(),
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let receipt = accepted.result.unwrap()["result"].clone();
    assert_eq!(receipt["status"], "accepted");
    assert_eq!(receipt["accepted"], true);
    assert_eq!(receipt["completion"], "query_required");
    let id = receipt["request_id"].as_str().unwrap();
    let applied = peer.wait_phase(id, "applied").await;
    assert_eq!(applied["completion"]["confirmed"], true);
    assert_eq!(applied["session"], accepted.params["session_id"]);
    assert_eq!(applied["app_id"], APPS[0]);
    assert_eq!(applied["impact"]["scope"], "affected_apps");
    assert_eq!(applied["affected_apps"], json!(APPS));
    assert_eq!(applied["checked_retired_apps"], json!(APPS));
    assert!(applied["global_stop_review_id"].is_null());
    assert_children(id, 1, true);
    caller.assert_retired();
    other_owner.assert_retired();
    for app in APPS {
        assert!(!Path::new("/usr/lib/cos/apps").join(app).exists());
        assert!(crate::clawd::app_mutation::admit(app).is_ok());
    }
    assert_eq!(std::fs::read(SUPPORT_PATH).unwrap(), SUPPORT_BYTES);
    assert!(
        !crate::clawd::packages::query_package_state(BUNDLE)
            .await
            .unwrap()
            .installed
    );
    let by_invocation = peer
        .result(
            Command::SystemPackageStatus,
            json!({
                "invocation_id": accepted.params["params"]["invocation_id"],
            }),
        )
        .await;
    assert_eq!(by_invocation["transactions"].as_array().unwrap().len(), 1);
    assert_eq!(by_invocation["transactions"][0]["request_id"], id);
    let installed = dpkg_state();
    let recovered = peer
        .result(Command::SystemPackageRecover, json!({"request_id": id}))
        .await;
    assert_eq!(recovered["phase"], "applied");
    assert_eq!(dpkg_state(), installed);
    assert_eq!(fixture.server.count(), 2, "lost caller reply replayed APT");
    let mut foreign = RetainedPeer::start(FOREIGN_UID);
    assert!(
        !foreign
            .call(Command::SystemPackageStatus, json!({"request_id": id}))
            .await
            .ok
    );
    drop(foreign);
    caller.finish().await;
    other_owner.finish().await;
    persistent_mcp_self_handoff(&fixture, &mut peer).await;
    drop(peer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/PID/network namespaces and matching installed Web/App Host binaries"]
async fn private_installed_product_web_calls_real_kv_without_model() {
    use crate::clawd::routes::Command;
    let _lock = crate::test_env::lock_env();
    // Private mounts can hide the runner's real runtime path.
    let node = std::fs::File::open(
        std::env::var_os("COS_APP_APT_TEST_NODE").expect("driver-pinned real Node runtime"),
    ).expect("open real browser fixture runtime before private mounts");
    let fixture = Fixture::new();
    fixture.enable_processes();
    copy_executable(
        &PathBuf::from(format!("/proc/self/fd/{}", node.as_raw_fd())),
        Path::new("/usr/local/bin/node"),
    );
    drop(node);
    let installed = stage_real_web_client();
    let (web, client, token) = WebOwner::start().await;
    let mut web = Some(web);
    let response = client.get(format!("{WEB_ORIGIN}/api/apps"))
        .bearer_auth(&token).send().await.unwrap();
    let status = response.status();
    let inventory = response.json::<Value>().await.unwrap();
    assert!(status.is_success(), "installed inventory: {status}: {inventory}");
    assert!(inventory["apps"].as_array().unwrap().iter()
        .any(|app| app["id"] == "kv" && app["web_ready"] == true));
    let opened = web_post(&client, &token, "/api/apps/kv/open", json!({})).await;
    assert_eq!(opened["status"], "ready");
    let view = opened["view_id"].as_str().unwrap();
    for asset in ["index.html", "app.js", "shared/client.js", "shared/ui.js", "shared/ui.css"] {
        let response = client.get(format!("{WEB_ORIGIN}/app-ui/{view}/{asset}"))
            .send().await.unwrap().error_for_status().unwrap();
        assert!(response.headers()["content-security-policy"].to_str().unwrap()
            .contains("connect-src 'none'"));
        assert_eq!(response.bytes().await.unwrap().as_ref(),
            std::fs::read(installed.join("web").join(asset)).unwrap());
    }
    let call_count = || fixture.server.calls.lock().unwrap().iter()
        .filter(|call| call.command == Command::AppServiceCliCall).count();
    let before = call_count();
    for (app, tool) in [(APPS[0], "kv.get"), ("kv", "agent.task.submit")] {
        let response = client.post(format!("{WEB_ORIGIN}/api/apps/{app}/call"))
            .header("Origin", WEB_ORIGIN).bearer_auth(&token)
            .json(&json!({"view_id": view, "tool": tool, "arguments": {"key": "web-proof"}}))
            .send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    assert_eq!(call_count(), before, "a view escaped its owning App/tool boundary");
    let value = "persistent signed KV over authenticated Web, without an AI request";
    let set = web_post(&client, &token, "/api/apps/kv/call",
        json!({"view_id": view, "tool": "kv.set",
            "arguments": {"key": "web-proof", "value": value}})).await;
    assert_ne!(set["result"]["isError"], true, "{set}");
    let before_browser = call_count();
    installed_product_browser(&token, value);
    assert!(call_count() >= before_browser + 3, "real UI button/form/get never reached the Root MCP route");
    let browser_session = crate::proc::try_registry_sessions_for_owner(OWNER).unwrap()
        .into_iter().find(|session| session.app_id.as_deref() == Some("kv")
            && session.group.as_deref() == Some("app-mcp")).unwrap();
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, browser_session.pid, 0) };
    assert!(raw >= 0);
    let browser_process = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    assert!(!claw_display_control::identity::pidfd_exited(browser_process.as_fd()).unwrap(),
        "Root-owned App MCP died with its browser UI");
    let mut previous_session = None;
    for pass in 0..2 {
        let get = web_post(&client, &token, "/api/apps/kv/call",
            json!({"view_id": view, "tool": "kv.get", "arguments": {"key": "web-proof"}})).await;
        assert_eq!(get["result"]["content"][0]["text"], value);
        let session = crate::proc::try_registry_sessions_for_owner(OWNER).unwrap()
            .into_iter().find(|session| session.app_id.as_deref() == Some("kv")
                && session.group.as_deref() == Some("app-mcp")).unwrap();
        assert!(!session.pending_bind);
        if pass == 0 { assert_eq!(session.session_id, browser_session.session_id); }
        assert_ne!(previous_session.as_deref(), Some(session.session_id.as_str()));
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, session.pid, 0) };
        assert!(raw >= 0);
        let process = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        assert!(!claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
        if pass == 1 {
            web_post(&client, &token, &format!("/api/app-views/{view}/close"), json!({})).await;
            assert_eq!(client.get(format!("{WEB_ORIGIN}/app-ui/{view}/index.html"))
                .send().await.unwrap().status(), reqwest::StatusCode::NOT_FOUND);
            drop(web.take());
            assert!(!claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap(),
                "Root-owned App MCP died with cos agent serve");
            let mut cli = RetainedPeer::start(OWNER);
            cli.authorize_caps(crate::caps::CapSet::from_caps([
                crate::agent::tools::app_gateway::invoke_cap("kv", "kv.get").unwrap(),
                crate::caps::Cap::new(crate::caps::Verb::DATA_KV_READ,
                    crate::caps::Scope::name("web-proof")),
            ]));
            let read = cli.result(Command::AppServiceCliCall,
                json!({"app_id": "kv", "tool": "kv.get", "arguments": {"key": "web-proof"}})).await;
            assert_eq!(read["content"][0]["text"], value);
            assert!(crate::proc::try_registry_sessions_for_owner(OWNER).unwrap().iter()
                .any(|current| current.session_id == session.session_id && current.pid == session.pid),
                "CLI had to replace the App service after its Web frontend stopped");
            drop(cli);
        }
        fixture.services.retire_app_for_owner("kv", Some(OWNER),
            std::time::Instant::now() + Duration::from_secs(15)).await.unwrap();
        assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
        assert!(fixture.services.retained_trust_bindings(Some(OWNER)).await.unwrap().is_empty());
        previous_session = Some(session.session_id);
    }
    assert!(claw_display_control::identity::pidfd_exited(browser_process.as_fd()).unwrap());
    assert!(fixture.server.calls.lock().unwrap().iter()
        .all(|call| call.command != Command::TaskSubmit));
    drop(web);
    eprintln!("private installed KV Web: actual browser, UI/Serve-independent MCP and checked service retirement passed; draining read-only frontend polls");
    fixture.finish().await;
    eprintln!("private installed KV Web: real browser clicks, signed resources, UI/Serve-independent MCP persistence and checked retirement without a model passed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/PID/network namespaces, cgroup v2, ptrace and matching COS_APP_APT_TEST_COS/RUNNER/HOOK"]
async fn private_apt_global_stop_is_separate_and_unreviewed_effect_is_not_applied() {
    use crate::clawd::routes::Command;
    use clawd_client::system_review::{ReviewAction, ReviewKind, ReviewStatus};
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    fixture.enable_processes();
    fixture.install_bundle().await;
    let mut alpha = OrdinaryActor::start(OWNER, APPS[0], "hold");
    let mut beta = OrdinaryActor::start(FOREIGN_UID, APPS[1], "hold");
    alpha.ready(&fixture).await;
    beta.ready(&fixture).await;
    let mut peer = RetainedPeer::start(OWNER);
    peer.authorize(SIDE_EFFECT);
    let params = json!({
        "session": peer.session, "action": "install", "package": SIDE_EFFECT,
        "invocation_id": uuid::Uuid::new_v4().simple().to_string(),
    });
    let receipt = peer
        .result(Command::SystemPackageControl, params.clone())
        .await;
    let id = receipt["request_id"].as_str().unwrap();
    assert!(
        !peer
            .call(Command::SystemPackageControl, params.clone())
            .await
            .ok,
        "exhausted authority was accepted for another effect request"
    );
    let retained = peer
        .result(
            Command::SystemPackageStatus,
            json!({
                "invocation_id": params["invocation_id"],
            }),
        )
        .await;
    assert_eq!(retained["transactions"].as_array().unwrap().len(), 1);
    assert_eq!(retained["transactions"][0]["request_id"], id);
    let mut different = params.clone();
    different["invocation_id"] = json!(uuid::Uuid::new_v4().simple().to_string());
    assert!(
        !peer.call(Command::SystemPackageControl, different).await.ok,
        "acceptance did not consume the exact one-use package authority"
    );
    let pending = peer.wait_phase(id, "awaiting_impact_confirmation").await;
    let review_id = pending["global_stop_review_id"].as_str().unwrap();
    let view = global_view(&mut peer, review_id).await;
    assert_eq!(view.kind, ReviewKind::PackageImpact);
    assert_eq!(view.status, ReviewStatus::Pending);
    assert!(pending["app_reviews"].as_array().unwrap().is_empty());
    assert_eq!(pending["impact"]["known_affected_apps"], json!([]));
    assert_eq!(pending["impact"]["all_apps"], json!(APPS));
    assert!(pending["impact"]["uncertainties"].as_array().unwrap().len() >= 2);
    let before = dpkg_state();
    assert!(
        !decide_global(OWNER, &view, ReviewAction::ConfirmInstall)
            .await
            .ok
    );
    assert!(
        !decide_global(FOREIGN_UID, &view, ReviewAction::ConfirmGlobalAppStop)
            .await
            .ok
    );
    let mut stale = view.clone();
    stale.revision += 1;
    assert!(
        !decide_global(OWNER, &stale, ReviewAction::ConfirmGlobalAppStop)
            .await
            .ok
    );
    assert!(
        !peer
            .call(
                Command::SystemReviewDecide,
                json!({
                    "owner_uid": OWNER, "review": {
                        "id": view.id, "revision": view.revision,
                        "action": ReviewAction::ConfirmGlobalAppStop, "choices": [],
                    },
                })
            )
            .await
            .ok
    );
    peer.result(
        Command::SystemReviewCancel,
        json!({"review": {
            "id": view.id, "revision": view.revision, "action": ReviewAction::Cancel, "choices": [],
        }}),
    )
    .await;
    peer.wait_phase(id, "not_applied").await;
    alpha.assert_running();
    beta.assert_running();
    assert_eq!(dpkg_state(), before);
    assert_eq!(
        fixture.server.count(),
        1,
        "refused global stop still dispatched APT"
    );
    for app in APPS {
        assert!(crate::clawd::app_mutation::admit(app).is_ok());
    }

    let id = peer.accept("install", SIDE_EFFECT).await;
    let pending = peer.wait_phase(&id, "awaiting_impact_confirmation").await;
    let first_id = pending["global_stop_review_id"]
        .as_str()
        .unwrap()
        .to_string();
    let first = global_view(&mut peer, &first_id).await;
    assert!(
        !decide_global(OWNER, &view, ReviewAction::ConfirmGlobalAppStop)
            .await
            .ok,
        "a denied transaction's review approved a later request"
    );
    // Change real installed App impact while the first exact confirmation waits.
    // This pre-existing quarantine is not approved by either stop confirmation.
    let extra = Path::new("/usr/lib/cos/apps/apt-preexisting-quarantine");
    std::fs::create_dir(extra).unwrap();
    std::fs::write(
        extra.join("app.json"),
        json!({
            "schema_version": 2, "id": "apt-preexisting-quarantine",
            "version": "1.0.0", "runtime": "python",
            "name": {"en": "Private quarantine"}, "summary": {"en": "Impact change"},
            "operations": {"probe": {"label": {"en": "Probe"}, "needs": [], "args": []}},
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        extra.join("main.py"),
        "def run(command, args):\n    return {\"fixture\": True}\n",
    )
    .unwrap();
    std::fs::copy(
        Path::new("/usr/lib/cos/apps").join(APPS[0]).join(".provenance.json"),
        extra.join(".provenance.json"),
    )
    .unwrap();
    assert!(crate::apps::discover_all(Path::new("/usr/lib/cos/apps"))
        .quarantined
        .contains_key("apt-preexisting-quarantine"));
    assert!(
        decide_global(OWNER, &first, ReviewAction::ConfirmGlobalAppStop)
            .await
            .ok
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let refreshed = loop {
        let status = peer.status(&id).await;
        if status["phase"] == "awaiting_impact_confirmation"
            && status["global_stop_review_id"]
                .as_str()
                .is_some_and(|id| id != first_id)
        {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "changed impact reused old approval: {status}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        global_view(&mut peer, &first_id).await.status,
        ReviewStatus::Stale
    );
    assert!(
        !decide_global(OWNER, &first, ReviewAction::ConfirmGlobalAppStop)
            .await
            .ok
    );
    alpha.assert_running();
    beta.assert_running();
    assert_eq!(fixture.server.count(), 1);
    assert_eq!(dpkg_state(), before);
    let current_id = refreshed["global_stop_review_id"].as_str().unwrap();
    let current = global_view(&mut peer, current_id).await;
    assert_ne!(current.contract_digest, first.contract_digest);
    assert!(refreshed["impact"]["all_apps"]
        .as_array()
        .unwrap()
        .contains(&json!("apt-preexisting-quarantine")));
    assert!(
        decide_global(OWNER, &current, ReviewAction::ConfirmGlobalAppStop)
            .await
            .ok
    );
    let failed = peer.wait_phase(&id, "indeterminate").await;
    assert!(
        failed["completion"].is_null(),
        "all-App stop consent became App delivery consent"
    );
    assert!(
        failed["error"].as_str().unwrap().contains("quarantine"),
        "{failed}"
    );
    assert_children(&id, 1, true);
    let record = retained_record(&id);
    assert!(record["plan"]["removed_apps"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(record["plan"]["delivery"]["hook_consumed"], true);
    assert!(
        crate::clawd::packages::query_package_state(SIDE_EFFECT)
            .await
            .unwrap()
            .installed
    );
    assert!(Path::new("/usr/lib/cos/apps/apt-unreviewed/app.json").exists());
    assert!(crate::apps::discover_all(Path::new("/usr/lib/cos/apps"))
        .quarantined
        .contains_key("apt-unreviewed"));
    alpha.assert_retired();
    beta.assert_retired();
    assert_eq!(
        global_view(&mut peer, current_id).await.status,
        ReviewStatus::Consumed
    );
    assert!(
        !decide_global(OWNER, &current, ReviewAction::ConfirmGlobalAppStop)
            .await
            .ok
    );
    assert!(crate::clawd::app_mutation::admit("otherwise-unaffected-app").is_err());
    let installed = dpkg_state();
    refused_recovery(&mut peer, &id).await;
    assert_eq!(peer.status(&id).await["phase"], "indeterminate");
    assert_eq!(dpkg_state(), installed);
    assert_eq!(
        fixture.server.count(),
        2,
        "outcome checking reran a successful maintainer effect"
    );
    alpha.finish().await;
    beta.finish().await;
    drop(peer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/PID/network namespaces and matching COS_APP_APT_TEST_COS/RUNNER/HOOK"]
async fn private_apt_retained_payload_lifecycle_and_nonzero_completion() {
    use crate::clawd::routes::Command;
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    fixture.enable_processes();
    let mut peer = RetainedPeer::start(OWNER);
    let install = peer.accept("install", SUPPORT).await;
    assert_eq!(
        peer.wait_phase(&install, "applied").await["completion"]["confirmed"],
        true
    );
    assert_eq!(std::fs::read(SUPPORT_PATH).unwrap(), SUPPORT_BYTES);
    assert_children(&install, 1, true);
    let remove = peer.accept("remove", SUPPORT).await;
    let removed = peer.wait_phase(&remove, "applied").await;
    assert!(!Path::new(SUPPORT_PATH).exists());
    assert_eq!(removed["rollback"]["available"], true, "{removed}");
    let seq = removed["rollback"]["mutation_seq"].as_u64().unwrap();
    peer.authorize(SUPPORT);
    let invocation =
        crate::crypto::sha256_hex(format!("package-inverse:{}:{seq}", peer.parent).as_bytes());
    let receipt = peer
        .result(
            Command::SystemPackageRestore,
            json!({
                "session": peer.session, "package": SUPPORT,
                "previous_version": "1.0.0", "was_held": false,
                "mutation_session": peer.parent, "mutation_seq": seq,
                "invocation_id": &invocation[..32],
            }),
        )
        .await;
    assert_eq!(receipt["status"], "accepted");
    let restore = receipt["request_id"].as_str().unwrap();
    let restored = peer.wait_phase(restore, "applied").await;
    assert_eq!(restored["completion"]["confirmed"], true);
    assert_eq!(std::fs::read(SUPPORT_PATH).unwrap(), SUPPORT_BYTES);
    assert_children(restore, 2, true);
    let again = peer.accept("remove", SUPPORT).await;
    peer.wait_phase(&again, "applied").await;
    assert!(!Path::new(SUPPORT_PATH).exists());

    std::fs::write(
        "/etc/apt/apt.conf.d/99fixture-post-dpkg",
        "DPkg::Post-Invoke { \"/usr/bin/false\"; };\n",
    )
    .unwrap();
    let failed = peer.accept("install", SUPPORT).await;
    let status = peer.wait_phase(&failed, "indeterminate").await;
    assert!(status["completion"].is_null());
    assert_children(&failed, 1, false);
    assert_eq!(
        retained_record(&failed)["plan"]["delivery"]["hook_consumed"],
        true
    );
    let installed = crate::clawd::packages::query_package_state(SUPPORT)
        .await
        .unwrap();
    assert!(installed.installed);
    assert_eq!(installed.version.as_deref(), Some("1.0.0"));
    assert_eq!(std::fs::read(SUPPORT_PATH).unwrap(), SUPPORT_BYTES);
    assert!(checked(std::process::Command::new("/usr/bin/dpkg").arg("--audit")).is_empty());
    let before = dpkg_state();
    let dispatched = fixture.server.count();
    for _ in 0..2 {
        let response = refused_recovery(&mut peer, &failed).await;
        assert!(
            !response.ok,
            "installed versions hid a normal nonzero APT exit"
        );
        let error = response.error.unwrap();
        assert_eq!(error.code, "indeterminate", "{error:?}");
        assert_eq!(peer.status(&failed).await["phase"], "indeterminate");
        assert_eq!(dpkg_state(), before);
        assert_eq!(
            fixture.server.count(),
            dispatched,
            "read-only recovery replayed APT"
        );
    }
    drop(peer);
    fixture.finish().await;
}
