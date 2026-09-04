//! Adversarial tests for the *sandboxed* App session server.
//!
//! An App with a `session` block runs a long-lived stdio JSON-RPC peer
//! that outlives every individual tool call. These tests launch a real
//! fixture server through the same derivation and provider the kernel
//! uses, speak the real protocol to it over pipes, and then ask the
//! kernel what it actually built — namespaces, mounts, `/proc`,
//! descriptors, environment, network — rather than asserting on the
//! policy struct that requested it.
//!
//! Every test is skipped, loudly, on a host that cannot enforce the
//! policy. CI and the image builds declare the prerequisites installed
//! by setting `COS_WORKER_SANDBOX_REQUIRED=1`; there an unavailable
//! sandbox is a failure rather than a skip, so a missing dependency
//! cannot quietly turn this suite into a no-op.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::time::{Duration, Instant};

use cos::caps::{Cap, CapSet, Scope, Verb};
use cos::worker::derive::{AppSessionInput, SessionLifetime};
use cos::worker::{BrokerAuthority, LaunchResources, WorkerLaunch};

fn sandbox_available() -> bool {
    cos::worker::availability().is_available()
}

macro_rules! require_sandbox {
    () => {
        if !sandbox_available() {
            let availability = cos::worker::availability();
            if std::env::var_os("COS_WORKER_SANDBOX_REQUIRED").is_some() {
                panic!(
                    "worker sandbox prerequisites are declared installed but missing: {}",
                    availability.refusal()
                );
            }
            eprintln!("skipping: {}", availability.refusal());
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// Fixture server
// ---------------------------------------------------------------------------

/// A hand-rolled stdio MCP server.
///
/// Deliberately not built on the Python SDK: these tests need a server
/// that will misbehave on request — emit an oversized frame, leave a
/// descendant behind, hang past the deadline — which a well-behaved
/// scaffold makes awkward to express.
const FIXTURE_SERVER: &str = r#"
import json, os, pathlib, socket as socket_mod, subprocess, sys, time

DATA = os.environ.get("COS_DATA_DIR", "/tmp")
STATE = {}


def probe():
    out = {}
    out["marker"] = os.environ.get("COS_WORKER_SANDBOX")
    out["app"] = os.environ.get("COS_APP_ID")
    out["session"] = os.environ.get("COS_SESSION")
    out["env"] = sorted(os.environ)
    out["data_dir"] = DATA
    out["cwd"] = os.getcwd()
    out["uid"] = os.getuid()
    out["home_entries"] = sorted(os.listdir("/home")) if os.path.isdir("/home") else None
    out["pids"] = sorted(p for p in os.listdir("/proc") if p.isdigit())
    out["fds"] = len(os.listdir("/proc/self/fd"))
    out["fd_targets"] = sorted(
        set(
            os.path.realpath("/proc/self/fd/" + fd)
            for fd in os.listdir("/proc/self/fd")
        )
    )
    out["ns"] = {
        name: os.readlink("/proc/self/ns/" + name)
        for name in ("pid", "mnt", "net", "user", "ipc", "uts")
    }
    out["seccomp"] = [
        line.split(":", 1)[1].strip()
        for line in pathlib.Path("/proc/self/status").read_text().splitlines()
        if line.startswith(("Seccomp:", "NoNewPrivs:", "CapEff:"))
    ]
    out["interfaces"] = [
        line.split(":", 1)[0].strip()
        for line in pathlib.Path("/proc/net/dev").read_text().splitlines()[2:]
        if ":" in line
    ]
    for label, path in (
        ("shadow", "/etc/shadow"),
        ("cos_state", "/var/lib/cos"),
        ("cos_runtime", "/run/cos"),
        ("broker_sock", "/run/cos/clawd.sock"),
        ("egress_sock", "/run/cos/worker-egress.sock"),
    ):
        out[label] = os.path.exists(path)
    try:
        socket_mod.socket(socket_mod.AF_INET, socket_mod.SOCK_STREAM)
        out["af_inet"] = "OPENED"
    except OSError as error:
        out["af_inet"] = "refused:%d" % error.errno
    a, b = socket_mod.socketpair()
    a.send(b"ping")
    out["af_unix"] = b.recv(4).decode()
    try:
        pathlib.Path("/usr/bin/planted").write_text("x")
        out["root_write"] = "WRITABLE"
    except OSError:
        out["root_write"] = "read-only"
    own = pathlib.Path(DATA) / "own.txt"
    own.write_text("private")
    out["own_write"] = own.read_text()
    return out


def policy(verb, name):
    binary = os.environ.get("CLAW_COS_BIN", "cos")
    proc = subprocess.run(
        [binary, "__policy", "check", verb, "--name", name],
        capture_output=True,
        text=True,
    )
    body = (proc.stdout or proc.stderr or "").strip()
    try:
        return {"exit": proc.returncode, "body": json.loads(body)}
    except ValueError:
        return {"exit": proc.returncode, "raw": body[:400]}


def descendant(marker, delay):
    # Double-fork so the child reparents away from this server: only a
    # process-group or cgroup kill reaches it.
    script = "import os,time,sys;time.sleep(%f);open(%r,'w').write('survived')" % (
        delay,
        marker,
    )
    child = subprocess.Popen(
        [sys.executable, "-c", script],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    return {"pid": child.pid}


def write_probe(target, sibling):
    out = {}
    try:
        pathlib.Path(target).write_text("captured")
        out["granted"] = "written"
    except OSError as error:
        out["granted"] = "denied:%d" % error.errno
    try:
        pathlib.Path(sibling).write_text("stolen")
        out["sibling"] = "WRITTEN"
    except OSError as error:
        out["sibling"] = "denied:%d" % error.errno
    # A traversal out of the granted directory names a real host path,
    # so it is the mount that has to refuse it, not the string check.
    escape = os.path.join(os.path.dirname(target), "..", "escaped.txt")
    try:
        pathlib.Path(escape).write_text("escaped")
        out["traversal"] = "WRITTEN"
    except OSError as error:
        out["traversal"] = "denied:%d" % error.errno
    return out


def transport_probe(socket):
    out = {}
    try:
        client = socket_mod.socket(socket_mod.AF_UNIX, socket_mod.SOCK_STREAM)
        client.settimeout(2)
        client.connect(socket)
        client.close()
        out["socket"] = "connected"
    except OSError as error:
        out["socket"] = "denied:%d" % error.errno
    runtime_dir = os.path.dirname(socket)
    try:
        out["runtime_dir"] = sorted(os.listdir(runtime_dir))
    except OSError as error:
        out["runtime_dir"] = "denied:%d" % error.errno
    # The broker socket the sandbox sees is the launch's own shadow.
    out["broker_is_real"] = os.path.realpath("/run/cos/clawd.sock").startswith("/run/user")
    out["env_values"] = sorted(os.environ.values())
    return out


TOOLS = [
    {"name": "probe.isolation", "description": "report the sandbox", "inputSchema": {"type": "object"}},
    {"name": "probe.policy", "description": "ask the broker", "inputSchema": {"type": "object"}},
    {"name": "probe.remember", "description": "keep state", "inputSchema": {"type": "object"}},
    {"name": "probe.recall", "description": "read state", "inputSchema": {"type": "object"}},
    {"name": "probe.spawn", "description": "leave a descendant", "inputSchema": {"type": "object"}},
    {"name": "probe.write", "description": "write where granted", "inputSchema": {"type": "object"}},
    {"name": "probe.transport", "description": "probe the session bus", "inputSchema": {"type": "object"}},
    {"name": "probe.hang", "description": "never answer", "inputSchema": {"type": "object"}},
    {"name": "probe.fail", "description": "tool error", "inputSchema": {"type": "object"}},
]


def call(name, args):
    if name == "probe.isolation":
        return probe()
    if name == "probe.policy":
        return policy(args["verb"], args["name"])
    if name == "probe.remember":
        STATE[args["key"]] = args["value"]
        return {"stored": args["key"]}
    if name == "probe.recall":
        return {"value": STATE.get(args["key"])}
    if name == "probe.spawn":
        return descendant(args["marker"], float(args.get("delay", 3.0)))
    if name == "probe.write":
        return write_probe(args["target"], args["sibling"])
    if name == "probe.transport":
        return transport_probe(args["socket"])
    if name == "probe.hang":
        time.sleep(3600)
        return {}
    if name == "probe.fail":
        raise RuntimeError("fixture refused")
    raise RuntimeError("unknown tool " + name)


def send(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    request = json.loads(line)
    method = request.get("method")
    ident = request.get("id")
    if ident is None:
        continue
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": ident, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "probe", "version": "1.0.0"},
        }})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": ident, "result": {"tools": TOOLS}})
    elif method == "tools/call":
        params = request.get("params") or {}
        try:
            body = call(params.get("name"), params.get("arguments") or {})
            result = {"content": [{"type": "text", "text": json.dumps(body)}]}
        except Exception as failure:
            result = {
                "content": [{"type": "text", "text": str(failure)}],
                "isError": True,
            }
        send({"jsonrpc": "2.0", "id": ident, "result": result})
    elif method == "desync":
        params = request.get("params") or {}
        # Noise a correlating client must ignore: an answer to an id
        # nobody asked for, an unsolicited notification, and a replay
        # of an older id.
        send({"jsonrpc": "2.0", "id": 999999, "result": {"answer": "unasked"}})
        send({"jsonrpc": "2.0", "method": "notifications/message",
              "params": {"level": "info", "data": "noise"}})
        send({"jsonrpc": "2.0", "id": params.get("replay"), "result": {"answer": "replay"}})
        send({"jsonrpc": "2.0", "id": ident, "result": {"answer": "correlated"}})
    else:
        send({"jsonrpc": "2.0", "id": ident, "error": {"code": -32601, "message": "no method"}})
"#;

/// A running fixture session: the sandboxed child plus the framed
/// transport the launcher speaks to it over.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    resources: LaunchResources,
    pid: u32,
    policy_digest: String,
    policy: cos::worker::LaunchPolicy,
    next_id: u64,
}

impl Session {
    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let id = self.next_id;
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{frame}").expect("write request");
        self.stdin.flush().expect("flush request");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        let response: serde_json::Value =
            serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("bad frame {line:?}: {e}"));
        assert_eq!(
            response["id"].as_u64(),
            Some(id),
            "the server answered a different request: {response}"
        );
        response
    }

    /// Call one tool and decode the JSON document the fixture returns.
    fn call(&mut self, tool: &str, args: serde_json::Value) -> serde_json::Value {
        let response = self.request(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": args }),
        );
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text content: {response}"));
        assert_ne!(
            response["result"]["isError"].as_bool(),
            Some(true),
            "tool `{tool}` failed: {text}"
        );
        serde_json::from_str(text).unwrap_or_else(|e| panic!("bad tool body {text:?}: {e}"))
    }

    fn handshake(&mut self) {
        let init = self.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "cos-agent", "version": "test" },
            }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "probe");
        let listed = self.request("tools/list", serde_json::json!({}));
        assert_eq!(
            listed["result"]["tools"].as_array().map(Vec::len),
            Some(9),
            "unexpected tool list: {listed}"
        );
    }

    fn close(mut self) {
        self.resources.kill_all(Some(self.pid));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write the fixture package and return `(package_dir, data_dir)`.
fn fixture_package() -> (tempfile::TempDir, tempfile::TempDir) {
    let package = tempfile::tempdir().expect("package dir");
    let data = tempfile::tempdir().expect("data dir");
    std::fs::write(package.path().join("server.py"), FIXTURE_SERVER).expect("write server");
    (package, data)
}

fn pinned(path: &Path) -> Vec<(PathBuf, (u64, u64))> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).expect("stat entry");
    vec![(path.to_path_buf(), (meta.dev(), meta.ino()))]
}

