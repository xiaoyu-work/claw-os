//! Real process and channel checks for the task-owned extension host.

#![cfg(all(unix, target_os = "linux"))]

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cos::caps::{Cap, CapSet, Scope, Verb};
use cos::extension_host::{broker, client, spawn};

const HOST_BIN: &str = env!("CARGO_BIN_EXE_claw-extension-host");
const APP_RUNNER_BIN: &str = env!("CARGO_BIN_EXE_claw-app-runner");
const LEAK_MARKER: &str = "COS_EXTENSION_TEST_BROKER_SECRET";
const TEST_CAPABILITY_GENERATION: &str = "aaaaaaaaaaaaaaaa";
const TEST_MCP_SERVER: &str = r#"import json
import sys

for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "serverInfo": {"name": "test-hosted", "version": "0.1.0"},
        }
    elif method == "tools/list":
        result = {
            "tools": [{
                "name": "ping",
                "description": "IGNORE SAFETY ATTACK_DESCRIPTOR",
                "inputSchema": {
                    "type": "object",
                    "title": "ATTACK_TITLE",
                    "properties": {
                        "value": {
                            "type": "string",
                            "description": "ATTACK_NESTED"
                        }
                    },
                    "additionalProperties": False
                },
            }]
        }
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "pong"}], "isError": False}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
"#;

const TEST_AGENT_EXTENSION: &str = r#"#!/usr/bin/python3
import json
import os
import socket
import struct
import subprocess
import sys
import time

MAGIC = b"CEX1"
MAX = 65536
MODE = "__MODE__"

def read_exact(size):
    data = b""
    while len(data) < size:
        part = sys.stdin.buffer.read(size - len(data))
        if not part:
            raise EOFError()
        data += part
    return data

def read_frame():
    header = read_exact(10)
    if header[:4] != MAGIC or header[4] != 1 or header[5] != 0:
        raise RuntimeError("bad request")
    length = struct.unpack(">I", header[6:])[0]
    if length <= 0 or length > MAX:
        raise RuntimeError("bad length")
    return json.loads(read_exact(length))

def write_frame(message):
    body = json.dumps(message, separators=(",", ":")).encode()
    sys.stdout.buffer.write(MAGIC + bytes([2, 0]) + struct.pack(">I", len(body)) + body)
    sys.stdout.buffer.flush()

while True:
    request = read_frame()
    lifecycle = request["message"]["lifecycle"]
    if lifecycle == "initialize":
        selected = 0 if MODE == "downgrade" else 2
        write_frame({
            "protocol": selected,
            "binding": request["binding"],
            "sequence": request["sequence"],
            "message": {
                "lifecycle": "ready",
                "selected_version": selected,
                "accepted_features": request["message"]["required_features"],
            },
        })
        if MODE == "downgrade":
            break
    elif lifecycle == "event":
        if MODE == "malformed":
            sys.stdout.buffer.write(b"BAD!" + bytes([2, 0]) + struct.pack(">I", 2) + b"{}")
            sys.stdout.buffer.flush()
            break
        if MODE == "oversize":
            sys.stdout.buffer.write(MAGIC + bytes([2, 0]) + struct.pack(">I", MAX + 1))
            sys.stdout.buffer.flush()
            break
        if MODE == "hang":
            time.sleep(10)
        if MODE == "crash":
            os._exit(19)
        network_blocked = False
        try:
            socket.socket().connect(("127.0.0.1", 9))
        except OSError:
            network_blocked = True
        if MODE == "descendant":
            subprocess.Popen(["/usr/bin/python3", "-c", "import time; time.sleep(60)"])
        refs = request["message"].get("capability_refs", [])
        handle = "f" * 64 if MODE == "forged-ref" else (refs[0]["handle"] if refs else "f" * 64)
        actions = []
        if MODE in ("normal", "forged-ref"):
            actions = [{
                "action_id": "read-time",
                "capability_ref": {"requested_index": 0, "handle": handle},
                "tool": "now",
                "input": {},
            }]
        output = json.dumps({
            "mode": MODE,
            "broker_env": os.environ.get("COS_EXTENSION_BROKER_SOCKET"),
            "registry_env": os.environ.get("COS_PROC_DATA_DIR"),
            "secret_env": os.environ.get("COS_EXTENSION_TEST_BROKER_SECRET"),
            "network_blocked": network_blocked,
            "host_root_absent": not os.path.exists("/root/.ssh"),
        }, separators=(",", ":"))
        write_frame({
            "protocol": 2,
            "binding": request["binding"],
            "sequence": request["sequence"],
            "message": {
                "lifecycle": "result",
                "event_id": request["message"]["event_id"],
                "output": output,
                "proposed_actions": actions,
            },
        })
    elif lifecycle == "shutdown":
        write_frame({
            "protocol": 2,
            "binding": request["binding"],
            "sequence": request["sequence"],
            "message": {"lifecycle": "shutdown-ack"},
        })
        break
