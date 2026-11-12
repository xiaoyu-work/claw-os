// Real common-Host GUI + retained package controller acceptance, isolated CI only.
// The GUI is a signed probe, not the native Store ELF. Root supplies synthetic
// protected decisions through the real review route; no live polkit is invoked.

use super::*;
use clawd_client::system_review::{
    PermissionChoice, PermissionSelection, ReviewAction, ReviewDecision, ReviewKind,
    ReviewStatus, SystemReview,
};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

const BASE: &str = "/run/cos-app-apt-fixture";
const APP: &str = "gui-boundary-test";
const DENIED_APP: &str = "gui-package-denied";
const SUPPORT: &str = "claw-app-acceptance-support";
const EFFECT: &str = "claw-package-acceptance-side-effect";
const UNUSED: &str = "claw-native-package-unused";
const PENDING: &str = "claw-native-package-pending";
const DATA: &str = "/run/display-home/.local/share/cos/apps/gui-boundary-test";
const PEER: &str = "clawd::server::gui_fixture::package::private_native_package_owner";
const PLUMBING: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/clawd/app_store/repository.py");

fn plumbing(action: &str) {
    let mut command = ProcessCommand::new("/usr/bin/python3");
    command.args(["-I", "-B", PLUMBING, action]).env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    namespaces(&mut command);
    let output = command.output().unwrap();
    assert!(output.status.success(), "private repository {action}: {}",
        String::from_utf8_lossy(&output.stderr));
}

fn namespaces(command: &mut ProcessCommand) {
    for kind in ["MNT", "PID", "NET"] {
        let name = format!("COS_APP_APT_PARENT_{kind}");
        command.env(&name, std::env::var(&name).expect("isolated native-package lane"));
    }
}

pub(super) struct Repository(Child);

pub(super) fn require_namespaces() {
    assert_eq!(unsafe { libc::getuid() }, 0);
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert!(std::process::id() > 1, "PID 1 must supervise, not own Root package children");
    for kind in ["MNT", "PID", "NET"] {
        let parent = std::env::var(format!("COS_APP_APT_PARENT_{kind}")).unwrap();
        assert_ne!(std::fs::read_link(format!("/proc/self/ns/{}", kind.to_lowercase()))
            .unwrap().to_str().unwrap(), parent);
    }
}

impl Repository {
    pub(super) fn start() -> Self {
        require_namespaces();
        crate::extension_host::identity::ExtensionIdentityPool::load(
            crate::extension_host::identity::GROUP_GID,
        ).expect("actual isolated reserved NSS identities");
        let support = Path::new(BASE).join("payloads/support/usr/lib/cos/python");
        std::fs::create_dir_all(&support).unwrap();
        std::fs::write(support.join("native_package_support.py"), b"VALUE = 'real separately dpkg-owned support'\n").unwrap();
        plumbing("repository-native-package");
        let log = std::fs::File::create(Path::new(BASE).join("native-package-http.log")).unwrap();
        let mut command = ProcessCommand::new("/usr/bin/python3");
        command.args(["-I", "-B", PLUMBING, "serve"]).env_clear()
            .env("PATH", "/usr/bin:/bin").stdin(Stdio::null())
            .stdout(Stdio::null()).stderr(log);
        namespaces(&mut command);
        let parent = std::process::id() as libc::pid_t;
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != parent
                { return Err(std::io::Error::last_os_error()); }
                Ok(())
            });
        }
        let mut server = Self(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(server.0.try_wait().unwrap().is_none(), "signed private HTTP server exited");
            if let Ok(mut stream) = std::net::TcpStream::connect_timeout(
                &"127.0.0.1:38081".parse().unwrap(), Duration::from_millis(100),
            ) {
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                stream.write_all(b"GET /dists/stable/InRelease HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
                let mut bytes = Vec::new();
                stream.take(16384).read_to_end(&mut bytes).unwrap();
                assert!(bytes.starts_with(b"HTTP/1.0 200"));
                assert!(String::from_utf8_lossy(&bytes).contains("BEGIN PGP SIGNED MESSAGE"));
                break;
            }
            assert!(Instant::now() < deadline, "signed private repository never became responsive");
            std::thread::sleep(Duration::from_millis(10));
        }
        plumbing("update");
        assert!(Path::new("/usr/lib/cos/bin/claw-app-apt-hook").is_file());
        assert!(Path::new("/usr/lib/cos/app-apt-metadata.py").is_file());
        server
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() { self.0.kill().unwrap(); }
        self.0.wait().unwrap();
    }
}