fn package_identity(dir: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let canonical = dir.canonicalize().expect("canonical package");
    let meta = std::fs::metadata(canonical).expect("stat package");
    Some((meta.dev(), meta.ino()))
}

/// Derive, prepare and start one fixture session under the sandbox.
struct Spec<'a> {
    package: &'a Path,
    data: &'a Path,
    app_id: &'a str,
    session_id: &'a str,
    /// Capabilities the broker endpoint answers with.
    authority: CapSet,
    /// Capabilities the *policy* is derived from.
    policy_caps: CapSet,
    lifetime: SessionLifetime,
    transports: &'a [cos::worker::trusted_desktop::Transport],
}

impl<'a> Spec<'a> {
    fn new(package: &'a Path, data: &'a Path, session_id: &'a str) -> Self {
        Self {
            package,
            data,
            app_id: "probe-fixture",
            session_id,
            authority: CapSet::new(),
            policy_caps: CapSet::new(),
            lifetime: SessionLifetime::Reusable,
            transports: &[],
        }
    }

    fn derive(&self) -> Result<cos::worker::LaunchPolicy, String> {
        let entry = self.package.join("server.py");
        let authorized_mounts = if self.lifetime == SessionLifetime::SingleCall {
            cos::worker::derive::authorize_granted_path_mounts(&self.policy_caps)?
        } else {
            Vec::new()
        };
        cos::worker::derive::app_session(AppSessionInput {
            app_id: self.app_id,
            app_dir: self.package,
            program: PathBuf::from("/usr/bin/python3"),
            argv: vec![entry.to_string_lossy().into_owned()],
            caps: &self.policy_caps,
            authorized_mounts: &authorized_mounts,
            lifetime: self.lifetime,
            session_id: self.session_id,
            data_dir: &self.data.to_string_lossy(),
            apps_dir: &self.package.to_string_lossy(),
            extra_env: BTreeMap::from([("PYTHONUNBUFFERED".to_string(), "1".to_string())]),
            package_identity: package_identity(self.package),
            pinned_entries: pinned(&entry),
            transports: self.transports,
        })
    }
}