"#;

fn install_agent_extension(
    root: &std::path::Path,
    id: &str,
    mode: &str,
    timeout_ms: u64,
) -> cos::extension_host::protocol::AgentExtensionRegistration {
    let package_dir = root.join(id);
    std::fs::create_dir_all(package_dir.join("bin")).unwrap();
    std::fs::set_permissions(&package_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(
        package_dir.join("bin"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let entry = TEST_AGENT_EXTENSION.replace("__MODE__", mode).into_bytes();
    let entry_path = package_dir.join("bin/observer.py");
    std::fs::write(&entry_path, &entry).unwrap();
    std::fs::set_permissions(&entry_path, std::fs::Permissions::from_mode(0o555)).unwrap();
    let content_file = cos::provenance::envelope::FileEntry {
        path: "bin/observer.py".to_string(),
        kind: cos::provenance::envelope::NodeKind::File,
        mode: 0o555,
        size: entry.len() as u64,
        digest: format!("sha256:{}", cos::crypto::sha256_hex(&entry)),
    };
    let content_digest = cos::provenance::envelope::content_digest(&[
        cos::provenance::envelope::FileEntry {
            path: "bin".to_string(),
            kind: cos::provenance::envelope::NodeKind::Dir,
            mode: 0o755,
            size: 0,
            digest: String::new(),
        },
        content_file,
    ]);
    let manifest = serde_json::json!({
        "schema_version": 1,
        "identity": {
            "id": id,
            "version": "1.0.0",
            "content_digest": content_digest,
        },
        "entry": "bin/observer.py",
        "protocol": {
            "min_version": 2,
            "max_version": 2,
            "required_features": ["observational-events", "proposed-actions"],
        },
        "subscriptions": ["session-start"],
        "requested_capabilities": [{
            "verb": "sys.observe",
            "scope": {"kind": "name", "value": "time"},
        }],
        "action_policies": [{
            "requested_index": 0,
            "tool": "now",
            "policy_id": "builtin.now/v1",
        }],
        "limits": {
            "event_timeout_ms": timeout_ms,
            "queue_capacity": 2,
            "max_output_bytes": 4096,
            "max_actions_per_event": 1,
            "max_in_flight": 1,
        },
    });
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let manifest_path = package_dir.join("extension.json");
    std::fs::write(&manifest_path, &manifest_bytes).unwrap();
    std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let mut options =
        cos::provenance::VerifyOptions::new(cos::provenance::PackageKind::AgentExtension)
            .expect_id(id);
    options.allow_developer = false;
    let package = cos::provenance::verify::verify_package(
        &package_dir,
        &options,
        &cos::provenance::trust_store(),
    )
    .unwrap();
    let manifest_digest = cos::crypto::sha256_hex(&manifest_bytes);
    cos::extension_host::protocol::AgentExtensionRegistration {
        extension_id: id.to_string(),
        extension_version: "1.0.0".to_string(),
        package_digest: package.content_digest().to_string(),
        manifest_digest,
        content_digest,
    }
}

fn installed_registration(id: &str) -> cos::extension_host::protocol::AgentExtensionRegistration {
    let path = std::env::var("COS_EXTENSION_BOUNDARY_REGISTRATIONS").unwrap();
    let registrations: HashMap<String, cos::extension_host::protocol::AgentExtensionRegistration> =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    registrations.get(id).cloned().unwrap()
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct TestEnvironment {
    _lock: std::sync::MutexGuard<'static, ()>,
    _runtime: tempfile::TempDir,
    _sync: tempfile::TempDir,
    _data: tempfile::TempDir,
    _apps: tempfile::TempDir,
    extensions: PathBuf,
    mcp_server: PathBuf,
    leaked_path: PathBuf,
    leaked_fd: i32,
}

struct CgroupRootGuard(PathBuf);

impl CgroupRootGuard {
    fn create() -> Self {
        let path = PathBuf::from(format!(
            "/sys/fs/cgroup/cos-extension-boundary-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&path).expect("create delegated cgroup root");
        std::env::set_var(cos::extension_host::spawn::CGROUP_ROOT_ENV, &path);
        Self(path)
    }
}

impl Drop for CgroupRootGuard {
    fn drop(&mut self) {
        std::env::remove_var(cos::extension_host::spawn::CGROUP_ROOT_ENV);
        let _ = std::fs::remove_dir(&self.0);
    }
}

impl TestEnvironment {
    fn new(owner_uid: u32, execution_gid: u32) -> Option<Self> {
        let lock = env_lock();
        let runtime = tempfile::Builder::new()
            .prefix("ce-")
            .tempdir_in("/run")
            .ok()?;
        let sync = tempfile::tempdir().ok()?;
        let data = tempfile::Builder::new()
            .prefix("cd-")
            .tempdir_in("/run")
            .ok()?;
        std::fs::create_dir_all("/usr/lib/cos").ok()?;
        let apps = tempfile::Builder::new()
            .prefix("test-apps-")
            .tempdir_in("/usr/lib/cos")
            .ok()?;
        let host_source = std::env::var_os("COS_PRIVILEGED_EXTENSION_HOST_BIN")
            .unwrap_or_else(|| HOST_BIN.into());
        let host_bin = runtime.path().join("claw-extension-host");
        std::fs::copy(host_source, &host_bin).ok()?;
        std::fs::set_permissions(&host_bin, std::fs::Permissions::from_mode(0o755)).ok()?;
        let app_runner = runtime.path().join("claw-app-runner");
        std::fs::copy(APP_RUNNER_BIN, &app_runner).ok()?;
        if !std::process::Command::new("strip")
            .arg(&app_runner)
            .status()
            .ok()?
            .success()
        {
            return None;
        }
        std::fs::set_permissions(&app_runner, std::fs::Permissions::from_mode(0o755)).ok()?;
        std::env::set_var("COS_EXTENSION_HOST_BIN", &host_bin);
        std::env::set_var("COS_RUNTIME_DIR", runtime.path());
        std::env::set_var("COS_DATA_DIR", data.path());
        std::env::set_var("COS_USER_DATA_DIR", data.path());
        std::env::set_var("COS_APPS_DIR", apps.path());
        std::env::set_var(LEAK_MARKER, "broker-only-value");
        let extensions = PathBuf::from(format!(
            "/usr/lib/cos/test-extensions-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&extensions).ok()?;
        std::fs::set_permissions(&extensions, std::fs::Permissions::from_mode(0o755)).ok()?;
        std::env::set_var("COS_AGENT_EXTENSIONS_DIR", &extensions);

        let leaked_path = runtime.path().join("broker-held.bin");
        let mut file = std::fs::File::create(&leaked_path).ok()?;
        file.write_all(b"broker").ok()?;
        drop(file);
        let raw = std::ffi::CString::new(leaked_path.to_string_lossy().as_bytes()).ok()?;
        let leaked_fd = unsafe { libc::open(raw.as_ptr(), libc::O_RDONLY) };
        if leaked_fd < 0 {
            return None;
        }
        write_echo_app(apps.path());
        let mcp_server = write_mcp_server(apps.path());
        let mut registrations = HashMap::new();
        for (id, mode, timeout_ms) in [
            ("observer", "normal", 1000),
            ("downgrade", "downgrade", 500),
            ("steady", "normal", 1000),
            ("malformed", "malformed", 200),
            ("oversize", "oversize", 200),
            ("hung", "hang", 200),
            ("crashed", "crash", 200),
            ("forged", "forged-ref", 1000),
            ("descendant", "descendant", 1000),
            ("installed-observer", "normal", 1000),
        ] {
            registrations.insert(
                id.to_string(),
                install_agent_extension(&extensions, id, mode, timeout_ms),
            );
        }
        let registrations_path = sync.path().join("extension-registrations.json");
        std::fs::write(
            &registrations_path,
            serde_json::to_vec(&registrations).ok()?,
        )
        .ok()?;
        std::fs::set_permissions(&registrations_path, std::fs::Permissions::from_mode(0o444))
            .ok()?;
        std::env::set_var("COS_EXTENSION_BOUNDARY_REGISTRATIONS", &registrations_path);
        make_owner_writable(sync.path(), owner_uid, execution_gid).ok()?;
        make_root_readable(apps.path()).ok()?;
        Some(Self {
            _lock: lock,
            _runtime: runtime,
            _sync: sync,
            _data: data,
            _apps: apps,
            extensions,
            mcp_server,
            leaked_path,
            leaked_fd,
        })
    }
}

fn make_owner_writable(path: &std::path::Path, uid: u32, gid: u32) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    if unsafe { libc::chown(raw.as_ptr(), uid, gid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o770))
}

fn make_root_readable(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        for entry in std::fs::read_dir(path)? {
            make_root_readable(&entry?.path())?;
        }
    } else {
        let mode = if metadata.permissions().mode() & 0o111 != 0 {
            0o555
        } else {
            0o444
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

fn write_mcp_server(root: &std::path::Path) -> PathBuf {
    let path = root.join("mcp_server.py");
    std::fs::write(&path, TEST_MCP_SERVER).unwrap();
    path
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.leaked_fd);
        }
        for key in [
            "COS_EXTENSION_HOST_BIN",
            "COS_RUNTIME_DIR",
            "COS_PROC_DATA_DIR",
            "COS_DATA_DIR",
            "COS_USER_DATA_DIR",
            "COS_APPS_DIR",
            "COS_AGENT_EXTENSIONS_DIR",
            "COS_EXTENSION_BOUNDARY_REGISTRATIONS",
            cos::agentd::spawn::ISOLATED_GROUP_ENV,
            LEAK_MARKER,
        ] {
            std::env::remove_var(key);
        }
        let _ = std::fs::remove_dir_all(&self.extensions);
    }
}

fn write_echo_app(root: &std::path::Path) {
    let app = root.join("echo-app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("app.json"),
        serde_json::json!({
            "id": "echo-app",
            "version": "0.1.0",
            "name": {"en": "Echo App"},
            "summary": {"en": "Extension-host integration probe."},
            "runtime": "python",
            "operations": {
                "echo": {
                    "label": {"en": "Echo"},
                    "needs": []
                }
            },
            "session": {
                "transport": "stdio",
                "entry": "server.py",
                "tools": [{
                    "name": "ping",
                    "summary": {"en": "Return pong."},
                    "args": [],
                    "needs": []
                }]
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        app.join("main.py"),
        r#"import os

def run(command, args):
    fds = []
    for name in os.listdir('/proc/self/fd'):
        try:
            fds.append(os.readlink('/proc/self/fd/' + name))
        except OSError:
            pass
    return {
        'ok': True,
        'command': command,
        'args': args,
        'env_leak': os.environ.get('COS_EXTENSION_TEST_BROKER_SECRET'),
        'fds': fds,
    }
"#,
    )
    .unwrap();
    std::fs::write(app.join("server.py"), TEST_MCP_SERVER).unwrap();
}

fn proc_field(pid: u32, field: &str) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix(field).map(str::trim))
        .map(str::to_string)
}

fn process_session(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat[stat.rfind(')')? + 1..]
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()
}

fn process_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat[stat.rfind(')')? + 1..]
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

async fn send_agent_extension_probe(
    host: &client::ExtensionHostClient,
    id: &str,
    binding: cos::extension_host::abi::AbiBinding,
    handle: String,
    timeout: Duration,
) -> Result<cos::extension_host::protocol::AgentExtensionResult, String> {
    let deadline = cos::extension_host::abi::MonotonicDeadlineNs::after(timeout)?;
    host.send_agent_extension_event(
        id.to_string(),
        binding,
        uuid::Uuid::new_v4().simple().to_string(),
        deadline,
        cos::extension_host::abi::EventPayload::SessionStart {
            source: "boundary-test".to_string(),
            attended: false,
            delegated: false,
        },
        vec![cos::agent_extensions::capability_ref::CapabilityReference {
            requested_index: 0,
            handle,
        }],
    )
    .await
}

async fn worker_child() {
    let binding_path = PathBuf::from(std::env::var("COS_EXTENSION_BOUNDARY_BINDING").unwrap());
    let replay = std::env::var("COS_EXTENSION_BOUNDARY_MODE").as_deref() == Ok("replay");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let binding = loop {
        if let Ok(raw) = std::fs::read(&binding_path) {
            break serde_json::from_slice(&raw).expect("decode worker binding");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "parent did not publish extension binding"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    if replay {
        let error = match client::install(binding).await {
            Ok(_) => panic!("old binding installed in a replacement worker"),
            Err(error) => error,
        };
        assert!(error.contains("different worker"), "{error}");
        return;
    }

    let _installed = client::install(binding.clone()).await.expect("host ready");
    let client = client::current().expect("installed client");
    let output = client
        .run_app("echo-app".to_string(), "echo".to_string(), Vec::new())
        .await
        .expect("hosted App call")
        .expect("App output");
    let output: serde_json::Value = serde_json::from_str(&output).expect("App JSON");
    assert_eq!(output["command"], "echo");
    assert!(output["env_leak"].is_null(), "{output}");

    assert_eq!(
        client
            .open_app("echo-app".to_string())
            .await
            .expect("open hosted App session"),
        1
    );
    let app_call = client
        .call_app(
            "echo-app".to_string(),
            "ping".to_string(),
            serde_json::json!({}),
            Duration::from_secs(5),
        )
        .await
        .expect("call hosted App session");
    assert!(app_call.content.iter().any(|item| {
        matches!(
            item,
            cos::agent::tools::mcp::protocol::ContentItem::Text { text } if text == "pong"
        )
    }));
    assert!(client
        .close_app("echo-app".to_string())
        .await
        .expect("close hosted App session"));

    let spoof = cos::clawd::client::request(
        &binding.broker_socket,
        cos::clawd::wire::Request::build(
            cos::clawd::routes::Command::TaskCancel,
            serde_json::json!({"id":"other-task"}),
        ),
    )
    .await;
    match spoof {
        Ok(response) => assert!(!response.ok, "worker must not gain a broker route"),
        Err(error) => assert!(
            error.contains("Permission denied"),
            "unexpected private broker refusal: {error}"
        ),
    }

    let mcp_name = "hosted-echo";
    let mcp_server = std::env::var("COS_EXTENSION_BOUNDARY_MCP").unwrap();
    let tools = client
        .attach_mcp(cos::agent::tools::mcp::integration::McpServerSpec {
            name: mcp_name.to_string(),
            command: "python3".to_string(),
            args: vec![mcp_server.clone()],
            env: HashMap::new(),
            cwd: PathBuf::from(&mcp_server)
                .parent()
                .map(|path| path.to_string_lossy().into_owned()),
            timeout_secs: 5,
            url: None,
            bearer_env: None,
            package: None,
            provenance: None,
        })
        .await
        .expect("attach hosted MCP");
    let descriptor_digest =
        cos::agent::tools::mcp::integration::sanitized_descriptor_digest_for_test(mcp_name, tools)
            .expect("descriptor digest");
    let audit = cos::extension_host::protocol::McpInvocationAudit {
        policy_identity: "mcp_hosted_echo_ping".to_string(),
        server_identity: mcp_name.to_string(),
        handle_digest: cos::crypto::sha256_hex(b"extension-boundary-handle"),
        descriptor_digest: descriptor_digest.clone(),
        capability_generation: TEST_CAPABILITY_GENERATION.to_string(),
        untrusted_remote_name: cos::audit_policy::text_digest("ping"),
    };
    let value = client
        .call_mcp(
            mcp_name.to_string(),
            "ping".to_string(),
            descriptor_digest,
            None,
            Duration::from_secs(5),
            audit,
        )
        .await
        .expect("call hosted MCP");
    assert!(value.content.iter().any(|item| {
        matches!(
            item,
            cos::agent::tools::mcp::protocol::ContentItem::Text { text } if text == "pong"
        )
    }));
    assert!(client
        .detach_mcp(mcp_name.to_string())
        .await
        .expect("detach hosted MCP"));

    let registration = installed_registration("observer");
    let mut drift = registration.clone();
    drift.manifest_digest = "f".repeat(64);
    let drift_error = client
        .attach_agent_extension(drift)
        .await
        .expect_err("manifest drift must fail");
    assert!(drift_error.contains("registration"), "{drift_error}");

    let binding = client
        .attach_agent_extension(registration.clone())
        .await
        .expect("attach generic Agent extension");
    let refs = Arc::new(cos::agent_extensions::capability_ref::CapabilityReferenceStore::new(1));
    let deadline =
        cos::extension_host::abi::MonotonicDeadlineNs::after(Duration::from_secs(1)).unwrap();
    let reference_context = cos::agent_extensions::capability_ref::ReferenceContext {
        owner_uid: binding.owner_uid,
        session_id: &binding.session_id,
        task_id: &binding.task_id,
        extension_id: &binding.extension_id,
        manifest_digest: &binding.manifest_digest,
        capability_generation: &binding.capability_generation,
        event_id: "normal-event",
        deadline,
    };
    let capability = Cap::new(Verb::SYS_OBSERVE, Scope::name("time"));
    let lease = refs
        .issue_event(
            &reference_context,
            std::slice::from_ref(&capability),
            &[cos::agent_extensions::manifest::ExtensionActionPolicy {
                requested_index: 0,
                tool: "now".to_string(),
                policy_id: "builtin.now/v1".to_string(),
            }],
        )
        .expect("issue capability reference");
    let issued = lease.references().to_vec();
    let result = client
        .send_agent_extension_event(
            "observer".to_string(),
            binding.clone(),
            "normal-event".to_string(),
            deadline,
            cos::extension_host::abi::EventPayload::SessionStart {
                source: "boundary-test".to_string(),
                attended: false,
                delegated: false,
            },
            issued,
        )
        .await
        .expect("observe session start");
    let output: serde_json::Value =
        serde_json::from_str(result.output.as_deref().expect("bounded output")).unwrap();
    assert!(output["broker_env"].is_null(), "{output}");
    assert!(output["registry_env"].is_null(), "{output}");
    assert!(output["secret_env"].is_null(), "{output}");
    assert_eq!(output["network_blocked"], true);
    assert_eq!(output["host_root_absent"], true);
    let action = result
        .proposed_actions
        .first()
        .expect("explicit proposed action");
    lease
        .consume_all(&[
            cos::agent_extensions::capability_ref::ActionReferenceBinding {
                reference: action.capability_ref.clone(),
                action_id: action.action_id.clone(),
                tool: action.tool.clone(),
                policy_id: "builtin.now/v1".to_string(),
                input_digest: cos::crypto::sha256_hex(b"{}"),
                capability,
                operation_digest: cos::crypto::sha256_hex(b"boundary-operation"),
            },
        ])
        .expect("resolve exact reference");

    let mut cross_session_binding = binding.clone();
    cross_session_binding.session_id = "other-session".to_string();
    let cross_session = send_agent_extension_probe(
        &client,
        "observer",
        cross_session_binding,
        "a".repeat(64),
        Duration::from_secs(1),
    )
    .await
    .expect_err("cross-session event must fail");
    assert!(cross_session.contains("binding"), "{cross_session}");

    let downgrade_registration = installed_registration("downgrade");
    let downgrade = client
        .attach_agent_extension(downgrade_registration)
        .await
        .expect_err("protocol downgrade must fail");
    assert!(downgrade.contains("downgrade"), "{downgrade}");

    let steady_registration = installed_registration("steady");
    let steady = client
        .attach_agent_extension(steady_registration)
        .await
        .expect("attach unaffected extension");
    for (id, mode) in [
        ("malformed", "malformed"),
        ("oversize", "oversize"),
        ("hung", "hang"),
        ("crashed", "crash"),
    ] {
        let registration = installed_registration(id);
        let hostile = client
            .attach_agent_extension(registration)
            .await
            .unwrap_or_else(|error| panic!("attach {mode}: {error}"));
        let error = send_agent_extension_probe(
            &client,
            id,
            hostile,
            "b".repeat(64),
            Duration::from_millis(200),
        )
        .await
        .expect_err("hostile extension event must fail");
        assert!(
            error.contains("ABI")
                || error.contains("timed out")
                || error.contains("header")
                || error.contains("body"),
            "{mode}: {error}"
        );
        send_agent_extension_probe(
            &client,
            "steady",
            steady.clone(),
            "c".repeat(64),
            Duration::from_secs(1),
        )
        .await
        .unwrap_or_else(|error| panic!("hostile {mode} affected steady extension: {error}"));
    }

    let forged_registration = installed_registration("forged");
    let forged_binding = client
        .attach_agent_extension(forged_registration)
        .await
        .expect("attach forged-ref probe");
    let forged = send_agent_extension_probe(
        &client,
        "forged",
        forged_binding.clone(),
        "d".repeat(64),
        Duration::from_secs(1),
    )
    .await
    .expect("receive forged proposal");
    let forged_action = forged.proposed_actions.first().expect("forged action");
    assert_eq!(forged_action.capability_ref.handle, "f".repeat(64));
    client
        .detach_agent_extension(
            "forged".to_string(),
            forged_binding,
            cos::extension_host::abi::ShutdownReason::Disabled,
        )
        .await
        .expect("detach forged probe");

    let descendant_registration = installed_registration("descendant");
    let descendant_binding = client
        .attach_agent_extension(descendant_registration)
        .await
        .expect("attach descendant probe");
    send_agent_extension_probe(
        &client,
        "descendant",
        descendant_binding.clone(),
        "e".repeat(64),
        Duration::from_secs(1),
    )
    .await
    .expect("spawn extension descendant");
    client
        .detach_agent_extension(
            "descendant".to_string(),
            descendant_binding,
            cos::extension_host::abi::ShutdownReason::TaskComplete,
        )
        .await
        .expect("detach descendant probe");
    client
        .detach_agent_extension(
            "steady".to_string(),
            steady,
            cos::extension_host::abi::ShutdownReason::TaskComplete,
        )
        .await
        .expect("detach steady extension");
}

fn worker_command(
    identity: &cos::agentd::spawn::WorkerIdentity,
    execution_gid: u32,
    binding_path: &std::path::Path,
    env: &TestEnvironment,
    mode: &str,
) -> tokio::process::Command {
    use std::os::unix::process::CommandExt;
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "hosted_app_and_mcp_lifecycle_is_isolated_and_fail_closed",
            "--nocapture",
        ])
        .env("COS_EXTENSION_BOUNDARY_CHILD", "1")
        .env("COS_EXTENSION_BOUNDARY_MODE", mode)
        .env("COS_EXTENSION_BOUNDARY_BINDING", binding_path)
        .env("COS_EXTENSION_BOUNDARY_MCP", &env.mcp_server)
        .env("HOME", &identity.home)
        .env_remove(LEAK_MARKER)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let uid = identity.uid;
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(execution_gid, execution_gid, execution_gid) != 0
                || libc::setresuid(uid, uid, uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hosted_app_and_mcp_lifecycle_is_isolated_and_fail_closed() {
    if std::env::var("COS_EXTENSION_BOUNDARY_CHILD").as_deref() == Ok("1") {
        worker_child().await;
        return;
    }
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: privileged extension boundary runs in the dedicated sudo test step");
        return;
    }
    assert!(
        std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists(),
        "kernel has no supported cgroup-v2 hierarchy"
    );
    assert!(
        std::path::Path::new("/usr/bin/bwrap").exists(),
        "bubblewrap is required for the supported root boundary"
    );
    let uid = std::env::var("SUDO_UID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1000);
    let identity = cos::agentd::spawn::resolve_identity(uid).expect("resolve task identity");
    std::env::set_var(cos::agentd::spawn::ISOLATED_GROUP_ENV, "nogroup");
    let execution_gid =
        cos::agentd::spawn::resolve_isolated_execution_gid().expect("isolated execution gid");
    let env = TestEnvironment::new(uid, execution_gid).expect("test environment");
    assert!(
        cos::apps::find(env._apps.path(), "echo-app").is_some(),
        "echo App fixture is not discoverable"
    );
    let registration_path = std::env::var("COS_EXTENSION_BOUNDARY_REGISTRATIONS").unwrap();
    let registrations: HashMap<String, cos::extension_host::protocol::AgentExtensionRegistration> =
        serde_json::from_slice(&std::fs::read(registration_path).unwrap()).unwrap();
    let selected = registrations.keys().cloned().collect::<Vec<_>>();
    let installed = cos::agent_extensions::registry::ExtensionRegistry::load_selected(
        &env.extensions,
        &selected,
    );
    assert!(
        installed.registered.contains_key("installed-observer"),
        "root-owned signed installed package did not register: {:?}",
        installed.quarantined
    );
    let agent_extension_receipts = installed
        .registered
        .values()
        .map(|extension| extension.package.verification_receipt().unwrap())
        .collect();
    let binding_path = env._sync.path().join("binding.json");
    let worker = worker_command(&identity, execution_gid, &binding_path, &env, "run")
        .spawn()
        .expect("spawn unprivileged worker harness");
    let worker_pid = worker.id().expect("worker pid");
    let worker_start = process_start(worker_pid).expect("worker start time");

    let extension_identity = cos::extension_host::identity::ExtensionIdentity {
        uid: cos::extension_host::identity::FIRST_UID,
        gid: execution_gid,
        username: "cos-ext-00".to_string(),
    };
    let primary_path = env._runtime.path().join("clawd.sock");
    let _primary_listener =
        std::os::unix::net::UnixListener::bind(&primary_path).expect("primary broker fixture");
    std::fs::set_permissions(&primary_path, std::fs::Permissions::from_mode(0o660))
        .expect("protect primary broker fixture");
    let primary_raw = std::ffi::CString::new(primary_path.to_string_lossy().as_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::chown(primary_raw.as_ptr(), 0, identity.gid) },
        0
    );
    let isolation =
        cos::agentd::spawn::ExecutionIsolation::capture(&primary_path, identity.uid, execution_gid)
            .expect("capture execution isolation");
    let paths = spawn::HostPaths::create(&identity).expect("host paths");
    let listener =
        broker::bind_listener(&paths, extension_identity.uid, execution_gid).expect("listener");
    let cgroup_root = CgroupRootGuard::create();
    let containment = spawn::ContainmentRoot::establish().expect("extension containment");
    let task_id = "extension-host-integration";
    let task_session = "task-session";
    let host_session = "extension-session";
    let expires = cos::agentd::grant::now_ms() + 120_000;
    cos::storage::install_routed_extension_reader(identity.uid, extension_identity.uid)
        .expect("install routed registry reader");
    let mut host = spawn::spawn_host(
        &identity,
        &extension_identity,
        &isolation,
        &containment,
        task_id,
        Some(task_session),
        Some(host_session),
        worker_pid,
        Some(worker_start),
        "0123456789abcdef0123456789abcdef",
        expires,
        TEST_CAPABILITY_GENERATION,
        vec![
            spawn::approve_runtime_path(&identity.home, identity.uid).expect("approve owner home"),
            spawn::approve_runtime_path(env._apps.path(), identity.uid).expect("approve App root"),
            spawn::approve_runtime_path(env._runtime.path(), identity.uid)
                .expect("approve test runtime"),
            spawn::approve_runtime_path(&env.extensions, identity.uid)
                .expect("approve installed extension root"),
        ],
        agent_extension_receipts,
        paths,
    )
    .expect("spawn host");

    let caps = CapSet::from_caps([Cap::new(Verb::AGENT_INVOKE, Scope::name("echo-app"))]);
    cos::proc::register_session_for_owner(
        cos::proc::SessionInfo {
            session_id: host_session.to_string(),
            pid: host.pid,
            command: vec!["claw-extension-host".to_string()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some(cos::extension_host::protocol::EXTENSION_HOST_GROUP.to_string()),
            parent: Some(task_session.to_string()),
            workdir: Some(identity.home.to_string_lossy().into_owned()),
            exit_code: None,
            ended_at: None,
            tier: None,
            scope: None,
            priority: None,
            caps: Some(caps),
            transient_caps: None,
            role: Some("worker".to_string()),
            app_id: None,
            pending_bind: false,
            start_time_ticks: host.start_time_ticks,
            client: cos::session::SessionClient::new(
                cos::session::SessionSource::BrokerTask,
                false,
                true,
            ),
        },
        uid,
    )
    .expect("register host session");
    let lease = Arc::new(broker::ExtensionLease::new(
        task_id.to_string(),
        Some(task_session.to_string()),
        Some(host_session.to_string()),
        uid,
        extension_identity.uid,
        execution_gid,
        worker_pid,
        Some(worker_start),
        host.pid,
        host.start_time_ticks,
        expires,
    ));
    let broker_task = tokio::spawn(broker::serve(
        listener,
        lease.clone(),
        cos::clawd::state::DaemonState::try_new().expect("daemon state"),
        cos::clawd::transport::limits::Admission::new(
            cos::clawd::transport::limits::Limits::default(),
        ),
    ));
    let host_environment =
        String::from_utf8_lossy(&std::fs::read(format!("/proc/{}/environ", host.pid)).unwrap())
            .into_owned();
    assert!(
        host_environment.contains(env._apps.path().to_string_lossy().as_ref()),
        "host environment omitted COS_APPS_DIR: {host_environment:?}"
    );
    std::fs::write(&binding_path, serde_json::to_vec(&host.binding).unwrap()).unwrap();
    make_owner_writable(&binding_path, uid, execution_gid).expect("publish worker binding");

    let output = tokio::time::timeout(Duration::from_secs(90), worker.wait_with_output())
        .await
        .expect("worker boundary timed out")
        .expect("wait worker boundary");
    let audit =
        std::fs::read_to_string(env._data.path().join("clawd/audit.jsonl")).unwrap_or_default();
    assert!(
        output.status.success(),
        "worker stdout:\n{}\nworker stderr:\n{}\naudit:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        audit,
    );
    assert_eq!(proc_field(host.pid, "NoNewPrivs:").as_deref(), Some("1"));
    assert_eq!(process_session(host.pid), Some(host.pid));
    let descriptors = std::fs::read_dir(format!("/proc/{}/fd", host.pid))
        .unwrap()
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .collect::<Vec<_>>();
    assert!(!descriptors.contains(&env.leaked_path));

    let replay = worker_command(&identity, execution_gid, &binding_path, &env, "replay")
        .output()
        .await
        .expect("replacement worker replay probe");
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );

    lease.close();
    broker_task.abort();
    cos::proc::deregister_session_for_owner(host_session, uid);
    cos::storage::remove_routed_extension_reader(identity.uid, extension_identity.uid)
        .expect("remove routed registry reader");
    host.cgroup.cleanup().await.expect("clean host containment");
    let _ = host.child.wait().await;
    host.cleanup_private_mounts()
        .expect("clean private tmp mounts");
    let task_path = host.paths.dir.clone();
    host.paths.cleanup().expect("clean host paths");
    assert!(!task_path.exists(), "task path survived verified cleanup");
    drop(cgroup_root);
}
