//! Real process and channel checks for the task-owned extension host.

#![cfg(all(unix, target_os = "linux"))]

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cos::caps::{Cap, CapSet, Scope, Verb};
use cos::extension_host::{broker, client, spawn};

const HOST_BIN: &str = env!("CARGO_BIN_EXE_claw-extension-host");
const LEAK_MARKER: &str = "COS_EXTENSION_TEST_BROKER_SECRET";
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
                "description": "Return pong.",
                "inputSchema": {"type": "object", "additionalProperties": False},
            }]
        }
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "pong"}], "isError": False}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
"#;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct TestEnvironment {
    _lock: std::sync::MutexGuard<'static, ()>,
    _runtime: tempfile::TempDir,
    _proc_data: tempfile::TempDir,
    _data: tempfile::TempDir,
    _apps: tempfile::TempDir,
    mcp_server: PathBuf,
    leaked_path: PathBuf,
    leaked_fd: i32,
}

impl TestEnvironment {
    fn new() -> Option<Self> {
        let lock = env_lock();
        let uid = unsafe { libc::geteuid() } as u32;
        if uid == 0 {
            return None;
        }
        let runtime = tempfile::tempdir().ok()?;
        let proc_data = tempfile::tempdir().ok()?;
        let data = tempfile::tempdir().ok()?;
        let apps = tempfile::tempdir().ok()?;
        std::env::set_var("COS_EXTENSION_HOST_BIN", HOST_BIN);
        std::env::set_var("COS_RUNTIME_DIR", runtime.path());
        std::env::set_var("COS_PROC_DATA_DIR", proc_data.path());
        std::env::set_var("COS_DATA_DIR", data.path());
        std::env::set_var("COS_USER_DATA_DIR", data.path());
        std::env::set_var("COS_APPS_DIR", apps.path());
        std::env::set_var(LEAK_MARKER, "broker-only-value");

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
        Some(Self {
            _lock: lock,
            _runtime: runtime,
            _proc_data: proc_data,
            _data: data,
            _apps: apps,
            mcp_server,
            leaked_path,
            leaked_fd,
        })
    }
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
            LEAK_MARKER,
        ] {
            std::env::remove_var(key);
        }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hosted_app_and_mcp_lifecycle_is_isolated_and_fail_closed() {
    let Some(env) = TestEnvironment::new() else {
        eprintln!("skipping: extension host test needs an unprivileged Unix account");
        return;
    };
    let uid = unsafe { libc::geteuid() } as u32;
    let identity = cos::agentd::spawn::resolve_identity(uid).expect("identity");
    let worker_pid = std::process::id();
    let worker_start = process_start(worker_pid);
    let execution_gid = match cos::agentd::spawn::resolve_isolated_execution_gid() {
        Ok(gid) => gid,
        Err(error) => {
            eprintln!("skipping: isolated execution group unavailable: {error}");
            return;
        }
    };
    let paths = spawn::HostPaths::create(&identity, execution_gid).expect("paths");
    let listener =
        broker::bind_listener(&paths.broker_socket, identity.uid, execution_gid).expect("listener");
    let isolation = match cos::agentd::spawn::ExecutionIsolation::capture(
        &paths.broker_socket,
        identity.uid,
        execution_gid,
    ) {
        Ok(isolation) => isolation,
        Err(error) => {
            eprintln!("skipping: root broker socket fixture unavailable: {error}");
            return;
        }
    };
    let containment = match spawn::ContainmentRoot::establish() {
        Ok(containment) => containment,
        Err(error) => {
            eprintln!("skipping: mandatory extension containment unavailable: {error}");
            return;
        }
    };
    let task_id = "extension-host-integration";
    let task_session = "task-session";
    let host_session = "extension-session";
    let nonce = "0123456789abcdef0123456789abcdef";
    let expires = cos::agentd::grant::now_ms() + 120_000;
    let mut host = spawn::spawn_host(
        &identity,
        &isolation,
        &containment,
        task_id,
        Some(task_session),
        Some(host_session),
        worker_pid,
        worker_start,
        nonce,
        expires,
        paths,
    )
    .expect("spawn host");

    let caps = CapSet::from_caps([Cap::new(Verb::AGENT_INVOKE, Scope::name("echo-app"))]);
    cos::proc::register_session(cos::proc::SessionInfo {
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
    })
    .expect("register host session");

    let lease = Arc::new(broker::ExtensionLease::new(
        task_id.to_string(),
        Some(task_session.to_string()),
        Some(host_session.to_string()),
        uid,
        unsafe { libc::getegid() },
        worker_pid,
        worker_start,
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

    let installed = match client::install(host.binding.clone()).await {
        Ok(installed) => installed,
        Err(error) => {
            let status = host.child.try_wait().ok().flatten();
            let mut stderr = String::new();
            if let Some(mut pipe) = host.child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = pipe.read_to_string(&mut stderr).await;
            }
            panic!("host ready: {error}; status={status:?}; stderr={stderr}");
        }
    };
    let client = client::current().expect("installed client");

    assert_eq!(proc_field(host.pid, "NoNewPrivs:").as_deref(), Some("1"));
    assert_eq!(process_session(host.pid), Some(host.pid));
    if let Ok(descriptors) = std::fs::read_dir(format!("/proc/{}/fd", host.pid)) {
        let descriptors = descriptors
            .flatten()
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .collect::<Vec<_>>();
        assert!(
            !descriptors.contains(&env.leaked_path),
            "host inherited broker fd: {descriptors:?}"
        );
    }
    if let Ok(environ) = std::fs::read(format!("/proc/{}/environ", host.pid)) {
        let environ = String::from_utf8_lossy(&environ);
        assert!(!environ.contains(LEAK_MARKER));
        assert!(!environ.contains("CLAWD_SOCKET="));
        assert!(environ.contains("COS_EXTENSION_BROKER_SOCKET="));
    }

    let output = client
        .run_app("echo-app".to_string(), "echo".to_string(), Vec::new())
        .await
        .expect("hosted App call")
        .expect("App output");
    assert!(output.contains("\"command\": \"echo\""), "{output}");
    let output: serde_json::Value = serde_json::from_str(&output).expect("App JSON");
    assert!(output["env_leak"].is_null(), "{output}");
    assert!(
        !output["fds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == env.leaked_path.to_str()),
        "{output}"
    );

    assert_eq!(
        client
            .open_app("echo-app".to_string())
            .await
            .expect("open hosted App session"),
        1
    );
    let app_call = match client
        .call_app(
            "echo-app".to_string(),
            "ping".to_string(),
            serde_json::json!({}),
            Duration::from_secs(5),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => {
            let audit = std::fs::read_to_string(env._data.path().join("clawd/audit.jsonl"))
                .unwrap_or_default();
            panic!("call hosted App session: {error}\naudit:\n{audit}");
        }
    };
    assert!(app_call.content.iter().any(|item| {
        matches!(
            item,
            cos::agent::tools::mcp::protocol::ContentItem::Text { text }
                if text == "pong"
        )
    }));
    assert!(client
        .close_app("echo-app".to_string())
        .await
        .expect("close hosted App session"));

    let spoof = cos::clawd::client::request(
        &host.binding.broker_socket,
        cos::clawd::wire::Request::build(
            cos::clawd::routes::Command::TaskCancel,
            serde_json::json!({"id":"other-task"}),
        ),
    )
    .await
    .expect("private broker response");
    assert!(!spoof.ok, "worker must not gain a broker route");

    let mcp_name = "hosted-echo";
    let tools = client
        .attach_mcp(cos::agent::tools::mcp::integration::McpServerSpec {
            name: mcp_name.to_string(),
            command: "python3".to_string(),
            args: vec![env.mcp_server.to_string_lossy().into_owned()],
            env: HashMap::new(),
            cwd: env
                .mcp_server
                .parent()
                .map(|path| path.to_string_lossy().into_owned()),
            timeout_secs: 5,
            url: None,
            bearer_env: None,
        })
        .await
        .expect("attach hosted MCP");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "ping");
    let value = client
        .call_mcp(
            mcp_name.to_string(),
            "ping".to_string(),
            None,
            Duration::from_secs(5),
        )
        .await
        .expect("call hosted MCP");
    assert!(
        value.content.iter().any(|item| {
            matches!(
                item,
                cos::agent::tools::mcp::protocol::ContentItem::Text { text }
                    if text == "pong"
            )
        }),
        "{value:?}"
    );
    assert!(client
        .detach_mcp(mcp_name.to_string())
        .await
        .expect("detach hosted MCP"));

    let timeout = client
        .attach_mcp(cos::agent::tools::mcp::integration::McpServerSpec {
            name: "hung".to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "sleep 60".to_string()],
            env: HashMap::new(),
            cwd: None,
            timeout_secs: 1,
            url: None,
            bearer_env: None,
        })
        .await
        .expect_err("hung MCP attach must time out");
    assert!(timeout.contains("timed out"), "{timeout}");

    let host_pid = host.pid;
    host.child.start_kill().expect("kill host");
    host.child.wait().await.expect("reap crashed host");
    let crash = client
        .run_app("echo-app".to_string(), "echo".to_string(), Vec::new())
        .await
        .expect_err("crashed host must fail only the extension call");
    assert!(
        crash.contains("connect extension host") || crash.contains("different process"),
        "{crash}"
    );
    assert!(!cos::proc::is_pid_alive(host_pid));

    drop(installed);
    lease.close();
    broker_task.abort();
    cos::proc::deregister_session(host_session);
    host.cgroup.cleanup().await.expect("clean host containment");
    host.paths.cleanup();
}