fn start_session(spec: Spec<'_>) -> Result<Session, String> {
    let policy = spec.derive()?;
    let policy_digest = policy.digest();
    let shape = policy.clone();
    let launch = WorkerLaunch::new(policy).with_authority(BrokerAuthority::new(
        spec.session_id,
        Some(spec.app_id.to_string()),
        spec.authority.clone(),
        cos::worker::relay_slot(),
    ));
    let prepared = cos::worker::prepare(&launch)?;
    let cos::worker::PreparedLaunch {
        mut command,
        resources,
        ..
    } = prepared;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn fixture: {e}"))?;
    let pid = child.id();
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    Ok(Session {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        resources,
        pid,
        policy_digest,
        policy: shape,
        next_id: 0,
    })
}

fn open_session(package: &Path, data: &Path, session_id: &str, caps: CapSet) -> Session {
    let mut spec = Spec::new(package, data, session_id);
    spec.authority = caps;
    let mut session = start_session(spec).expect("start fixture session");
    session.handshake();
    session
}

// ---------------------------------------------------------------------------
// Isolation
// ---------------------------------------------------------------------------

#[test]
fn a_session_server_runs_under_the_same_isolation_as_any_hostile_worker() {
    require_sandbox!();
    let (package, data) = fixture_package();
    // Secrets and a stray descriptor the launcher holds. Neither may
    // survive into a process that is only supposed to see its package.
    std::env::set_var("OPENAI_API_KEY", "sk-should-not-leak");
    std::env::set_var("SSH_AUTH_SOCK", "/run/user/0/ssh-agent");
    let leaked = data.path().join("launcher-secret.txt");
    std::fs::write(&leaked, "sensitive").expect("marker");
    let held = std::fs::File::open(&leaked).expect("hold descriptor");

    let mut session = open_session(package.path(), data.path(), "app-probe", CapSet::new());
    let facts = session.call("probe.isolation", serde_json::json!({}));

    std::env::remove_var("OPENAI_API_KEY");
    std::env::remove_var("SSH_AUTH_SOCK");
    drop(held);

    // The policy the provider enforced, not the one we asked for.
    assert_eq!(session.policy.tier.as_str(), "mcp-server");
    assert_eq!(session.policy.network.as_str(), "denied");
    assert_eq!(session.policy.seccomp.as_str(), "strict");
    assert_eq!(session.policy.stdio.as_str(), "streamed");
    assert!(session.policy.broker, "the session got no broker endpoint");
    // A server is bounded by its handle, not by a wall clock.
    assert_eq!(session.policy.limits.runtime, Duration::ZERO);

    assert_eq!(facts["marker"], "1", "sandbox marker missing: {facts}");
    assert_eq!(facts["app"], "probe-fixture");
    assert_eq!(facts["session"], "app-probe");

    // Environment: a closed allowlist, no launcher secrets, and none of
    // the launcher's own registry pointers.
    let env: Vec<&str> = facts["env"]
        .as_array()
        .expect("env")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    for leaked in [
        "OPENAI_API_KEY",
        "SSH_AUTH_SOCK",
        "COS_PROC_DATA_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XAUTHORITY",
        "WAYLAND_DISPLAY",
    ] {
        assert!(
            !env.contains(&leaked),
            "{leaked} reached the session: {env:?}"
        );
    }
    assert!(env.contains(&"COS_APP_ID"), "{env:?}");

    // Namespaces: private pid, mount, net, ipc, uts and user.
    let host: BTreeMap<&str, String> = ["pid", "mnt", "net", "user", "ipc", "uts"]
        .into_iter()
        .map(|name| {
            (
                name,
                std::fs::read_link(format!("/proc/self/ns/{name}"))
                    .map(|link| link.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )
        })
        .collect();
    for (name, value) in &host {
        assert_ne!(
            facts["ns"][*name].as_str().unwrap_or_default(),
            value.as_str(),
            "the session shares the host `{name}` namespace"
        );
    }

    // `/proc` shows only the sandbox's own processes.
    let pids = facts["pids"].as_array().expect("pids").len();
    assert!(pids <= 4, "host processes are visible: {facts}");

    // Seccomp filter installed, NNP set, no capabilities.
    let status = facts["seccomp"].as_array().expect("status");
    let status: Vec<&str> = status
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        status.contains(&"2") || status.iter().any(|line| line.trim() == "2"),
        "no seccomp filter is installed: {status:?}"
    );
    assert!(
        status.iter().any(|line| line.trim() == "0000000000000000"),
        "the session kept capabilities: {status:?}"
    );

    // Filesystem: read-only root, its own data partition, nothing else.
    assert_eq!(facts["root_write"], "read-only", "{facts}");
    assert_eq!(facts["own_write"], "private", "{facts}");
    assert_eq!(facts["shadow"], false, "/etc/shadow is inside: {facts}");
    assert_eq!(facts["cos_state"], false, "kernel state is inside: {facts}");
    assert_eq!(
        facts["home_entries"].as_array().map(Vec::len),
        Some(0),
        "another account's home is inside: {facts}"
    );
    // The App data directory is its own partition of the owner's root.
    let mounted = data.path().join("apps/probe-fixture");
    assert_eq!(
        facts["data_dir"].as_str().map(PathBuf::from),
        Some(mounted.canonicalize().expect("partition")),
        "{facts}"
    );
    assert!(
        mounted.join("own.txt").is_file(),
        "the session's write did not land in its own partition"
    );

    // Network: loopback only, no routable domain, AF_UNIX intact.
    assert_eq!(
        facts["interfaces"].as_array().map(Vec::len),
        Some(1),
        "the session has host network: {facts}"
    );
    assert_eq!(facts["af_unix"], "ping", "{facts}");
    assert!(
        facts["af_inet"]
            .as_str()
            .unwrap_or_default()
            .starts_with("refused"),
        "a routable socket was available: {facts}"
    );
    // The egress broker exists for brokered launches only.
    assert_eq!(facts["egress_sock"], false, "{facts}");
    // The broker socket the session *does* see is its own shadow, not
    // the real `clawd` endpoint: `/run/cos` holds nothing else.
    assert_eq!(facts["broker_sock"], true, "{facts}");
    assert_eq!(facts["cos_runtime"], true, "{facts}");

    // No launcher descriptor survives `execve`, and nothing the
    // sandbox cannot see by path is open in it.
    let targets: Vec<&str> = facts["fd_targets"]
        .as_array()
        .expect("fd targets")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        !targets
            .iter()
            .any(|target| target.contains("launcher-secret")),
        "a launcher descriptor survived the launch: {targets:?}"
    );
    assert!(
        !targets
            .iter()
            .any(|target| target.starts_with("/var/lib/cos") || target.contains("registry")),
        "a kernel-state descriptor survived the launch: {targets:?}"
    );
    assert!(
        facts["fds"].as_u64().unwrap_or(u64::MAX) <= 16,
        "descriptors leaked into the session: {facts}"
    );

    session.close();
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