pub(super) struct Custody {
    _services: Arc<crate::clawd::app_services::AppServiceManager>,
    _environment: crate::test_env::TestEnvVarGuard,
}

pub(super) fn custody(state: &DaemonState, admission: &Arc<Admission>) -> Custody {
    let root = std::fs::read_to_string("/run/gui-fixture/cgroup").unwrap();
    assert!(root.trim().starts_with("/sys/fs/cgroup/claw-display-fixture."));
    std::fs::write(Path::new(root.trim()).join("cgroup.subtree_control"), b"+cpu +memory +pids").unwrap();
    let hosts = Path::new(root.trim()).join("package-hosts");
    std::fs::create_dir(&hosts).unwrap();
    std::fs::write(hosts.join("cgroup.subtree_control"), b"+cpu +memory +pids").unwrap();
    let environment = crate::test_env::TestEnvVarGuard::set(
        "CLAWD_EXTENSION_CGROUP_ROOT", hosts,
    );
    let broker = crate::agentd::supervisor::BrokerContext::new(
        state.clone(), admission.clone(), "/run/cos/clawd.sock".into(),
    ).unwrap();
    crate::clawd::app_execution::initialize(&broker);
    let services = crate::clawd::app_services::AppServiceManager::new(broker);
    state.install_app_service_manager(&services).unwrap();
    Custody { _services: services, _environment: environment }
}