#[test]
fn the_streamed_transport_keeps_state_and_answers_one_response_per_request() {
    require_sandbox!();
    let (package, data) = fixture_package();
    let mut session = open_session(package.path(), data.path(), "app-probe", CapSet::new());

    // In-memory state is what a session is for.
    session.call(
        "probe.remember",
        serde_json::json!({"key": "x", "value": "42"}),
    );
    let recalled = session.call("probe.recall", serde_json::json!({"key": "x"}));
    assert_eq!(recalled["value"], "42");

    // Each request gets exactly one response, correlated by id. The
    // helper asserts the id on every exchange, so a server replaying an
    // old id or answering twice fails here.
    for round in 0..5 {
        let key = format!("k{round}");
        session.call(
            "probe.remember",
            serde_json::json!({"key": key, "value": round}),
        );
        let seen = session.call("probe.recall", serde_json::json!({"key": key}));
        assert_eq!(seen["value"], round);
    }

    // A tool error is a protocol-level result, not a transport failure:
    // the session stays usable afterwards.
    let response = session.request(
        "tools/call",
        serde_json::json!({"name": "probe.fail", "arguments": {}}),
    );
    assert_eq!(response["result"]["isError"], true, "{response}");
    let still_there = session.call("probe.recall", serde_json::json!({"key": "x"}));
    assert_eq!(still_there["value"], "42");

    session.close();
}

// ---------------------------------------------------------------------------
// Authority
// ---------------------------------------------------------------------------

/// Register a routed session row so the broker endpoint has a live
/// capability set to read, and return a guard that removes it.
struct SessionRow {
    id: String,
}

impl SessionRow {
    fn install(id: &str, proc_dir: &Path) -> Self {
        std::env::set_var("COS_PROC_DATA_DIR", proc_dir);
        cos::proc::register_session(cos::proc::SessionInfo {
            session_id: id.to_string(),
            pid: std::process::id(),
            command: vec!["cos app probe-fixture session".to_string()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some("app".to_string()),
            parent: None,
            workdir: None,
            exit_code: None,
            ended_at: None,
            tier: None,
            scope: None,
            priority: None,
            caps: Some(CapSet::new()),
            transient_caps: None,
            role: None,
            app_id: Some("probe-fixture".to_string()),
            pending_bind: false,
            start_time_ticks: None,
            client: cos::session::SessionClient::new(cos::session::SessionSource::App, false, true),
        })
        .expect("register session row");
        Self { id: id.to_string() }
    }

    fn grant(&self, caps: Option<CapSet>) {
        cos::proc::set_app_session_transient_caps(&self.id, caps).expect("set transient caps");
    }
}

impl Drop for SessionRow {
    fn drop(&mut self) {
        cos::proc::deregister_session(&self.id);
        std::env::remove_var("COS_PROC_DATA_DIR");
    }
}

#[test]
fn a_transient_grant_is_answered_only_while_the_call_holds_it() {
    require_sandbox!();
    let (package, data) = fixture_package();
    let proc_dir = tempfile::tempdir().expect("proc dir");
    let row = SessionRow::install("app-transient", proc_dir.path());
    std::env::set_var("COS_BIN", env!("CARGO_BIN_EXE_cos"));

    let mut session = open_session(package.path(), data.path(), "app-transient", CapSet::new());

    // At rest a session tool holds nothing at all.
    let idle = session.call(
        "probe.policy",
        serde_json::json!({"verb": "data.kv.read", "name": "x"}),
    );
    assert_eq!(idle["body"]["decision"], "deny", "{idle}");

    // The launcher installs the exact capability for one call.
    row.grant(Some(CapSet::from_caps(vec![Cap::new(
        Verb::DATA_KV_READ,
        Scope::name("x"),
    )])));
    let granted = session.call(
        "probe.policy",
        serde_json::json!({"verb": "data.kv.read", "name": "x"}),
    );
    assert_eq!(granted["body"]["decision"], "allow", "{granted}");

    // The grant is exact: a neighbouring scope is not covered by it.
    let neighbour = session.call(
        "probe.policy",
        serde_json::json!({"verb": "data.kv.read", "name": "y"}),
    );
    assert_eq!(neighbour["body"]["decision"], "deny", "{neighbour}");
    // Nor is a different verb on the same scope.
    let other_verb = session.call(
        "probe.policy",
        serde_json::json!({"verb": "data.kv.write", "name": "x"}),
    );
    assert_eq!(other_verb["body"]["decision"], "deny", "{other_verb}");

    // Cleared when the call ends — including on the error and timeout
    // paths, which clear through the same `Drop`.
    row.grant(None);
    let cleared = session.call(
        "probe.policy",
        serde_json::json!({"verb": "data.kv.read", "name": "x"}),
    );
    assert_eq!(
        cleared["body"]["decision"], "deny",
        "a reused worker kept the previous call's capability: {cleared}"
    );

    std::env::remove_var("COS_BIN");
    session.close();
    drop(row);
}

#[test]
fn a_standing_resource_grant_refuses_the_launch_instead_of_widening_it() {
    require_sandbox!();
    let (package, data) = fixture_package();
    let reachable = tempfile::tempdir().expect("granted dir");

    // A session-level filesystem grant would have to become a mount
    // that every later call inherits, so the launch fails closed.
    let standing = CapSet::from_caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path(reachable.path().to_string_lossy()),
    )]);
    let mut spec = Spec::new(package.path(), data.path(), "app-standing");
    spec.policy_caps = standing;
    let refused = match start_session(spec) {
        Ok(_) => panic!("a standing filesystem grant must refuse the launch"),
        Err(error) => error,
    };
    assert!(refused.contains("standing filesystem grant"), "{refused}");

    // Same for a standing egress grant.
    let standing = CapSet::from_caps(vec![Cap::new(
        Verb::NET_DIAL,
        Scope::host("example.com:443"),
    )]);
    let mut spec = Spec::new(package.path(), data.path(), "app-standing");
    spec.policy_caps = standing;
    let refused = match start_session(spec) {
        Ok(_) => panic!("a standing egress grant must refuse the launch"),
        Err(error) => error,
    };
    assert!(refused.contains("standing network grant"), "{refused}");
}