pub(super) fn install_denied_app(key: &crate::provenance::sign::SigningKeyFile) {
    use crate::provenance::{PackageKind, sign};
    let directory = Path::new("/usr/lib/cos/apps").join(DENIED_APP);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    let body = r#"
import json, os, subprocess, sys

def refused():
    result = subprocess.run(
        ["/usr/local/bin/cos", "--wire=1", "__package_intent", "--retained",
         "review", "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "install",
         "claw-package-acceptance-side-effect"],
        stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=15,
    )
    assert result.returncode != 0, "ordinary/MCP App reached the GUI-only intent endpoint"
    return {"intent_refused": True, "app": os.environ["COS_APP_ID"],
            "session": os.environ["COS_SESSION"], "mcp": os.environ.get("COS_MCP_SERVER") == "1"}

def run(command, args):
    assert command == "probe" and not args
    return refused()

if __name__ == "__main__":
    for line in sys.stdin:
        message = json.loads(line)
        if "id" not in message:
            continue
        method = message["method"]
        if method == "initialize":
            result = {"protocolVersion": message["params"]["protocolVersion"],
                      "capabilities": {"tools": {}},
                      "serverInfo": {"name": "private-package-denial", "version": "1"}}
        elif method == "tools/list":
            result = {"tools": [{"name": "fixture.package-intent", "description": "Refuse GUI-only intent",
                      "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}}]}
        elif method == "tools/call":
            context = message["params"]["_meta"]["claw-os.dev/call-context"]
            assert context["caller"]["kind"] == "cli"
            result = {"content": [{"type": "text", "text": json.dumps(refused())}]}
        else:
            assert method == "ping"
            result = {}
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
"#;
    std::fs::write(directory.join("main.py"), body).unwrap();
    std::fs::write(directory.join("app.json"), json!({
        "id": DENIED_APP, "schema_version": 2, "version": "1.0.0", "runtime": "python",
        "name": {"en": "Private package route refusal"},
        "operations": {"probe": {"label": {"en": "Reject ordinary GUI intent"}, "args": [], "needs": []}},
        "mcp": {"entry": "main.py", "transport": "stdio", "lifecycle": "lazy",
            "access": {"system_agent": true},
            "tools": [{"name": "fixture.package-intent", "summary": {"en": "Reject MCP GUI intent"},
                "args": [], "needs": []}]},
    }).to_string()).unwrap();
    for name in ["main.py", "app.json"] {
        std::fs::set_permissions(directory.join(name), std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        assert!(std::fs::File::open(directory.join("app.json")).is_ok(),
            "the normal-UID denial actor must be able to discover its real installed App");
    }
    sign::sign_directory(&directory, &sign::SignRequest {
        kind: PackageKind::App, id: DENIED_APP.into(), version: "1.0.0".into(),
        manifest_schema: "integration".into(), manifest_path: "app.json".into(),
        entrypoints: vec!["main.py".into()], resources: Vec::new(),
    }, key).unwrap();
    std::fs::set_permissions(directory.join(".provenance.json"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let verified = crate::provenance::verify::verify_package(
        &directory, &crate::provenance::VerifyOptions::new(PackageKind::App),
        &crate::provenance::trust_store(),
    ).unwrap();
    let review = crate::approvals::system_review::submit(
        OWNER, crate::approvals::system_review::ReviewKind::AppActivation, &verified,
        "private Root package fixture".into(),
    ).unwrap();
    crate::approvals::system_review::decide(OWNER, &review.id, true, Some(&verified)).unwrap();
}

#[test]
#[ignore = "normal-UID subprocess of isolated native package GUI acceptance only"]
fn private_native_package_owner() {
    assert_eq!(unsafe { libc::getuid() }, OWNER);
    assert_eq!(unsafe { libc::geteuid() }, OWNER);
    assert_eq!(std::fs::read("/run/gui-public/isolated").unwrap(), b"private-v1");
    assert_eq!(std::env::var("COS_NATIVE_PACKAGE_PEER").as_deref(), Ok("1"));
    unsafe {
        std::env::set_var("COS_SESSION", format!("native-package-owner-{}", std::process::id()));
    }
    println!("{}", json!({"native_package_peer": true, "ready": true}));
    std::io::stdout().flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        let argv: Vec<String> = serde_json::from_str(&line.unwrap()).unwrap();
        if argv == ["stop"] { break; }
        if argv == ["binding"] {
            let session = crate::proc::current_session_id().unwrap();
            let registry = crate::proc::registry_path_for_caps();
            let state: Value = serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
            let row = state["sessions"].as_array().unwrap().iter()
                .find(|row| row["session_id"] == session).unwrap();
            let info: crate::proc::SessionInfo = serde_json::from_value(row.clone()).unwrap();
            assert_eq!(info.pid, std::process::id());
            assert_eq!(info.start_time_ticks, crate::proc::read_start_time_ticks_pub(info.pid));
            assert!(info.app_id.is_none());
            assert!(!info.pending_bind);
            assert_eq!(info.caps.as_ref().unwrap(), &owner_invoke_caps());
            crate::caps::enforcement::require_current_session_identity(&session, info.pid).unwrap();
            let liveness = crate::provenance::runtime::assert_live_now(OWNER, &session);
            println!("{}", json!({
                "native_package_peer": true, "ok": true, "result": {
                    "pid": info.pid, "uid": unsafe { libc::geteuid() },
                    "session": session, "registry": registry, "caps": info.caps,
                    "runtime_record": crate::provenance::runtime::state_path_for(OWNER),
                    "local_liveness_error": liveness.err(),
                },
            }));
            std::io::stdout().flush().unwrap();
            continue;
        }
        assert!(matches!(argv.first().map(String::as_str),
            Some("package-transactions" | "review" | "app" | "__package_intent")));
        let result = if argv == ["app", DENIED_APP, "fixture.package-intent", "--args-stdin"] {
            claw_os_sdk::cos_call_json_with_stdin("native-package-fixture", "owner", argv, b"{}")
        } else {
            claw_os_sdk::cos_call_json_with_binary(
                "/usr/local/bin/cos", "native-package-fixture", "owner", argv,
            )
        };
        println!("{}", match result {
            Ok(value) => json!({"native_package_peer": true, "ok": true, "result": value}),
            Err(error) => json!({"native_package_peer": true, "ok": false, "error": error.to_string()}),
        });
        std::io::stdout().flush().unwrap();
    }
}

fn owner_invoke_caps() -> CapSet {
    CapSet::from_caps([
        Cap::new(Verb::AGENT_INVOKE, Scope::name(DENIED_APP)),
        crate::agent::tools::app_gateway::invoke_cap(DENIED_APP, "fixture.package-intent").unwrap(),
    ])
}

struct OwnerPeer {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    session: String,
}

impl OwnerPeer {
    async fn start() -> Self {
        use crate::clawd::authority::{
            authority, Audience, AudienceSet, Binding, Issuance, Issuer, Principal, Subject, Uses,
        };
        use tokio::io::AsyncBufReadExt;
        let mut command = ProcessCommand::new("/usr/local/bin/claw-gui-broker-fixture");
        command.args(["--exact", PEER, "--ignored", "--nocapture", "--test-threads=1"])
            .env_clear().env("HOME", "/run/display-home")
            .env("PATH", "/usr/local/bin:/usr/bin:/bin").env("COS_NATIVE_PACKAGE_PEER", "1")
            .env("COS_PROC_DATA_DIR", "/run/cos/caps/62050").env("CLAWD_SOCKET", "/run/cos/clawd.sock")
            .env("CLAW_COS_BIN", "/usr/local/bin/cos")
            .current_dir("/run/display-home").stdin(Stdio::piped())
            .stdout(Stdio::piped()).stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(|| {
                if libc::setgroups(0, std::ptr::null()) != 0 || libc::setgid(OWNER) != 0 || libc::setuid(OWNER) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let mut peer = Self { child, input, output, session: String::new() };
        assert_eq!(peer.message().await["ready"], true);
        let pid = peer.child.id().unwrap();
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        assert!(raw >= 0);
        let descriptor = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        let process = ProcessIdentity::from_pidfd(descriptor.as_fd()).unwrap();
        peer.session = format!("native-package-owner-{pid}");
        let caps = owner_invoke_caps();
        let info = crate::proc::SessionInfo {
            session_id: peer.session.clone(), pid,
            command: vec!["/usr/local/bin/claw-gui-broker-fixture".into(), PEER.into()],
            started_at: chrono::Utc::now().to_rfc3339(), stdout_path: String::new(), stderr_path: String::new(),
            group: Some("user".into()), parent: None, workdir: None, exit_code: None, ended_at: None,
            tier: Some(2), scope: None, priority: None,
            caps: Some(caps.clone()),
            transient_caps: None, role: None, app_id: None, pending_bind: false,
            start_time_ticks: Some(process.start_ticks()),
            client: crate::session::SessionClient::new(crate::session::SessionSource::LocalCli, true, true),
        };
        crate::paths::RoutedPathContext::for_owner(OWNER, PathBuf::from("/run/display-home"))
            .scope_sync(|| crate::proc::register_session_for_owner(info, OWNER)).unwrap();
        authority().issue(Issuance {
            issuer: Issuer::TrustedSession,
            principal: Principal::of_process(OWNER, pid).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&peer.session),
            audience: AudienceSet::one(Audience::SystemService),
            caps: caps.clone(),
            lifetime: Duration::from_secs(120),
            uses: Uses::Budget(2),
            index_session: true,
        }).unwrap();
        let binding = peer.result(&["binding"]).await;
        assert_eq!(binding["pid"], pid);
        assert_eq!(binding["uid"], OWNER);
        assert_eq!(binding["session"], peer.session);
        assert_eq!(serde_json::from_value::<CapSet>(binding["caps"].clone()).unwrap(), caps);
        eprintln!("native package real owner CLI binding: {binding}");
        peer
    }

    async fn message(&mut self) -> Value {
        loop {
            let line = tokio::time::timeout(Duration::from_secs(20), self.output.next_line())
                .await.expect("normal-UID package frontend timed out").unwrap()
                .expect("normal-UID package frontend exited");
            let Some(start) = line.find('{') else { continue; };
            if let Ok(value) = serde_json::from_str::<Value>(&line[start..]) {
                if value["native_package_peer"] == true { return value; }
            }
        }
    }

    async fn call(&mut self, arguments: &[&str]) -> Value {
        use tokio::io::AsyncWriteExt;
        self.input.write_all(format!("{}\n", json!(arguments)).as_bytes()).await.unwrap();
        self.input.flush().await.unwrap();
        self.message().await
    }

    async fn result(&mut self, arguments: &[&str]) -> Value {
        let response = self.call(arguments).await;
        assert_eq!(response["ok"], true, "{response}");
        response["result"].clone()
    }

    async fn finish(mut self) {
        use tokio::io::AsyncWriteExt;
        self.input.write_all(b"[\"stop\"]\n").await.unwrap();
        self.input.flush().await.unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await.unwrap().unwrap().success());
        crate::clawd::authority::revoke_session(&self.session);
        crate::paths::RoutedPathContext::for_owner(OWNER, PathBuf::from("/run/display-home"))
            .scope_sync(|| crate::proc::try_deregister_session_for_owner(&self.session, OWNER)).unwrap();
    }
}

async fn decide(owner: u32, decision: &ReviewDecision) -> crate::clawd::protocol::Response {
    crate::clawd::client::request("/run/cos/clawd.sock", crate::clawd::wire::Request::new(
        crate::clawd::routes::Command::SystemReviewDecide,
        json!({"owner_uid": owner, "review": decision}),
    )).await.unwrap()
}

async fn approve_capability(peer: &mut OwnerPeer, id: &str) -> crate::approvals::Request {
    let request = crate::approvals::lookup_pending(id).unwrap();
    assert_eq!(request.owner_uid, Some(OWNER));
    assert_eq!(request.verb, "sys.package");
    assert_eq!(request.risk, Some(crate::caps::Risk::Critical));
    assert!(request.session.starts_with("package-intent:"));
    assert_eq!(request.operation_digest.as_ref().unwrap().len(), 64);
    let view: SystemReview = serde_json::from_value(peer.result(&["review", "show", id]).await).unwrap();
    view.validate().unwrap();
    assert_eq!(view.kind, ReviewKind::Capability);
    let mut decision = ReviewDecision {
        id: view.id.clone(), revision: view.revision, action: ReviewAction::ApplyChoices,
        choices: vec![PermissionSelection {
            permission_id: view.permissions[0].id.clone(), choice: PermissionChoice::AllowOnce,
        }],
    };
    assert!(!decide(OWNER + 1, &decision).await.ok);
    decision.revision += 1;
    assert!(!decide(OWNER, &decision).await.ok);
    decision.revision = view.revision;
    assert!(decide(OWNER, &decision).await.ok);
    request
}

fn body(name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(Path::new(DATA).join(name)).unwrap()).unwrap()
}

fn owned_marker(name: &str) {
    let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
        .open(Path::new(DATA).join(name)).unwrap();
    file.write_all(b"continue exact private test request").unwrap();
    file.sync_all().unwrap();
}

fn live(processes: &[OwnedFd]) {
    for process in processes {
        assert!(!claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
    }
}

pub(super) async fn run(group: &std::fs::File) {
    let mut owner = OwnerPeer::start().await;
    for arguments in [
        vec!["app", DENIED_APP, "probe"],
        vec!["app", DENIED_APP, "fixture.package-intent", "--args-stdin"],
    ] {
        let denied = owner.result(&arguments).await;
        assert_eq!(denied["intent_refused"], true, "{denied}");
        assert_eq!(denied["mcp"], arguments.last() == Some(&"--args-stdin"));
    }
    let public = crate::clawd::client::request("/run/cos/clawd.sock", crate::clawd::wire::Request::new(
        crate::clawd::routes::Command::GuiPackageIntent,
        json!({"session": owner.session, "mode": "review", "invocation_id": "f".repeat(32),
            "action": "install", "packages": [EFFECT]}),
    )).await.unwrap();
    assert!(!public.ok, "the public route became a GUI authority channel");

    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam("gui", "22", group, Stdio::from(child_control));
    assert_eq!(status(&control, &pam, Instant::now() + Duration::from_secs(30)), *b"AUTH");
    command(&control, b"CASE");
    command(&control, b"22..");
    let child = bootstrap::receive(control.as_fd(), *b"CHLD", Instant::now() + Duration::from_secs(5)).unwrap();
    assert_eq!(kernel::sender(child.credentials).unwrap(), ProcessIdentity::child(&pam).unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    let launcher = loop {
        let process = ProcessIdentity::from_pidfd(child.descriptor.as_fd()).unwrap();
        if process.uid() == OWNER && process.gid() == OWNER { break process; }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let parent = launch_parent(&launcher, 22);
    command(&control, b"RUN!");
    wait_marker(&Path::new(DATA).join("package-reviews.json"), &mut pam);
    let local = body("package-intent-local.json");
    let invocation = local["invocation_id"].as_str().unwrap();
    let session = local["session"].as_str().unwrap();
    let context = crate::paths::RoutedPathContext::for_owner(OWNER, PathBuf::from("/run/display-home"));
    context.clone().scope_sync(|| {
        for id in [parent.as_str(), session] {
            let info = crate::proc::session_info_by_id(id).unwrap();
            assert_eq!(info.app_id.is_some(), id == session);
            assert!(info.caps.as_ref().unwrap().iter().all(|cap| cap.verb != Verb::SYS_PACKAGE));
        }
    });
    let reviews = body("package-reviews.json");
    let ids = reviews["main"]["approval_requests"].as_array().unwrap();
    let first = approve_capability(&mut owner, ids[0].as_str().unwrap()).await;
    let targets = [SUPPORT, EFFECT].map(|name| Cap::new(Verb::SYS_PACKAGE, Scope::name(name)));
    assert!(crate::approvals::consume_grant_set_once_for_owner_operation_evidence(
        &first.session, &targets, Some(OWNER), first.operation_digest.as_deref(),
    ).unwrap().is_none(), "partial decisions were consumed non-atomically");
    let first_cap = Cap::new(Verb::SYS_PACKAGE, first.scope.clone());
    assert!(!crate::approvals::has_approved_grant_for_context_operation(
        &first.session, &first_cap, Some(OWNER), None, Some(&"0".repeat(64)),
    ).unwrap());
    let wrong = Cap::new(Verb::SYS_PACKAGE, Scope::name("claw-native-package-neighbor"));
    assert!(!crate::approvals::has_approved_grant_for_context_operation(
        &first.session, &wrong, Some(OWNER), None, first.operation_digest.as_deref(),
    ).unwrap());
    assert!(owner.result(&["package-transactions", "--invocation", invocation]).await["transactions"]
        .as_array().unwrap().is_empty());
    approve_capability(&mut owner, ids[1].as_str().unwrap()).await;
    let unused_id = reviews["unused"]["approval_requests"][0].as_str().unwrap();
    let unused = approve_capability(&mut owner, unused_id).await;
    let pending_id = reviews["pending"]["approval_requests"][0].as_str().unwrap();
    let pending = crate::approvals::lookup_pending(pending_id).unwrap();
    assert_eq!(unused.scope, Scope::name(UNUSED));
    assert_eq!(pending.scope, Scope::name(PENDING));
    wait_marker(&Path::new(DATA).join("package-permission-ready.json"), &mut pam);
    assert_eq!(body("package-permission-ready.json")["accepted"], false);
    assert!(owner.result(&["package-transactions", "--invocation", invocation]).await["transactions"]
        .as_array().unwrap().is_empty(), "ready was treated as retained acceptance");
    assert!(!crate::clawd::packages::query_package_state(EFFECT).await.unwrap().installed);
    owned_marker("package-submit-go");
    wait_marker(&Path::new(DATA).join("package-receipt-discarded.json"), &mut pam);
    let deadline = Instant::now() + Duration::from_secs(35);
    let transaction = loop {
        let records = owner.result(&["package-transactions", "--invocation", invocation]).await;
        if let Some(record) = records["transactions"].as_array().unwrap().first() {
            if record["phase"] == "awaiting_impact_confirmation" { break record.clone(); }
            assert!(!matches!(record["phase"].as_str(), Some("applied" | "not_applied" | "indeterminate")), "{record}");
        }
        assert!(Instant::now() < deadline, "Root did not retain the separate impact review");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(transaction["operation"], json!({"action": "install-packages", "packages": [SUPPORT, EFFECT]}));
    assert!(transaction["app_reviews"].as_array().unwrap().is_empty());
    assert!(!transaction["plan"]["actions"].as_array().unwrap().is_empty(), "no-effect plan is not acceptance");
    assert_eq!(transaction["app_id"], APP);
    let request_id = transaction["request_id"].as_str().unwrap();
    let retained = retained_instance(session);
    let runtime = Path::new("/run/cos/gui").join(claw_display_control::Epoch(retained.instance.0).label());
    let group_path = std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
    let cgroup_pids = std::fs::read_to_string(group_path.join("cgroup.procs")).unwrap();
    let groups = cgroup_pids.lines().map(|pid| {
        let group = unsafe { libc::getpgid(pid.parse::<i32>().unwrap()) };
        assert!(group > 0);
        group
    }).collect::<std::collections::BTreeSet<_>>();
    assert!(groups.len() >= 2, "one process-group kill would not prove cgroup retirement");
    let processes = cgroup_pids.lines().map(|pid| {
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.parse::<i32>().unwrap(), 0) };
        assert!(descriptor >= 0);
        unsafe { OwnedFd::from_raw_fd(descriptor as i32) }
    }).collect::<Vec<_>>();
    assert!(processes.len() >= 3, "the GUI must hold a separate-PGID descendant");
    let descendant = body("package-descendant.json");
    assert_ne!(descendant["descendant"], descendant["own_group"]);
    live(&processes);
    assert!(!crate::clawd::packages::query_package_state(EFFECT).await.unwrap().installed);
    let review_id = transaction["global_stop_review_id"].as_str().unwrap();
    let view: SystemReview = serde_json::from_value(owner.result(&["review", "show", review_id]).await).unwrap();
    view.validate().unwrap();
    assert_eq!(view.kind, ReviewKind::PackageImpact);
    assert_eq!(view.status, ReviewStatus::Pending);
    assert!(view.disclosures.iter().any(|detail| detail.value.contains("Unsaved")));
    assert!(!transaction["impact"]["uncertainties"].as_array().unwrap().is_empty());
    let mut global = ReviewDecision {
        id: view.id, revision: view.revision, action: ReviewAction::ConfirmInstall, choices: Vec::new(),
    };
    assert!(!decide(OWNER, &global).await.ok);
    global.action = ReviewAction::ConfirmGlobalAppStop;
    assert!(!decide(OWNER + 1, &global).await.ok);
    global.revision += 1;
    assert!(!decide(OWNER, &global).await.ok);
    global.revision -= 1;
    live(&processes);
    assert!(decide(OWNER, &global).await.ok);
    let deadline = Instant::now() + Duration::from_secs(45);
    let applied = loop {
        let record = owner.result(&["package-transactions", request_id]).await;
        if record["phase"] == "applied" { break record; }
        assert!(!matches!(record["phase"].as_str(), Some("not_applied" | "indeterminate" | "cleanup_pending")), "{record}");
        assert!(Instant::now() < deadline, "Root controller failed to complete after caller retirement: {record}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(applied["completion"]["confirmed"], true);
    assert!(applied["checked_retired_apps"].as_array().unwrap().iter().any(|id| id == APP));
    for process in processes { assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap()); }
    assert!(!runtime.exists(), "GUI provider/runtime retirement was not checked");
    for name in [SUPPORT, EFFECT] {
        let state = crate::clawd::packages::query_package_state(name).await.unwrap();
        assert!(state.installed);
        assert_eq!(state.version.as_deref(), Some("1.0.0"));
    }
    let payload = Path::new("/usr/lib/cos/fixture-side-effect");
    assert_eq!(std::fs::read(payload).unwrap(), b"A normal, successful private package install\n");
    assert!(crate::clawd::packages::query_package_owners(payload).await.unwrap()
        .iter().any(|name| name.split(':').next() == Some(EFFECT)));
    let record_path = Path::new("/var/lib/cos/package-transactions/requests").join(format!("{request_id}.json"));
    let evidence: Value = serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(evidence["cleanup_confirmed"], true);
    assert_eq!(evidence["plan"]["delivery"]["hook_consumed"], true);
    let children = evidence["children"].as_array().unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["ended"], true);
    assert_eq!(children[0]["exit_code"], 0);
    assert!(matches!(crate::approvals::status_for_owner(unused_id, Some(OWNER)),
        crate::approvals::RequestStatus::Consumed));
    assert!(matches!(crate::approvals::status_for_owner(pending_id, Some(OWNER)),
        crate::approvals::RequestStatus::Denied));
    let history = crate::approvals::list_recent_for_owner(1000, Some(OWNER));
    assert!(history.iter().any(|entry| entry.request.id == pending_id
        && entry.decision.decided_by.as_deref() == Some("system:validation")));
    assert!(history.iter().any(|entry| entry.request.id == unused_id
        && entry.decision.outcome == crate::approvals::Outcome::Approved));
    owner.finish().await;
    let before = std::fs::read("/var/lib/dpkg/status").unwrap();
    let mut replacement = OwnerPeer::start().await;
    let recovered = replacement.result(&["package-transactions", "--recover", request_id]).await;
    assert_eq!(recovered["phase"], "applied");
    assert_eq!(replacement.result(&["package-transactions", "--invocation", invocation]).await["transactions"]
        .as_array().unwrap().len(), 1);
    assert_eq!(std::fs::read("/var/lib/dpkg/status").unwrap(), before);
    let after: Value = serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
    assert_eq!(after["children"], evidence["children"], "read-only recovery replayed APT");
    replacement.finish().await;
    command(&control, b"WAIT");
    assert_eq!(status(&control, &pam, Instant::now() + Duration::from_secs(30)), *b"FAIL");
    command(&control, b"CLSE");
    assert!(pam.wait().unwrap().success());
    println!("private GUI case 22: signed common-Host package probe, exact Critical batch review, separate global impact, discarded receipt/frontend loss, checked caller/PGID retirement and actual APT child zero/read-only recovery passed; native Store ELF not tested");
}

#[test]
#[ignore = "isolated native-package-only Root PAM/display/APT lane; never run on a shared host"]
fn private_native_package_self_handoff() {
    super::private_gui_fixture_selected(false, false, true);
}