// ---------------------------------------------------------------------------
// Containment
// ---------------------------------------------------------------------------

#[test]
fn closing_a_session_kills_every_descendant_it_left_behind() {
    require_sandbox!();
    let (package, data) = fixture_package();
    let mut session = open_session(package.path(), data.path(), "app-probe", CapSet::new());

    // The marker lands in the App's own partition, which is bound into
    // the sandbox at the same path — so the launcher can see whether a
    // reparented descendant ever got to write it.
    let partition = data
        .path()
        .join("apps/probe-fixture")
        .canonicalize()
        .expect("partition");
    let marker = partition.join("survivor.txt");
    session.call(
        "probe.spawn",
        serde_json::json!({"marker": marker.to_string_lossy(), "delay": 3.0}),
    );
    session.close();

    // Well past the descendant's delay: nothing may have written it.
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        !marker.exists(),
        "a descendant outlived the session that started it"
    );
}

#[test]
fn a_hung_call_is_bounded_by_the_launcher_and_the_kill_reaches_the_group() {
    require_sandbox!();
    let (package, data) = fixture_package();
    let mut session = open_session(package.path(), data.path(), "app-probe", CapSet::new());

    // The server is told to never answer. The launcher owns the clock:
    // it stops waiting and tears the worker down rather than blocking
    // on a peer that has decided not to reply.
    session.next_id += 1;
    let id = session.next_id;
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": "probe.hang", "arguments": {}},
    });
    writeln!(session.stdin, "{frame}").expect("write request");
    session.stdin.flush().expect("flush");

    let pid = session.pid;
    session.close();

    // The whole group is gone, not just the direct child.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the session worker survived its teardown");
}

#[test]
fn two_sessions_of_the_same_app_get_different_sandboxes() {
    require_sandbox!();
    let (package, data) = fixture_package();
    let first = open_session(package.path(), data.path(), "app-one", CapSet::new());
    let second = open_session(package.path(), data.path(), "app-two", CapSet::new());
    // The policy digest binds the session identity, so a worker
    // launched for one session can never be mistaken for another's.
    assert_ne!(
        first.policy_digest, second.policy_digest,
        "two sessions derived an identical policy"
    );
    first.close();
    second.close();
}

// ---------------------------------------------------------------------------
// Single-call workers
// ---------------------------------------------------------------------------

#[test]
fn a_single_call_worker_writes_only_where_the_call_was_granted() {
    require_sandbox!();
    let (package, data) = fixture_package();
    let resources = tempfile::tempdir().expect("resources");
    let granted = resources.path().join("shots");
    std::fs::create_dir(&granted).expect("granted dir");
    let sibling = resources.path().join("private");
    std::fs::create_dir(&sibling).expect("sibling dir");

    // Exactly what a screenshot call is granted: write into one
    // directory the argument named, and nothing else.
    let call = CapSet::from_caps(vec![Cap::new(
        Verb::FS_WRITE,
        Scope::path(format!("{}/**", granted.display())),
    )]);
    let mut spec = Spec::new(package.path(), data.path(), "app-single");
    spec.policy_caps = call;
    spec.lifetime = SessionLifetime::SingleCall;
    let mut session = start_session(spec).expect("start single-call worker");
    session.handshake();

    let facts = session.call(
        "probe.write",
        serde_json::json!({
            "target": granted.join("shot.png").to_string_lossy(),
            "sibling": sibling.join("stolen.txt").to_string_lossy(),
        }),
    );
    assert_eq!(
        facts["granted"], "written",
        "the granted write failed: {facts}"
    );
    assert!(
        facts["sibling"]
            .as_str()
            .unwrap_or_default()
            .starts_with("denied"),
        "a sibling directory was writable: {facts}"
    );
    assert_eq!(
        std::fs::read_to_string(granted.join("shot.png")).expect("granted write"),
        "captured"
    );
    assert!(!sibling.join("stolen.txt").exists());
    // The traversal target resolves to the *parent* of the granted
    // directory, which is not bound: inside the sandbox that path is a
    // private tmpfs the launch owns, so a write there succeeds and
    // reaches nothing. What matters is that the host is untouched.
    assert!(
        !resources.path().join("escaped.txt").exists(),
        "a traversal out of the granted mount reached the host: {facts}"
    );
    // A single-call worker is bounded by a wall clock; the reusable
    // server is bounded by its handle.
    assert!(session.policy.limits.deadline().is_some());
    session.close();
}

#[test]
fn a_single_call_worker_reaches_only_its_exact_endpoint() {
    require_sandbox!();
    let (package, data) = fixture_package();
    let call = CapSet::from_caps(vec![Cap::new(
        Verb::NET_DIAL,
        Scope::host("example.com:443"),
    )]);
    let mut spec = Spec::new(package.path(), data.path(), "app-egress");
    spec.policy_caps = call;
    spec.lifetime = SessionLifetime::SingleCall;
    let session = start_session(spec).expect("start single-call worker");

    // The transient endpoint becomes a brokered egress rule for this
    // one worker; the reusable shape has no broker at all.
    assert_eq!(session.policy.network.as_str(), "brokered");
    assert_eq!(session.policy.network.endpoints().len(), 1);
    assert_eq!(
        session.policy.network.endpoints()[0].authority(),
        "example.com:443"
    );
    assert_eq!(session.policy.seccomp.as_str(), "strict-network");
    let digest = session.policy_digest.clone();
    session.close();

    // …and it disappears with the worker. The next reusable launch of
    // the same App derives a network-denied policy again.
    let reusable = open_session(package.path(), data.path(), "app-egress", CapSet::new());
    assert_eq!(reusable.policy.network.as_str(), "denied");
    assert_ne!(reusable.policy_digest, digest);
    reusable.close();
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Desktop transports
// ---------------------------------------------------------------------------

/// The owner's real systemd runtime directory, if this host has one
/// shaped the way production requires and with no live bus to disturb.
///
/// The transport resolver takes the owner uid from the launch identity
/// and the runtime directory from `/run/user/<uid>`, so a fixture bus
/// has to live there for the production path to be the one under test.
fn real_runtime_dir() -> Option<PathBuf> {
    let uid = unsafe { libc::geteuid() } as u32;
    let dir = PathBuf::from(format!("/run/user/{uid}"));
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    if !meta.is_dir() {
        return None;
    }
    // Never disturb a live session bus.
    if dir.join("bus").exists() {
        return None;
    }
    Some(dir)
}

/// Bind a fixture bus at the real `<runtime>/bus` and remove it after.
struct FixtureBus {
    path: PathBuf,
    _listener: std::os::unix::net::UnixListener,
    previous: Option<std::ffi::OsString>,
}

impl FixtureBus {
    fn install(runtime: &Path) -> Self {
        let path = runtime.join("bus");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind fixture bus");
        let previous = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
        std::env::set_var(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={},guid=abc", path.display()),
        );
        Self {
            path,
            _listener: listener,
            previous,
        }
    }
}

impl Drop for FixtureBus {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        match self.previous.take() {
            Some(value) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value),
            None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
        }
    }
}

#[test]
fn a_normal_signed_app_cannot_request_a_desktop_transport() {
    require_sandbox!();
    let (package, data) = fixture_package();
    // The fixture is not a vendor package and its id is not in the
    // kernel table, so the derivation it gets has no display mount and
    // the ordinary hostile tier.
    let session = open_session(package.path(), data.path(), "app-probe", CapSet::new());
    assert_eq!(session.policy.tier.as_str(), "mcp-server");
    assert!(
        !session
            .policy
            .mounts
            .iter()
            .any(|mount| mount.class == cos::worker::MountClass::Display),
        "an unclassified App received a display transport"
    );
    session.close();

    // And the policy validator refuses one even if a caller tried to
    // hand-build it: a display-class mount requires a display tier.
    let entry = package.path().join("server.py");
    let mut policy = cos::worker::derive::app_session(AppSessionInput {
        app_id: "probe-fixture",
        app_dir: package.path(),
        program: PathBuf::from("/usr/bin/python3"),
        argv: vec![entry.to_string_lossy().into_owned()],
        caps: &CapSet::new(),
        authorized_mounts: &[],
        lifetime: SessionLifetime::Reusable,
        session_id: "app-probe",
        data_dir: &data.path().to_string_lossy(),
        apps_dir: &package.path().to_string_lossy(),
        extra_env: BTreeMap::new(),
        package_identity: None,
        pinned_entries: Vec::new(),
        transports: &[],
    })
    .expect("derive");
    policy.mounts.push(cos::worker::Mount::read_write(
        "/run/user/0/bus",
        "/run/user/0/bus",
        cos::worker::MountClass::Display,
    ));
    let refused = cos::worker::prepare(&WorkerLaunch::new(policy))
        .err()
        .expect("a display mount on a headless tier must be refused");
    assert!(
        refused.contains("display transport"),
        "unexpected refusal: {refused}"
    );
}

#[test]
fn a_classified_transport_binds_one_socket_and_lifts_the_tier() {
    require_sandbox!();
    let Some(runtime) = real_runtime_dir() else {
        eprintln!("skipping: no usable /run/user/<uid> without a live bus");
        return;
    };
    let (package, data) = fixture_package();
    // A neighbour the owner also keeps in the same runtime directory.
    let neighbour = runtime.join("cos-test-keyring");
    let _ = std::fs::remove_file(&neighbour);
    let _keyring = std::os::unix::net::UnixListener::bind(&neighbour).expect("neighbour");
    let bus = FixtureBus::install(&runtime);

    let mut spec = Spec::new(package.path(), data.path(), "app-desktop");
    spec.app_id = "cosmic-player";
    spec.transports = &[cos::worker::trusted_desktop::Transport::SessionBus];
    let mut session = start_session(spec).expect("start classified worker");

    assert_eq!(session.policy.tier.as_str(), "trusted-desktop-session");
    // Still sandboxed exactly like any other hostile stdio server.
    assert!(session.policy.tier.is_sandboxed());
    assert_eq!(session.policy.network.as_str(), "denied");
    assert_eq!(session.policy.seccomp.as_str(), "strict");
    assert_eq!(session.policy.stdio.as_str(), "streamed");
    let display: Vec<_> = session
        .policy
        .mounts
        .iter()
        .filter(|mount| mount.class == cos::worker::MountClass::Display)
        .collect();
    assert_eq!(display.len(), 1, "expected exactly one transport mount");
    assert_eq!(display[0].source, bus.path);
    // Pinned by inode like any other authenticated mount, so a socket
    // swapped between derivation and `execve` fails the launch.
    let meta = std::fs::symlink_metadata(&bus.path).expect("bus meta");
    assert_eq!(
        display[0].expect_identity,
        Some((
            std::os::unix::fs::MetadataExt::dev(&meta),
            std::os::unix::fs::MetadataExt::ino(&meta)
        )),
        "the transport mount is not pinned to the socket's inode"
    );
    // The destination is a fixed private path, not the host layout:
    // nothing about the owner's uid or runtime directory crosses in.
    assert_eq!(
        display[0].target,
        PathBuf::from(cos::worker::trusted_desktop::SANDBOX_SESSION_BUS)
    );
    assert!(
        !session
            .policy
            .mounts
            .iter()
            .any(|mount| mount.source == neighbour || mount.source == runtime),
        "the runtime directory or a neighbouring socket came along"
    );
    assert_eq!(
        session
            .policy
            .env
            .get("DBUS_SESSION_BUS_ADDRESS")
            .map(String::as_str),
        Some(
            format!(
                "unix:path={}",
                cos::worker::trusted_desktop::SANDBOX_SESSION_BUS
            )
            .as_str()
        )
    );

    // It runs, it can reach the bus at the fixed path, and it cannot
    // see the owner's other sockets.
    session.handshake();
    let facts = session.call(
        "probe.transport",
        serde_json::json!({ "socket": cos::worker::trusted_desktop::SANDBOX_SESSION_BUS }),
    );
    assert_eq!(facts["socket"], "connected", "{facts}");
    // The mount point sits beside the launch's own shadow broker and
    // nothing else. The owner's neighbouring sockets stay on the host.
    assert_eq!(
        facts["runtime_dir"],
        serde_json::json!(["clawd.sock", "session-bus"]),
        "more than the bus and the launch broker is visible: {facts}"
    );
    // The real daemon socket is still shadowed by the launch broker.
    assert_eq!(facts["broker_is_real"], false, "{facts}");
    // …and the owner's host path is nowhere in the environment.
    assert!(
        !facts["env_values"]
            .as_array()
            .map(|values| values
                .iter()
                .any(|value| value.as_str().unwrap_or_default().contains("/run/user/")))
            .unwrap_or(false),
        "the owner's runtime path leaked into the sandbox: {facts}"
    );
    session.close();
    let _ = std::fs::remove_file(&neighbour);
}

#[test]
fn a_socket_swapped_after_derivation_fails_the_launch() {
    require_sandbox!();
    let Some(runtime) = real_runtime_dir() else {
        eprintln!("skipping: no usable /run/user/<uid> without a live bus");
        return;
    };
    let (package, data) = fixture_package();
    let bus = FixtureBus::install(&runtime);

    let mut spec = Spec::new(package.path(), data.path(), "app-swap");
    spec.app_id = "cosmic-player";
    spec.transports = &[cos::worker::trusted_desktop::Transport::SessionBus];
    let policy = spec.derive().expect("derive with the fixture bus");

    // Replace the socket at the same path between derivation and
    // preparation — the window a path-based bind would lose.
    std::fs::remove_file(&bus.path).expect("remove fixture bus");
    let _replacement =
        std::os::unix::net::UnixListener::bind(&bus.path).expect("rebind fixture bus");

    let refused = cos::worker::prepare(&WorkerLaunch::new(policy).with_authority(
        BrokerAuthority::new(
            "app-swap",
            Some("cosmic-player".to_string()),
            CapSet::new(),
            cos::worker::relay_slot(),
        ),
    ))
    .err()
    .expect("a swapped socket must fail the launch");
    assert!(
        refused.contains("not the verified artifact"),
        "unexpected refusal: {refused}"
    );
}

// ---------------------------------------------------------------------------
// A malicious server
// ---------------------------------------------------------------------------

#[test]
fn a_desynchronising_server_cannot_make_a_request_take_a_stale_answer() {
    require_sandbox!();
    let (package, data) = fixture_package();
    let mut session = open_session(package.path(), data.path(), "app-probe", CapSet::new());

    // The server replies to an id nobody asked for, emits an
    // unsolicited notification, replays the previous id, and only then
    // answers. `Session::request` asserts the correlation on every
    // exchange, so a reader that accepted any of the noise as the
    // answer would fail here.
    session.next_id += 1;
    let id = session.next_id;
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "desync",
        "params": {"replay": id - 1},
    });
    writeln!(session.stdin, "{frame}").expect("write request");
    session.stdin.flush().expect("flush");

    // Drain until the correlated answer arrives; nothing before it may
    // carry this id.
    let mut seen = 0;
    loop {
        let mut line = String::new();
        session.stdout.read_line(&mut line).expect("read");
        let value: serde_json::Value = serde_json::from_str(line.trim()).expect("frame");
        seen += 1;
        assert!(seen < 10, "the server never answered: {value}");
        if value["id"].as_u64() == Some(id) {
            assert_eq!(value["result"]["answer"], "correlated", "{value}");
            break;
        }
        assert_ne!(
            value["id"].as_u64(),
            Some(id),
            "a stale frame carried the live id: {value}"
        );
    }
    session.close();
}
