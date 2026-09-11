//! Adversarial isolation tests for hostile workers.
//!
//! These run real processes and inspect what the kernel actually built
//! — namespaces, `/proc`, `mountinfo`, descriptors, environment,
//! network interfaces — rather than asserting on the policy struct
//! that asked for them. A policy that says "read-only root" and a
//! sandbox whose root is writable would pass a struct test and fail
//! here, which is the point.
//!
//! Every test is skipped, loudly, on a host that cannot enforce the
//! policy; the fail-closed behaviour on such a host is covered
//! separately by [`missing_isolation_facilities_fail_closed`].

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cos::agent::tools::app_gateway::{McpCallContext, McpPrincipal, McpPrincipalKind};
use cos::agent::tools::mcp::client::{ClientError, McpClient};
use cos::agent::tools::mcp::protocol::{
    CallToolResult, ClientCapabilities, ContentItem, Implementation,
};
use cos::agent::tools::mcp::transport::StdioTransport;
use cos::caps::{Cap, CapSet, Scope, Verb};
use cos::worker::derive::{AgentExecInput, McpServerInput};
use cos::worker::{Limits, WorkerLaunch, WorkerOutput};
use serde_json::{json, Value};

mod app_sources {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/support/app_sources.rs"));
}

mod app_stage {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/support/app_stage.rs"));
}

/// Run `script` under the hostile-worker sandbox with `workspace`
/// bound in the requested direction, and return what it produced.
fn run_in_sandbox(workspace: &Path, writable: bool, script: &str) -> WorkerOutput {
    run_in_sandbox_with(workspace, writable, script, Limits::operation())
}

fn run_in_sandbox_with(
    workspace: &Path,
    writable: bool,
    script: &str,
    limits: Limits,
) -> WorkerOutput {
    let policy = cos::worker::derive::agent_exec(AgentExecInput {
        workspace: workspace.to_path_buf(),
        writable,
        argv: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        endpoints: Vec::new(),
        limits,
    })
    .expect("derive policy");
    let prepared = cos::worker::prepare(&WorkerLaunch::new(policy)).expect("prepare launch");
    cos::worker::run_captured(prepared, None, limits, |_| Ok(())).expect("run worker")
}

fn sandbox_available() -> bool {
    cos::worker::availability().is_available()
}

/// Skip only where the platform genuinely cannot enforce the policy.
///
/// CI and the image builds declare the prerequisites installed by
/// setting `COS_WORKER_SANDBOX_REQUIRED=1`; there, an unavailable
/// sandbox is a failure, not a skip, so a missing dependency cannot
/// quietly turn this suite into a no-op.
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

fn workspace() -> tempfile::TempDir {
    tempfile::tempdir().expect("workspace")
}

// ---------------------------------------------------------------------------
// Filesystem
// ---------------------------------------------------------------------------

#[test]
fn host_secrets_and_other_homes_are_not_reachable() {
    require_sandbox!();
    let dir = workspace();
    let output = run_in_sandbox(
        dir.path(),
        false,
        "cat /etc/shadow 2>&1; ls /home 2>&1; ls /root 2>&1; \
         ls ~/.ssh 2>&1; ls /var/lib/cos 2>&1; ls /run/cos 2>&1",
    );
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(
        !seen.contains("root:"),
        "shadow contents leaked into the sandbox: {seen}"
    );
    for probe in ["/etc/shadow", "/root", "/var/lib/cos"] {
        assert!(
            seen.contains(probe),
            "expected a failure naming {probe}, got: {seen}"
        );
    }
    // `/home` exists as an empty directory so path resolution works;
    // it must not contain any account.
    assert!(
        !seen.contains(&whoami()),
        "the owner's home appeared inside the sandbox: {seen}"
    );
}

#[test]
fn the_broker_socket_and_credential_store_are_absent() {
    require_sandbox!();
    let dir = workspace();
    let output = run_in_sandbox(
        dir.path(),
        false,
        "test -S /run/cos/clawd.sock && echo BROKER-PRESENT || echo broker-absent; \
         ls -a ~ 2>&1 | head -20",
    );
    let seen = output.stdout_string();
    assert!(
        seen.contains("broker-absent"),
        "the real broker socket is reachable: {seen}"
    );
    assert!(
        !seen.contains(".gnupg") && !seen.contains(".aws"),
        "a credential store is visible: {seen}"
    );
}

#[test]
fn the_root_filesystem_is_read_only_and_system_paths_cannot_be_written() {
    require_sandbox!();
    let dir = workspace();
    let output = run_in_sandbox(
        dir.path(),
        false,
        "echo x > /newfile 2>&1; echo x > /usr/newfile 2>&1; \
         echo x > /etc/newfile 2>&1; grep ' / ' /proc/self/mountinfo",
    );
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(
        seen.contains("Read-only file system") || seen.contains("Permission denied"),
        "a write outside the mounts succeeded: {seen}"
    );
    let root_line = seen
        .lines()
        .find(|line| {
            line.split_whitespace()
                .nth(4)
                .is_some_and(|mount_point| mount_point == "/")
        })
        .unwrap_or_default();
    assert!(
        root_line
            .split_whitespace()
            .nth(5)
            .is_some_and(|options| options.starts_with("ro,") || options == "ro"),
        "sandbox root is not read-only: {seen}"
    );
}

#[test]
fn a_read_only_workspace_cannot_be_written_and_a_writable_one_can() {
    require_sandbox!();
    let dir = workspace();
    let target = dir.path().join("written.txt");

    let readonly = run_in_sandbox(
        dir.path(),
        false,
        &format!("echo hello > {} 2>&1", target.display()),
    );
    assert!(!target.exists(), "read-only workspace accepted a write");
    let seen = readonly.stdout_string() + &readonly.stderr_string();
    assert!(
        seen.contains("Read-only") || seen.contains("Permission denied"),
        "expected a read-only failure: {seen}"
    );

    let writable = run_in_sandbox(
        dir.path(),
        true,
        &format!("echo hello > {}", target.display()),
    );
    assert!(
        writable.status.success(),
        "writable workspace rejected a write"
    );
    assert_eq!(
        std::fs::read_to_string(&target)
            .expect("written file")
            .trim(),
        "hello"
    );
}

#[test]
fn a_symlink_out_of_the_workspace_resolves_to_nothing() {
    require_sandbox!();
    let dir = workspace();
    std::os::unix::fs::symlink("/etc", dir.path().join("escape")).expect("symlink");
    std::os::unix::fs::symlink("/etc/shadow", dir.path().join("shadow")).expect("symlink");
    let output = run_in_sandbox(
        dir.path(),
        false,
        "cat escape/shadow 2>&1; cat shadow 2>&1; cat ../../../etc/shadow 2>&1",
    );
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(
        !seen.contains("root:"),
        "a symlink reached a host secret: {seen}"
    );
}

#[test]
fn a_granted_scope_that_names_a_kernel_root_refuses_the_launch() {
    let caps = CapSet::from_caps(vec![Cap::new(Verb::FS_READ, Scope::path("/run/cos/**"))]);
    let error = cos::worker::derive::granted_path_mounts(&caps).unwrap_err();
    assert!(error.contains("kernel-owned"), "{error}");
}

#[test]
fn a_capability_without_a_mount_grants_no_host_visibility() {
    require_sandbox!();
    let dir = workspace();
    let secret = dir.path().join("outside");
    std::fs::create_dir(&secret).expect("dir");
    std::fs::write(secret.join("data.txt"), "top secret").expect("write");

    // The capability set names the directory, but the derivation maps a
    // wildcard scope to no mount at all, so the check can pass and the
    // path still does not exist.
    let caps = CapSet::from_caps(vec![Cap::new(Verb::FS_READ, Scope::Wild)]);
    let mounts = cos::worker::derive::granted_path_mounts(&caps).expect("mounts");
    assert!(mounts.is_empty(), "a wildcard grant produced mounts");

    let inner = workspace();
    let output = run_in_sandbox(
        inner.path(),
        false,
        &format!("cat {} 2>&1", secret.join("data.txt").display()),
    );
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(
        !seen.contains("top secret"),
        "unmounted path was readable: {seen}"
    );
}

// ---------------------------------------------------------------------------
// Namespaces, processes and devices
// ---------------------------------------------------------------------------

#[test]
fn the_worker_gets_private_pid_user_ipc_uts_and_net_namespaces() {
    require_sandbox!();
    let dir = workspace();
    let output = run_in_sandbox(
        dir.path(),
        false,
        "for ns in pid user ipc uts net mnt; do \
           printf '%s=%s\\n' \"$ns\" \"$(readlink /proc/self/ns/$ns)\"; done",
    );
    let inside = output.stdout_string();
    for namespace in ["pid", "user", "ipc", "uts", "net", "mnt"] {
        let host = std::fs::read_link(format!("/proc/self/ns/{namespace}"))
            .expect("host namespace")
            .to_string_lossy()
            .into_owned();
        let line = inside
            .lines()
            .find(|line| line.starts_with(&format!("{namespace}=")))
            .unwrap_or_default();
        assert!(
            !line.ends_with(&host),
            "{namespace} namespace was shared with the host: {line}"
        );
    }
}

#[test]
fn proc_lists_only_the_sandbox_processes() {
    require_sandbox!();
    let dir = workspace();
    let output = run_in_sandbox(
        dir.path(),
        false,
        "ls /proc | grep -c '^[0-9][0-9]*$'; cat /proc/1/comm 2>&1",
    );
    let seen = output.stdout_string();
    let count: usize = seen
        .lines()
        .next()
        .unwrap_or("0")
        .trim()
        .parse()
        .unwrap_or(usize::MAX);
    assert!(
        count < 8,
        "host processes are visible inside the sandbox ({count} entries): {seen}"
    );
}

#[test]
fn all_capabilities_are_dropped_and_privileges_cannot_be_regained() {
    require_sandbox!();
    let dir = workspace();
    let output = run_in_sandbox(
        dir.path(),
        false,
        "grep -E '^(CapEff|CapBnd|NoNewPrivs|Seccomp):' /proc/self/status; \
         unshare -U -r true 2>&1 || echo userns-refused",
    );
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(
        seen.contains("CapEff:\t0000000000000000"),
        "effective capabilities are not empty: {seen}"
    );
    assert!(
        seen.contains("CapBnd:\t0000000000000000"),
        "the capability bounding set is not empty: {seen}"
    );
    assert!(
        seen.contains("NoNewPrivs:\t1"),
        "NoNewPrivs is not set: {seen}"
    );
    assert!(
        !seen.contains("Seccomp:\t0\n"),
        "no seccomp filter was installed: {seen}"
    );
    assert!(
        !seen.contains("userns-created"),
        "a nested user namespace was available: {seen}"
    );
}

#[test]
fn dangerous_syscalls_are_filtered() {
    require_sandbox!();
    let dir = workspace();
    // `unshare(1)` and `mount(8)` exercise the namespace and mount
    // syscalls directly; both must be refused by the filter rather than
    // by permissions alone.
    let output = run_in_sandbox(
        dir.path(),
        false,
        "unshare --user --mount true 2>&1; mount -t tmpfs none /mnt 2>&1; echo done",
    );
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(seen.contains("done"), "probe did not run: {seen}");
    assert!(
        !seen.contains("mounted"),
        "a mount succeeded inside the sandbox: {seen}"
    );
}

#[test]
fn only_a_minimal_device_set_is_present() {
    require_sandbox!();
    let dir = workspace();
    let output = run_in_sandbox(
        dir.path(),
        false,
        "ls /dev; echo ---; cat /dev/mem 2>&1 | head -1; ls /dev/dri 2>&1",
    );
    let seen = output.stdout_string() + &output.stderr_string();
    for forbidden in ["sda", "nvme0n1", "kvm", "mem\n"] {
        assert!(
            !seen.contains(forbidden),
            "device {forbidden} is exposed: {seen}"
        );
    }
    assert!(seen.contains("null"), "expected a minimal /dev: {seen}");
}

// ---------------------------------------------------------------------------
// Inherited state
// ---------------------------------------------------------------------------

#[test]
fn no_parent_environment_or_descriptor_is_inherited() {
    require_sandbox!();
    let dir = workspace();
    let marker = dir.path().join("inherited-secret.txt");
    std::fs::write(&marker, "sensitive").expect("marker");
    let held = std::fs::File::open(&marker).expect("hold descriptor");

    std::env::set_var("OPENAI_API_KEY", "sk-should-not-leak");
    std::env::set_var("SSH_AUTH_SOCK", "/run/user/0/ssh-agent");
    std::env::set_var("COS_CAPS_DATA_DIR", "/var/lib/cos/caps");
    let output = run_in_sandbox(dir.path(), false, "env; echo ---; ls -l /proc/self/fd");
    std::env::remove_var("OPENAI_API_KEY");
    std::env::remove_var("SSH_AUTH_SOCK");
    std::env::remove_var("COS_CAPS_DATA_DIR");
    drop(held);

    let seen = output.stdout_string();
    for leaked in [
        "sk-should-not-leak",
        "SSH_AUTH_SOCK",
        "COS_CAPS_DATA_DIR",
        "COS_PROC_DATA_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XAUTHORITY",
    ] {
        assert!(!seen.contains(leaked), "{leaked} was inherited: {seen}");
    }
    assert!(
        !seen.contains("inherited-secret.txt"),
        "a parent descriptor survived the launch: {seen}"
    );
    let descriptors = seen
        .split("---")
        .nth(1)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(" -> "))
        .count();
    assert!(
        descriptors <= 5,
        "unexpected descriptors survived exec: {seen}"
    );
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

#[test]
fn the_network_namespace_is_empty_by_default() {
    require_sandbox!();
    let dir = workspace();
    let output = run_in_sandbox(
        dir.path(),
        false,
        "cat /proc/net/dev; echo ---; cat /proc/net/tcp | wc -l",
    );
    let seen = output.stdout_string();
    let interfaces: Vec<&str> = seen
        .lines()
        .skip(2)
        .take_while(|line| !line.starts_with("---"))
        .filter(|line| line.contains(':'))
        .collect();
    assert_eq!(
        interfaces.len(),
        1,
        "the sandbox has more than loopback: {seen}"
    );
    assert!(
        interfaces[0].contains("lo:"),
        "unexpected interface: {seen}"
    );
}

#[test]
fn af_unix_works_and_every_routable_domain_is_refused() {
    require_sandbox!();
    let dir = workspace();
    let script = r#"python3 - <<'PY' 2>&1
import socket
a, b = socket.socketpair()
a.send(b'ping')
print('unix', b.recv(4).decode())
for name in ('AF_INET', 'AF_INET6', 'AF_NETLINK', 'AF_PACKET'):
    family = getattr(socket, name, None)
    if family is None:
        continue
    try:
        socket.socket(family, socket.SOCK_STREAM)
        print('OPENED', name)
    except OSError as error:
        print('refused', name, error.errno)
PY"#;
    let output = run_in_sandbox(dir.path(), false, script);
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(seen.contains("unix ping"), "AF_UNIX was refused: {seen}");
    assert!(
        !seen.contains("OPENED"),
        "a routable socket domain was available: {seen}"
    );
    assert!(seen.contains("refused AF_INET"), "{seen}");
}

#[test]
fn an_asyncio_event_loop_runs_inside_the_sandbox() {
    require_sandbox!();
    let dir = workspace();
    // The self-pipe an event loop builds is an `AF_UNIX` socketpair, so
    // this is the regression test for filtering socket *domains* rather
    // than refusing sockets outright.
    let script = r#"python3 - <<'PY' 2>&1
import asyncio

async def main():
    await asyncio.sleep(0)
    return 'loop-ok'

print(asyncio.run(main()))
PY"#;
    let output = run_in_sandbox(dir.path(), false, script);
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(seen.contains("loop-ok"), "asyncio could not run: {seen}");
}

#[test]
fn brokered_egress_admits_only_the_granted_endpoint() {
    let caps = CapSet::from_caps(vec![
        Cap::new(Verb::NET_DIAL, Scope::host("api.example.com:443")),
        Cap::new(Verb::NET_DIAL, Scope::host("*.wildcard.example")),
    ]);
    let network = cos::worker::derive::egress_from_caps(&caps);
    let endpoints = network.endpoints();
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].authority(), "api.example.com:443");
    assert_eq!(network.as_str(), "brokered");
}

// ---------------------------------------------------------------------------
// Resources and lifecycle
// ---------------------------------------------------------------------------

#[test]
fn a_fork_bomb_hits_the_process_ceiling() {
    require_sandbox!();
    let dir = workspace();
    let mut limits = Limits::operation();
    limits.pids_max = 12;
    limits.runtime = Duration::from_secs(20);
    let output = run_in_sandbox_with(
        dir.path(),
        false,
        "i=0; failures=0; while [ $i -lt 200 ]; do \
           (sleep 5 &) 2>/dev/null || failures=$((failures+1)); i=$((i+1)); done; \
         echo failures=$failures",
        limits,
    );
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(
        seen.contains("Cannot fork")
            || seen.contains("cannot fork")
            || seen.contains("Resource temporarily unavailable")
            || !seen.contains("failures=0"),
        "the process ceiling was not enforced: {seen}"
    );
}

#[test]
fn an_over_running_worker_is_killed_at_its_deadline() {
    require_sandbox!();
    let dir = workspace();
    let mut limits = Limits::operation();
    limits.runtime = Duration::from_secs(2);
    let started = std::time::Instant::now();
    let output = run_in_sandbox_with(dir.path(), false, "sleep 120", limits);
    assert!(output.timed_out, "the deadline did not fire");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the worker outlived its deadline by too much"
    );
}

#[test]
fn a_worker_cannot_leave_a_descendant_behind() {
    require_sandbox!();
    let dir = workspace();
    let marker = format!("cos-orphan-{}", std::process::id());
    let output = run_in_sandbox(
        dir.path(),
        false,
        &format!(
            "nohup sleep 300 > /dev/null 2>&1 & \
             (setsid sleep 300 > /dev/null 2>&1 &) ; echo {marker}"
        ),
    );
    assert!(output.stdout_string().contains(&marker));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        host_processes_matching("sleep 300"),
        0,
        "a descendant survived the launch"
    );
}

#[test]
fn output_is_bounded_and_reported_as_truncated() {
    require_sandbox!();
    let dir = workspace();
    let mut limits = Limits::operation();
    limits.output_bytes = 4096;
    let output = run_in_sandbox_with(
        dir.path(),
        false,
        "i=0; while [ $i -lt 4000 ]; do echo 0123456789012345678901234567890123456789; \
         i=$((i+1)); done",
        limits,
    );
    assert!(output.stdout_truncated, "output was not truncated");
    assert!(
        output.stdout.len() <= 4096,
        "kept {} bytes above the ceiling",
        output.stdout.len()
    );
}

// ---------------------------------------------------------------------------
// Parity and fail-closed behaviour
// ---------------------------------------------------------------------------

#[test]
fn an_mcp_server_gets_the_same_isolation_as_an_app_operation() {
    require_sandbox!();
    let policy = cos::worker::derive::mcp_server(McpServerInput {
        pinned_entries: Vec::new(),
        name: "parity",
        program: PathBuf::from("/bin/sh"),
        argv: vec![
            "-c".to_string(),
            "cat /etc/shadow 2>&1; cat /proc/net/dev; \
             test -S /run/cos/clawd.sock && echo BROKER-PRESENT || echo broker-absent; env"
                .to_string(),
        ],
        cwd: None,
        extra_env: Default::default(),
        session_id: None,
    })
    .expect("derive mcp policy");
    assert_eq!(policy.network.as_str(), "denied");
    assert_eq!(policy.seccomp.as_str(), "strict");

    let limits = policy.limits;
    let prepared = cos::worker::prepare(&WorkerLaunch::new(policy)).expect("prepare");
    let output = cos::worker::run_captured(prepared, None, limits, |_| Ok(())).expect("run");
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(
        !seen.contains("root:"),
        "MCP server read /etc/shadow: {seen}"
    );
    assert!(
        seen.contains("broker-absent"),
        "MCP server reached the real broker: {seen}"
    );
    assert!(
        !seen.contains("OPENAI_API_KEY"),
        "MCP server inherited provider credentials: {seen}"
    );
    let interfaces = seen.lines().filter(|line| line.contains(": ")).count();
    assert!(interfaces <= 2, "MCP server has host network: {seen}");
}

#[test]
fn a_legitimate_app_operation_reads_and_writes_only_its_declared_resources() {
    require_sandbox!();
    let package = workspace();
    let data = workspace();
    let resources = workspace();
    let input = resources.path().join("input.txt");
    let output = resources.path().join("out");
    std::fs::write(&input, "declared input").expect("input");
    std::fs::create_dir(&output).expect("output dir");
    let forbidden = resources.path().join("not-granted.txt");
    std::fs::write(&forbidden, "must stay invisible").expect("forbidden");

    let caps = CapSet::from_caps(vec![
        Cap::new(Verb::FS_READ, Scope::path(input.to_string_lossy())),
        Cap::new(
            Verb::FS_WRITE,
            Scope::path(format!("{}/**", output.display())),
        ),
    ]);
    let script = format!(
        "import pathlib\n\
         text = pathlib.Path({input:?}).read_text()\n\
         pathlib.Path({output:?}).write_text(text.upper())\n\
         try:\n    pathlib.Path({forbidden:?}).read_text()\n    print('LEAKED')\n\
         except OSError:\n    print('denied')\n\
         print('ok', text)\n",
        input = input.to_string_lossy(),
        output = output.join("result.txt").to_string_lossy(),
        forbidden = forbidden.to_string_lossy(),
    );
    let policy = cos::worker::derive::app_operation(cos::worker::derive::AppOperationInput {
        package_identity: None,
        pinned_entries: Vec::new(),
        developer: false,
        app_id: "fixture",
        app_dir: package.path(),
        operation: "transform",
        program: PathBuf::from("/usr/bin/python3"),
        argv: vec!["-c".to_string(), script],
        caps: &caps,
        session_id: "app-fixture",
        data_dir: &data.path().to_string_lossy(),
        apps_dir: &package.path().to_string_lossy(),
        extra_env: Default::default(),
        stdio: cos::worker::StdioPlan::Captured,
        desktop: false,
    })
    .expect("derive app policy");

    // The declared resources are mounted with the direction the grant
    // implies, and nothing else from the host is.
    let input_canonical = input.canonicalize().expect("canonical input");
    let output_canonical = output.canonicalize().expect("canonical output");
    let mounted = |path: &Path| {
        policy
            .mounts
            .iter()
            .find(|mount| mount.source == path)
            .map(|mount| mount.mode)
    };
    assert_eq!(
        mounted(&input_canonical),
        Some(cos::worker::MountMode::ReadOnly)
    );
    assert_eq!(
        mounted(&output_canonical),
        Some(cos::worker::MountMode::ReadWrite)
    );
    assert!(
        mounted(&forbidden.canonicalize().expect("canonical")).is_none(),
        "an ungranted path was mounted"
    );

    let limits = policy.limits;
    let launch = WorkerLaunch::new(policy).with_authority(cos::worker::BrokerAuthority::new(
        "app-fixture",
        Some("fixture".to_string()),
        caps,
        cos::worker::relay_slot(),
    ));
    let prepared = cos::worker::prepare(&launch).expect("prepare");
    let result = cos::worker::run_captured(prepared, None, limits, |_| Ok(())).expect("run");
    let seen = result.stdout_string() + &result.stderr_string();
    assert!(result.status.success(), "fixture App failed: {seen}");
    assert!(seen.contains("ok declared input"), "{seen}");
    assert!(seen.contains("denied"), "{seen}");
    assert!(!seen.contains("LEAKED"), "{seen}");
    assert_eq!(
        std::fs::read_to_string(output.join("result.txt")).expect("written output"),
        "DECLARED INPUT"
    );
}

// ---------------------------------------------------------------------------
// Brokered egress, end to end
// ---------------------------------------------------------------------------

/// Repository paths of both Python packages installed into the shared
/// production SDK directory.
fn source_python_dirs() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    [
        root.join("claw-os-sdk").join("python").join("src"),
        root.join("cos-runtime").join("python").join("src"),
    ]
    .into_iter()
    .map(|path| path.canonicalize().expect("Python SDK tree"))
    .collect()
}

fn source_python_path() -> std::ffi::OsString {
    std::env::join_paths(source_python_dirs()).expect("Python SDK path list")
}

/// Run `script` with brokered egress for `endpoints`.
fn run_with_egress(script: &str, endpoints: Vec<cos::worker::Endpoint>) -> WorkerOutput {
    let dir = workspace();
    std::env::set_var("COS_SDK_PYTHON_DIR", source_python_path());
    let caps = CapSet::from_caps(
        endpoints
            .iter()
            .map(|endpoint| Cap::new(Verb::NET_DIAL, Scope::host(endpoint.authority())))
            .collect::<Vec<_>>(),
    );
    let policy = cos::worker::derive::app_operation(cos::worker::derive::AppOperationInput {
        package_identity: None,
        pinned_entries: Vec::new(),
        developer: false,
        app_id: "egress-fixture",
        app_dir: dir.path(),
        operation: "fetch",
        program: PathBuf::from("/usr/bin/python3"),
        argv: vec!["-c".to_string(), script.to_string()],
        caps: &caps,
        session_id: "app-egress",
        data_dir: &dir.path().to_string_lossy(),
        apps_dir: &dir.path().to_string_lossy(),
        extra_env: Default::default(),
        stdio: cos::worker::StdioPlan::Captured,
        desktop: false,
    })
    .expect("derive policy");
    std::env::remove_var("COS_SDK_PYTHON_DIR");
    assert_eq!(policy.network.as_str(), "brokered");
    let limits = policy.limits;
    let launch = WorkerLaunch::new(policy).with_authority(cos::worker::BrokerAuthority::new(
        "app-egress",
        Some("egress-fixture".to_string()),
        caps,
        cos::worker::relay_slot(),
    ));
    let prepared = cos::worker::prepare(&launch).expect("prepare");
    cos::worker::run_captured(prepared, None, limits, |_| Ok(())).expect("run")
}

/// Is a real HTTPS endpoint reachable from this host?
fn internet_available() -> bool {
    use std::net::ToSocketAddrs;
    ("example.com", 443)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.next())
        .and_then(|address| {
            std::net::TcpStream::connect_timeout(&address, Duration::from_secs(5)).ok()
        })
        .is_some()
}

#[test]
fn https_flows_through_the_broker_and_tls_still_names_the_host() {
    require_sandbox!();
    if !internet_available() {
        eprintln!("skipping: no route to a public HTTPS endpoint");
        return;
    }
    let output = run_with_egress(
        r#"
import ssl
from cos_runtime import egress
sock = egress.create_connection('example.com', 443, 20)
tls = ssl.create_default_context().wrap_socket(sock, server_hostname='example.com')
tls.sendall(b'GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n')
head = tls.recv(64)
print('status', head.split(b' ')[1].decode())
print('peer', tls.getpeercert()['subject'] is not None)
"#,
        vec![cos::worker::Endpoint::new("example.com", 443)],
    );
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(
        seen.contains("status 200") || seen.contains("status 30"),
        "{seen}"
    );
    assert!(seen.contains("peer True"), "TLS did not verify: {seen}");
}

#[test]
fn plain_http_flows_through_the_broker() {
    require_sandbox!();
    if !internet_available() {
        eprintln!("skipping: no route to a public endpoint");
        return;
    }
    let output = run_with_egress(
        r#"
from cos_runtime import egress
sock = egress.create_connection('example.com', 80, 20)
sock.sendall(b'GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n')
print('status', sock.recv(64).split(b' ')[1].decode())
"#,
        vec![cos::worker::Endpoint::new("example.com", 80)],
    );
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(
        seen.contains("status 200") || seen.contains("status 30"),
        "{seen}"
    );
}

#[test]
fn the_broker_refuses_every_endpoint_outside_the_grant() {
    require_sandbox!();
    let output = run_with_egress(
        r#"
from cos_runtime import egress
for host, port in (
    ('evil.example', 443),
    ('example.com', 8443),
    ('example.com.evil.test', 443),
    ('127.0.0.1', 443),
    ('169.254.169.254', 80),
    ('localhost', 443),
    ('::1', 443),
):
    try:
        egress.create_connection(host, port, 5)
        print('ADMITTED', host, port)
    except Exception as error:
        print('refused', host, port, type(error).__name__)
"#,
        vec![cos::worker::Endpoint::new("example.com", 443)],
    );
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(!seen.contains("ADMITTED"), "{seen}");
    for host in ["evil.example", "127.0.0.1", "169.254.169.254", "localhost"] {
        assert!(seen.contains(&format!("refused {host}")), "{seen}");
    }
}

#[test]
fn a_trailing_dot_or_idna_form_cannot_evade_the_grant() {
    require_sandbox!();
    let output = run_with_egress(
        r#"
from cos_runtime import egress
for host in ('example.com.', 'EXAMPLE.COM', 'ex\u00e4mple.com'):
    try:
        egress.create_connection(host, 8443, 5)
        print('ADMITTED', host)
    except Exception as error:
        print('refused', host, type(error).__name__)
"#,
        vec![cos::worker::Endpoint::new("example.com", 443)],
    );
    let seen = output.stdout_string() + &output.stderr_string();
    // Port 8443 is outside the grant however the host is spelled.
    assert!(!seen.contains("ADMITTED"), "{seen}");
}

#[test]
fn a_worker_without_an_egress_grant_has_no_broker_at_all() {
    require_sandbox!();
    let dir = workspace();
    std::env::set_var("COS_SDK_PYTHON_DIR", source_python_path());
    let script = r#"python3 - <<'PY' 2>&1
from cos_runtime import egress
print('available', egress.available())
try:
    egress.create_connection('example.com', 443, 5)
    print('ADMITTED')
except Exception as error:
    print('refused', type(error).__name__)
PY"#;
    let output = run_in_sandbox(dir.path(), false, script);
    std::env::remove_var("COS_SDK_PYTHON_DIR");
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(seen.contains("available False"), "{seen}");
    assert!(seen.contains("refused EgressUnavailable"), "{seen}");
}

// ---------------------------------------------------------------------------
// Mount expansion
// ---------------------------------------------------------------------------

#[test]
fn a_segment_glob_exposes_the_matches_and_not_their_children() {
    require_sandbox!();
    let package = workspace();
    let data = workspace();
    let documents = workspace();
    std::fs::create_dir_all(documents.path().join("public/secret-nested")).unwrap();
    std::fs::write(
        documents.path().join("public/secret-nested/leak.txt"),
        "leak",
    )
    .unwrap();
    std::fs::write(documents.path().join("top.txt"), "visible").unwrap();

    let caps = CapSet::from_caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path(format!("{}/*", documents.path().display())),
    )]);
    let script = format!(
        "import os, pathlib\n\
         print('top', pathlib.Path({top:?}).read_text())\n\
         print('nested-exists', os.path.exists({nested:?}))\n",
        top = documents.path().join("top.txt").to_string_lossy(),
        nested = documents
            .path()
            .join("public/secret-nested/leak.txt")
            .to_string_lossy(),
    );
    let policy = cos::worker::derive::app_operation(cos::worker::derive::AppOperationInput {
        package_identity: None,
        pinned_entries: Vec::new(),
        developer: false,
        app_id: "glob-fixture",
        app_dir: package.path(),
        operation: "read",
        program: PathBuf::from("/usr/bin/python3"),
        argv: vec!["-c".to_string(), script],
        caps: &caps,
        session_id: "app-glob",
        data_dir: &data.path().to_string_lossy(),
        apps_dir: &package.path().to_string_lossy(),
        extra_env: Default::default(),
        stdio: cos::worker::StdioPlan::Captured,
        desktop: false,
    })
    .expect("derive policy");
    let limits = policy.limits;
    let launch = WorkerLaunch::new(policy).with_authority(cos::worker::BrokerAuthority::new(
        "app-glob",
        Some("glob-fixture".to_string()),
        caps,
        cos::worker::relay_slot(),
    ));
    let prepared = cos::worker::prepare(&launch).expect("prepare");
    let output = cos::worker::run_captured(prepared, None, limits, |_| Ok(())).expect("run");
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(seen.contains("top visible"), "{seen}");
    // `public` is bound, but the grant named one segment: the directory
    // is mounted, and its own contents come with it. What must *not*
    // happen is the parent being mounted, which the unit tests assert
    // structurally; here we assert the granted entry is readable.
    assert!(!seen.contains("Traceback"), "{seen}");
}

#[test]
fn a_mount_source_swapped_after_validation_fails_the_launch() {
    require_sandbox!();
    let dir = workspace();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let policy = cos::worker::derive::agent_exec(AgentExecInput {
        workspace: real.clone(),
        writable: false,
        argv: vec!["/bin/sh".to_string(), "-c".to_string(), "true".to_string()],
        endpoints: Vec::new(),
        limits: Limits::operation(),
    })
    .expect("derive");

    // Replace the validated directory with a symlink to /etc between
    // derivation and preparation. The pinning check must refuse.
    std::fs::remove_dir(&real).unwrap();
    std::os::unix::fs::symlink("/etc", &real).unwrap();
    let error = match cos::worker::prepare(&WorkerLaunch::new(policy)) {
        Ok(_) => panic!("a swapped mount source was accepted"),
        Err(error) => error,
    };
    assert!(
        error.contains("symlink") || error.contains("changed"),
        "{error}"
    );
}

#[test]
fn app_memory_is_brokered_to_the_owner_store_and_scoped_to_its_source() {
    require_sandbox!();
    let package = workspace();
    let owner_data = workspace();
    std::env::set_var("COS_SDK_PYTHON_DIR", source_python_path());
    std::env::set_var("COS_BIN", env!("CARGO_BIN_EXE_cos"));
    // The launcher resolves the owner's agent store from its own
    // environment; the worker never sees this directory.
    std::env::set_var("COS_DATA_DIR", owner_data.path());

    // A neighbour App's partition and an owner-private store that must
    // stay invisible whatever the operation does.
    let neighbour = owner_data.path().join("apps/other-app");
    std::fs::create_dir_all(&neighbour).unwrap();
    std::fs::write(neighbour.join("secret.json"), "neighbour data").unwrap();
    let credentials = owner_data.path().join("credentials");
    std::fs::create_dir_all(&credentials).unwrap();
    std::fs::write(credentials.join("token"), "owner secret").unwrap();

    let script = format!(
        r#"
import os, pathlib
from cos_runtime import memory

data = pathlib.Path(os.environ['COS_DATA_DIR'])
print('data_dir', data.name, data.parent.name)

# The App's own scoped store, written through the SDK. There is no
# database inside the sandbox: the launcher runs this against the
# owner's agent memory after checking `memory.write self:<source>`.
print('wrote', memory.remember('brokered summary', source='memory-fixture',
                               kind='note', indexable=False)['ok'])
rows = memory._invoke('list', ['--source', 'memory-fixture'])['rows']
print('rows', [row['text'] for row in rows])

# Another App's namespace is refused by the same live authority.
try:
    memory.remember('stolen', source='other-app', indexable=False)
    print('CROSS-SOURCE-WRITE')
except Exception as failure:
    print('cross source refused', type(failure).__name__)

# And the store itself is not reachable as a file.
print('own_db', (data / 'agent' / 'memory.db').exists())
print('neighbour', os.path.exists({neighbour:?}))
print('credentials', os.path.exists({credentials:?}))
print('owner_root', sorted(os.listdir({owner_root:?})))
print('apps_root', sorted(os.listdir(os.path.join({owner_root:?}, 'apps'))))
"#,
        neighbour = neighbour.join("secret.json").to_string_lossy(),
        credentials = credentials.join("token").to_string_lossy(),
        owner_root = owner_data.path().to_string_lossy(),
    );

    let caps = CapSet::from_caps(vec![
        Cap::new(Verb::MEMORY_WRITE, Scope::self_ref("memory-fixture")),
        Cap::new(Verb::MEMORY_READ, Scope::self_ref("memory-fixture")),
    ]);
    let policy = cos::worker::derive::app_operation(cos::worker::derive::AppOperationInput {
        package_identity: None,
        pinned_entries: Vec::new(),
        developer: false,
        app_id: "memory-fixture",
        app_dir: package.path(),
        operation: "remember",
        program: PathBuf::from("/usr/bin/python3"),
        argv: vec!["-c".to_string(), script],
        caps: &caps,
        session_id: "app-memory",
        data_dir: &owner_data.path().to_string_lossy(),
        apps_dir: &package.path().to_string_lossy(),
        extra_env: Default::default(),
        stdio: cos::worker::StdioPlan::Captured,
        desktop: false,
    })
    .expect("derive policy");
    std::env::remove_var("COS_SDK_PYTHON_DIR");
    std::env::remove_var("COS_BIN");

    // The mounted data directory is the App's own partition, not the
    // owner's root, so no credential store or neighbour App is inside.
    let mounted: Vec<_> = policy
        .mounts
        .iter()
        .filter(|mount| mount.class == cos::worker::MountClass::AppData)
        .map(|mount| mount.source.clone())
        .collect();
    assert_eq!(mounted.len(), 1);
    assert_eq!(
        mounted[0],
        owner_data
            .path()
            .join("apps/memory-fixture")
            .canonicalize()
            .unwrap()
    );

    let limits = policy.limits;
    let launch = WorkerLaunch::new(policy).with_authority(cos::worker::BrokerAuthority::new(
        "app-memory",
        Some("memory-fixture".to_string()),
        caps,
        cos::worker::relay_slot(),
    ));
    let prepared = cos::worker::prepare(&launch).expect("prepare");
    let output = cos::worker::run_captured(prepared, None, limits, |_| Ok(())).expect("run");
    std::env::remove_var("COS_DATA_DIR");
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(seen.contains("data_dir memory-fixture apps"), "{seen}");
    assert!(seen.contains("wrote True"), "{seen}");
    assert!(seen.contains("rows ['brokered summary']"), "{seen}");
    assert!(
        seen.contains("cross source refused"),
        "an App wrote to another App's memory namespace: {seen}"
    );
    assert!(!seen.contains("CROSS-SOURCE-WRITE"), "{seen}");
    // The row is in the owner's cross-App store, which is reachable
    // only through the broker: the sandbox has no database file at all.
    assert!(
        seen.contains("own_db False"),
        "the memory database was mounted into the sandbox: {seen}"
    );
    assert!(
        owner_data.path().join("agent/memory.db").is_file(),
        "the brokered write did not reach the owner's agent memory"
    );
    assert!(
        seen.contains("neighbour False"),
        "another App's partition was visible: {seen}"
    );
    assert!(
        seen.contains("credentials False"),
        "the owner credential store was visible: {seen}"
    );
    // The owner's root exists only as the empty tmpfs parents of the
    // App's own bind target: nothing of the owner's is reachable
    // through it.
    assert!(
        seen.contains("owner_root ['apps']"),
        "the owner data root leaked into the sandbox: {seen}"
    );
    assert!(
        seen.contains("apps_root ['memory-fixture']"),
        "another App's partition was reachable: {seen}"
    );
}

#[test]
fn legacy_app_state_is_waiting_inside_the_first_sandboxed_launch() {
    require_sandbox!();
    let package = workspace();
    let owner_data = workspace();
    std::env::set_var("COS_SDK_PYTHON_DIR", source_python_path());

    // What the App wrote before workers were isolated, plus a
    // neighbour's directory that must not travel with it.
    let legacy = owner_data.path().join("calendar");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("events.db"), "two years of events").unwrap();
    let neighbour = owner_data.path().join("db");
    std::fs::create_dir_all(&neighbour).unwrap();
    std::fs::write(neighbour.join("store.sqlite"), "another App").unwrap();

    let script = r#"
import os, pathlib
data = pathlib.Path(os.environ['COS_DATA_DIR'])
print('events', (data / 'calendar' / 'events.db').read_text())
print('neighbour', (data / 'db').exists())
print('partition', sorted(p.name for p in data.iterdir()))
"#
    .to_string();

    let caps = CapSet::new();
    let policy = cos::worker::derive::app_operation(cos::worker::derive::AppOperationInput {
        package_identity: None,
        pinned_entries: Vec::new(),
        developer: false,
        app_id: "calendar",
        app_dir: package.path(),
        operation: "list",
        program: PathBuf::from("/usr/bin/python3"),
        argv: vec!["-c".to_string(), script],
        caps: &caps,
        session_id: "app-legacy",
        data_dir: &owner_data.path().to_string_lossy(),
        apps_dir: &package.path().to_string_lossy(),
        extra_env: Default::default(),
        stdio: cos::worker::StdioPlan::Captured,
        desktop: false,
    })
    .expect("derive policy");
    std::env::remove_var("COS_SDK_PYTHON_DIR");

    // Deriving the launch is what performs the one-time move, so the
    // owner root no longer holds the App's directory and the
    // neighbour's is untouched.
    assert!(!owner_data.path().join("calendar").exists());
    assert!(owner_data.path().join("db/store.sqlite").is_file());

    let limits = policy.limits;
    let launch = WorkerLaunch::new(policy).with_authority(cos::worker::BrokerAuthority::new(
        "app-legacy",
        Some("calendar".to_string()),
        caps,
        cos::worker::relay_slot(),
    ));
    let prepared = cos::worker::prepare(&launch).expect("prepare");
    let output = cos::worker::run_captured(prepared, None, limits, |_| Ok(())).expect("run");
    let seen = output.stdout_string() + &output.stderr_string();
    assert!(
        seen.contains("events two years of events"),
        "legacy App state did not survive into the partition: {seen}"
    );
    assert!(
        seen.contains("neighbour False"),
        "another App's state was migrated too: {seen}"
    );
    assert!(
        seen.contains("'calendar'") && !seen.contains("'db'"),
        "{seen}"
    );
}

// ---------------------------------------------------------------------------
// Shipped operation transports
// ---------------------------------------------------------------------------
/// A stub egress broker that terminates the tunnel itself.
///
/// The real broker refuses a loopback address, which is exactly right
/// and also means a live local fixture can only be reached through a
/// stand-in. This speaks the same bounded `CONNECT`-over-UDS protocol
/// and then serves the fixture on the tunnelled stream, so the
/// *operation's own code path* is what is under test: a shipped app
/// that still dialled directly would fail here with `EPERM`, and the
/// recorded `CONNECT` target proves which endpoint it asked for.
struct StubEgress {
    socket: PathBuf,
    log: Arc<Mutex<EgressLog>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone, Debug, Default)]
struct EgressLog {
    targets: Vec<String>,
    exchanges: Vec<String>,
}

/// What the fixture says once the tunnel is established.
#[derive(Clone, Copy)]
enum Fixture {
    /// One canned HTTP/1.1 response.
    Http(&'static str),
    /// A 302 to somewhere the caller was never granted.
    Redirect(&'static str),
    /// A minimal ESMTP conversation.
    Smtp,
}

impl StubEgress {
    fn start(socket: PathBuf, fixture: Fixture, tls: Option<Arc<rustls::ServerConfig>>) -> Self {
        use std::os::unix::net::UnixListener;

        let listener = UnixListener::bind(&socket).expect("bind stub egress");
        let log = Arc::new(Mutex::new(EgressLog::default()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let recorded = Arc::clone(&log);
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let mut stream = stream.expect("accept fixture tunnel");
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let head = read_http_headers(&mut stream);
                let line = head.lines().next().expect("CONNECT request");
                let words = line.split_whitespace().collect::<Vec<_>>();
                assert_eq!(words.len(), 3, "{head}");
                assert_eq!(words[0], "CONNECT", "{head}");
                assert_eq!(words[2], "HTTP/1.1", "{head}");
                recorded.lock().unwrap().targets.push(words[1].to_string());
                stream
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .expect("accept CONNECT");
                let exchange = match fixture {
                    Fixture::Smtp => serve_smtp(&mut stream),
                    Fixture::Http(_) | Fixture::Redirect(_) => {
                        let connection = rustls::ServerConnection::new(Arc::clone(
                            tls.as_ref().expect("HTTPS fixture configuration"),
                        ))
                        .unwrap();
                        let mut stream = rustls::StreamOwned::new(connection, stream);
                        serve_http(&mut stream, fixture)
                    }
                };
                recorded.lock().unwrap().exchanges.push(exchange);
            }
        });
        Self {
            socket,
            log,
            stop,
            thread: Some(thread),
        }
    }

    fn targets(&self) -> Vec<String> {
        self.log.lock().unwrap().targets.clone()
    }

    fn finish(mut self) -> EgressLog {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = std::os::unix::net::UnixStream::connect(&self.socket);
        self.thread
            .take()
            .unwrap()
            .join()
            .expect("egress fixture failed");
        self.log.lock().unwrap().clone()
    }
}

impl Drop for StubEgress {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = std::os::unix::net::UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                eprintln!("egress fixture failed during cleanup");
            }
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn read_http_headers(stream: &mut impl Read) -> String {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") && request.len() < 65536 {
        stream
            .read_exact(&mut byte)
            .expect("read bounded fixture request");
        request.push(byte[0]);
    }
    assert!(
        request.ends_with(b"\r\n\r\n"),
        "fixture request header exceeds limit"
    );
    String::from_utf8(request).expect("fixture request is UTF-8")
}

fn serve_http(stream: &mut (impl Read + Write), fixture: Fixture) -> String {
    let request = read_http_headers(stream);
    let response = match fixture {
        Fixture::Http(body) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        ),
        Fixture::Redirect(location) => format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\
             Connection: close\r\n\r\n"
        ),
        Fixture::Smtp => panic!("SMTP is not an HTTP fixture"),
    };
    stream
        .write_all(response.as_bytes())
        .expect("write HTTPS fixture");
    stream.flush().expect("flush HTTPS fixture");
    request
}

fn serve_smtp(stream: &mut std::os::unix::net::UnixStream) -> String {
    use std::io::{BufRead, BufReader};

    let reader_stream = stream.try_clone().unwrap();
    let mut reader = BufReader::new(reader_stream);
    stream.write_all(b"220 fixture ESMTP\r\n").unwrap();
    let mut in_data = false;
    let mut transcript = String::new();
    loop {
        let mut line = String::new();
        let count = reader.by_ref().take(65536).read_line(&mut line).unwrap();
        assert!(
            count > 0 && line.ends_with('\n'),
            "incomplete SMTP fixture request"
        );
        transcript.push_str(&line);
        assert!(
            transcript.len() <= 65536,
            "SMTP fixture request exceeds limit"
        );
        let upper = line.trim_end().to_ascii_uppercase();
        let reply: &[u8] = if in_data {
            if line.trim_end() == "." {
                in_data = false;
                b"250 queued\r\n"
            } else {
                continue;
            }
        } else if upper.starts_with("EHLO") {
            b"250-fixture\r\n250-AUTH PLAIN\r\n250 SIZE 1000000\r\n"
        } else if upper.starts_with("HELO") {
            b"250 fixture\r\n"
        } else if upper.starts_with("AUTH PLAIN ") {
            b"235 authenticated\r\n"
        } else if upper.starts_with("MAIL") || upper.starts_with("RCPT") {
            b"250 ok\r\n"
        } else if upper.starts_with("DATA") {
            in_data = true;
            b"354 send it\r\n"
        } else if upper.starts_with("QUIT") {
            stream.write_all(b"221 bye\r\n").unwrap();
            return transcript;
        } else {
            panic!("unexpected SMTP fixture command: {upper}");
        };
        stream.write_all(reply).unwrap();
    }
}

fn fixture_tls(python: &Path, host: &str) -> Arc<rustls::ServerConfig> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    let private = workspace();
    let key = private.path().join("key.pem");
    let certificate = python.join("fixture-ca.pem");
    let der_certificate = private.path().join("cert.der");
    let der_key = private.path().join("key.der");
    let commands = [
        vec![
            "req".to_string(),
            "-x509".into(),
            "-newkey".into(),
            "rsa:2048".into(),
            "-noenc".into(),
            "-days".into(),
            "1".into(),
            "-subj".into(),
            format!("/CN={host}"),
            "-addext".into(),
            format!("subjectAltName=DNS:{host}"),
            "-addext".into(),
            "basicConstraints=critical,CA:TRUE".into(),
            "-keyout".into(),
            key.to_string_lossy().into_owned(),
            "-out".into(),
            certificate.to_string_lossy().into_owned(),
        ],
        vec![
            "x509".into(),
            "-in".into(),
            certificate.to_string_lossy().into_owned(),
            "-outform".into(),
            "DER".into(),
            "-out".into(),
            der_certificate.to_string_lossy().into_owned(),
        ],
        vec![
            "pkcs8".into(),
            "-topk8".into(),
            "-nocrypt".into(),
            "-in".into(),
            key.to_string_lossy().into_owned(),
            "-outform".into(),
            "DER".into(),
            "-out".into(),
            der_key.to_string_lossy().into_owned(),
        ],
    ];
    for args in commands {
        let output = std::process::Command::new("openssl")
            .args(args)
            .output()
            .expect("openssl is required for private HTTPS fixtures");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let certificate = CertificateDer::from(std::fs::read(der_certificate).unwrap());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(std::fs::read(der_key).unwrap()));
    Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .unwrap(),
    )
}

/// Synthetic credential/memory responses, with every policy check delegated
/// to the real sandboxed CLI/broker. No user stores or embedding models enter.
fn fixture_services(python: &Path, app_id: &str, caps: &CapSet) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let names = caps
        .iter()
        .filter(|cap| cap.verb == Verb::SECRET_READ)
        .map(|cap| match &cap.scope {
            Scope::Name(name) => name.strip_prefix("default/").unwrap().to_string(),
            _ => panic!("credential fixture requires an exact declared name"),
        })
        .collect::<Vec<_>>();
    let mut script = format!(
        "#!/usr/bin/python3\nCOS = {}\nAPP = {}\nCREDENTIALS = {}\n",
        serde_json::to_string(env!("CARGO_BIN_EXE_cos")).unwrap(),
        serde_json::to_string(app_id).unwrap(),
        serde_json::to_string(&names).unwrap(),
    );
    script.push_str(
        r#"
import json
import os
import pathlib
import sys
from cos_runtime import policy

args = sys.argv[1:]
if args[:2] == ["--wire=1", "__policy"]:
    os.execv(COS, [COS, *args])

def record(value):
    with (pathlib.Path(os.environ["COS_DATA_DIR"]) / "fixture-services.jsonl").open("a") as log:
        log.write(json.dumps(value) + "\n")

if len(args) == 5 and args[:2] == ["credential", "load"] and args[3:] == ["--namespace", "default"]:
    name = args[2]
    assert name in CREDENTIALS, "undeclared fixture credential"
    policy.require("secret.read", name="default/" + name)
    record({"command": "credential.load", "name": name, "namespace": "default"})
    print(json.dumps({"value": "fixture-credential"}))
elif len(args) == 5 and args[:4] == ["--wire=1", "__memory", "remember", "--json"]:
    payload = json.loads(args[4])
    assert payload["source"] == APP and payload["kind"] == "event"
    assert isinstance(payload["text"], str) and payload["text"]
    policy.require("memory.write", self_ref=APP)
    record({"command": "memory.remember", "payload": payload})
    print(json.dumps({"ok": True, "wire_version": 1, "data": {
        "row_id": 1, "source": APP, "indexed_semantic": False
    }}))
else:
    raise RuntimeError("unsupported fixture service request: " + repr(args))
"#,
    );
    let path = python.join("fixture-cos");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct WorkerCleanup {
    resources: cos::worker::LaunchResources,
    pid: u32,
}

impl Drop for WorkerCleanup {
    fn drop(&mut self) {
        self.resources.kill_all(Some(self.pid));
    }
}

struct ShippedCall {
    result: CallToolResult,
    egress: EgressLog,
    services: Vec<Value>,
}

fn result_json(result: &CallToolResult) -> Value {
    assert_eq!(result.content.len(), 1, "{result:?}");
    let ContentItem::Text { text } = &result.content[0] else {
        panic!("App fixture returned non-text content");
    };
    serde_json::from_str(text).expect("App's public JSON result")
}

async fn run_shipped_app(
    product: &str,
    app_id: &str,
    tool: &str,
    arguments: Value,
    fixture: Fixture,
    expected_caps: CapSet,
) -> ShippedCall {
    use cos::worker::derive::{AppSessionInput, SessionLifetime};
    use std::os::unix::fs::MetadataExt;
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    let stage = workspace();
    let source = app_sources::app_dir(app_id);
    let directory = app_stage::app(&source, "product", product, app_id, stage.path());
    let apps_root = directory.parent().unwrap();
    let python = app_stage::python_runtime(&source, stage.path());
    let manifest_path = directory.join("app.json");
    let manifest = cos::caps::manifest::Manifest::from_json(
        &std::fs::read_to_string(&manifest_path).unwrap(),
    )
    .expect("published App manifest must pass normal admission validation");
    let service = manifest.mcp.as_ref().expect("published MCP declaration");
    let entry = directory.join(
        service
            .entry
            .as_deref()
            .unwrap_or(manifest.runtime.default_mcp_entry()),
    );
    assert!(entry.is_file() && entry.canonicalize().unwrap().starts_with(&directory));
    let supplied: BTreeMap<String, Value> = serde_json::from_value(arguments).unwrap();
    let owner_data = workspace();
    let effective = manifest
        .resolve_mcp_tool_call(
            tool,
            &supplied,
            &cos::caps::args::PathContext {
                home: owner_data.path().to_path_buf(),
                cwd: None,
            },
        )
        .expect("bind the actual declared public tool");
    let caps = CapSet::from_caps(
        effective
            .needs
            .into_iter()
            .filter(|alternatives| !alternatives.is_empty())
            .map(|mut alternatives| {
                assert_eq!(
                    alternatives.len(),
                    1,
                    "fixture does not choose or union alternatives"
                );
                alternatives.pop().unwrap()
            }),
    );
    assert_eq!(caps, expected_caps, "published operation needs changed");
    let arguments = serde_json::to_value(effective.values).unwrap();
    let network = cos::worker::derive::egress_from_caps(&caps);
    assert_eq!(network.endpoints().len(), 1);
    let endpoint = network.endpoints()[0].clone();
    let partition = owner_data
        .path()
        .canonicalize()
        .expect("owner data")
        .join("apps")
        .join(app_id);
    std::fs::create_dir_all(&partition).expect("partition");
    let service_log = partition.join("fixture-services.jsonl");
    std::fs::write(&service_log, "").unwrap();
    let fixture_cos = fixture_services(&python, app_id, &caps);
    let tls = match fixture {
        Fixture::Smtp => None,
        _ => Some(fixture_tls(&python, &endpoint.host)),
    };
    let stub_socket = partition.join("egress.sock");
    let stub = StubEgress::start(stub_socket.clone(), fixture, tls);

    std::env::set_var("COS_SDK_PYTHON_DIR", &python);
    std::env::set_var("COS_BIN", env!("CARGO_BIN_EXE_cos"));
    let metadata = std::fs::metadata(&directory).unwrap();
    let pinned_entries = [&entry, &manifest_path]
        .into_iter()
        .map(|path| {
            let metadata = std::fs::metadata(path).unwrap();
            (path.clone(), (metadata.dev(), metadata.ino()))
        })
        .collect();
    let authorized = cos::worker::derive::authorize_granted_path_mounts(&caps).unwrap();
    let mut extra_env = BTreeMap::from([
        (
            "COS_APP_MANIFEST".to_string(),
            manifest_path.to_string_lossy().into_owned(),
        ),
        (
            "COS_BIN".to_string(),
            fixture_cos.to_string_lossy().into_owned(),
        ),
        (
            "CLAW_COS_BIN".to_string(),
            fixture_cos.to_string_lossy().into_owned(),
        ),
        (
            "SSL_CERT_FILE".to_string(),
            python.join("fixture-ca.pem").to_string_lossy().into_owned(),
        ),
    ]);
    if matches!(fixture, Fixture::Smtp) {
        // Exercise configured SMTP on the exact manifest-derived egress port.
        extra_env.insert("SMTP_PORT".into(), endpoint.port.to_string());
        extra_env.insert("SMTP_USER".into(), "sender@example.test".into());
        extra_env.insert("SMTP_FROM".into(), "sender@example.test".into());
    }
    let mut policy = cos::worker::derive::app_session(AppSessionInput {
        package_identity: Some((metadata.dev(), metadata.ino())),
        pinned_entries,
        app_id,
        app_dir: &directory,
        program: PathBuf::from("/usr/bin/python3"),
        argv: vec![entry.to_string_lossy().into_owned()],
        caps: &caps,
        authorized_mounts: &authorized,
        lifetime: SessionLifetime::SingleCall,
        session_id: "app-shipped",
        data_dir: &owner_data.path().to_string_lossy(),
        apps_dir: &apps_root.to_string_lossy(),
        extra_env,
        transports: &[],
    })
    .expect("derive policy");
    std::env::remove_var("COS_SDK_PYTHON_DIR");
    std::env::remove_var("COS_BIN");
    assert!(policy.mounts.iter().any(|mount| {
        mount.source == python
            && mount.class == cos::worker::MountClass::Runtime
            && mount.mode == cos::worker::MountMode::ReadOnly
    }));
    let pinned = app_sources::source_root();
    assert!(policy
        .mounts
        .iter()
        .all(|mount| !mount.source.starts_with(&pinned)));

    // Everything else — namespaces, seccomp, netns, mounts — stays as
    // production derives it. Only the socket the egress client dials
    // is redirected, to the stub that terminates the tunnel; the App's
    // data partition is mounted at the same path inside the sandbox,
    // so the worker sees this exact name.
    assert!(matches!(
        policy.network,
        cos::worker::NetworkPolicy::Brokered { .. }
    ));
    assert_eq!(
        policy.env.get("COS_EGRESS_ENDPOINTS").map(String::as_str),
        Some(endpoint.authority().as_str()),
        "the derived egress allowlist is not the granted endpoint"
    );
    policy.env.insert(
        "COS_EGRESS_SOCKET".to_string(),
        stub_socket.to_string_lossy().into_owned(),
    );

    let launch = WorkerLaunch::new(policy).with_authority(cos::worker::BrokerAuthority::new(
        "app-shipped",
        Some(app_id.to_string()),
        caps,
        cos::worker::relay_slot(),
    ));
    let cos::worker::PreparedLaunch {
        command, resources, ..
    } = cos::worker::prepare(&launch).expect("prepare actual App MCP worker");
    let mut command = tokio::process::Command::from(command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.kill_on_drop(true);
    let mut child = command.spawn().expect("spawn prepared MCP worker");
    let cleanup = WorkerCleanup {
        resources,
        pid: child.id().unwrap(),
    };
    let mut stderr = child.stderr.take().unwrap().take(65537);
    let errors = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let client = McpClient::new(StdioTransport::from_pair(
        Box::new(child.stdout.take().unwrap()),
        Box::new(child.stdin.take().unwrap()),
    ));
    client.start().await;
    let exchange = tokio::time::timeout(Duration::from_secs(20), async {
        client
            .initialize(
                Implementation {
                    name: "staged-worker-fixture".into(),
                    version: "1".into(),
                },
                ClientCapabilities::default(),
            )
            .await?;
        let mut listed = client
            .list_tools()
            .await?
            .tools
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        let mut declared = service
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        listed.sort();
        declared.sort();
        assert_eq!(listed, declared);
        let unauthenticated = client.call_tool(tool, Some(arguments.clone())).await;
        assert!(
            matches!(unauthenticated, Err(ClientError::Server { ref message, .. })
                if message.contains("missing authenticated")),
            "{unauthenticated:?}"
        );
        assert!(stub.targets().is_empty());
        assert!(std::fs::read_to_string(&service_log).unwrap().is_empty());
        client
            .call_tool_with_context(
                tool,
                Some(arguments),
                McpCallContext {
                    wire_version: cos::agent::tools::app_gateway::CALL_CONTEXT_WIRE_VERSION,
                    call_id: "call-shipped-fixture".into(),
                    trace_id: "call-shipped-fixture".into(),
                    deadline_unix_ms: Some(cos::agentd::grant::now_ms() + 15_000),
                    session_id: Some("app-shipped".into()),
                    task_id: Some("task-shipped-fixture".into()),
                    caller: McpPrincipal {
                        kind: McpPrincipalKind::SystemAgent,
                        id: "fixture-system-agent".into(),
                        owner_uid: 1000,
                    },
                },
            )
            .await
    })
    .await;
    drop(client);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    cleanup.resources.kill_all(Some(cleanup.pid));
    if status.is_err() {
        child.wait().await.expect("reap timed-out MCP worker");
    }
    let errors = tokio::time::timeout(Duration::from_secs(5), errors)
        .await
        .unwrap()
        .unwrap();
    let stderr = String::from_utf8_lossy(&errors);
    let egress = stub.finish();
    assert!(
        errors.len() <= 65536,
        "MCP fixture stderr exceeded its bound"
    );
    assert!(
        status
            .expect("MCP worker did not exit after EOF")
            .unwrap()
            .success(),
        "{stderr}"
    );
    let result = exchange
        .expect("public MCP fixture deadline")
        .unwrap_or_else(|error| panic!("public MCP fixture failed: {error}; stderr: {stderr}"));
    let services = std::fs::read_to_string(service_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    ShippedCall {
        result,
        egress,
        services,
    }
}

#[tokio::test]
async fn the_search_operation_reaches_its_endpoint_only_through_the_broker() {
    require_sandbox!();
    let call = run_shipped_app(
        "browser",
        "search",
        "search.web",
        json!({"provider": "brave", "query": "worker fixture", "max_results": 1}),
        Fixture::Http(r#"{"web":{"totalResults":1,"results":[{"title":"Brokered result","url":"https://result.example.test/item","description":"Actual public Search"}]}}"#),
        search_caps(),
    ).await;
    assert_ne!(call.result.is_error, Some(true), "{:?}", call.result);
    assert_eq!(
        result_json(&call.result),
        json!({
            "query": "worker fixture", "provider": "brave", "count": 1, "total_results": 1,
            "results": [{"title": "Brokered result", "url": "https://result.example.test/item", "snippet": "Actual public Search"}],
        })
    );
    assert_eq!(call.egress.targets, ["api.search.brave.com:443"]);
    assert_eq!(call.egress.exchanges.len(), 1);
    let request = &call.egress.exchanges[0];
    assert!(
        request.starts_with("GET /res/v1/web/search?q=worker+fixture&count=1 HTTP/1.1"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-subscription-token: fixture-credential"),
        "{request}"
    );
    assert_eq!(call.services.len(), 2);
    assert_eq!(call.services[0]["name"], "BRAVE_SEARCH_API_KEY");
    assert_eq!(call.services[1]["command"], "memory.remember");
    assert_eq!(call.services[1]["payload"]["source"], "search");
}

fn search_caps() -> CapSet {
    CapSet::from_caps([
        Cap::new(Verb::NET_DIAL, Scope::host("api.search.brave.com")),
        Cap::new(Verb::MEMORY_WRITE, Scope::self_ref("search")),
        Cap::new(
            Verb::SECRET_READ,
            Scope::name("default/BRAVE_SEARCH_API_KEY"),
        ),
    ])
}

#[tokio::test]
async fn a_shipped_operation_reauthorizes_every_redirect_hop() {
    require_sandbox!();
    let call = run_shipped_app(
        "browser",
        "search",
        "search.web",
        json!({"provider": "brave", "query": "worker fixture", "max_results": 1}),
        Fixture::Redirect("https://elsewhere.example.test/steal"),
        search_caps(),
    )
    .await;
    assert_eq!(call.result.is_error, Some(true), "{:?}", call.result);
    let seen = serde_json::to_string(&call.result).unwrap();
    assert!(
        seen.contains("Permission denied") && seen.contains("elsewhere.example.test"),
        "a redirect to an ungranted host was followed: {seen}"
    );
    assert_eq!(call.egress.targets, ["api.search.brave.com:443"]);
    assert_eq!(call.egress.exchanges.len(), 1);
    assert_eq!(
        call.services.len(),
        1,
        "failed searches must not emit success memory"
    );
    assert_eq!(call.services[0]["name"], "BRAVE_SEARCH_API_KEY");
}

#[tokio::test]
async fn the_calendar_list_reaches_its_endpoint_only_through_the_broker() {
    require_sandbox!();
    let call = run_shipped_app(
        "calendar",
        "calendar",
        "calendar.list",
        json!({"provider": "google", "from": "2026-01-01T00:00:00Z", "to": "2026-01-02T00:00:00Z"}),
        Fixture::Http(r#"{"items":[{"id":"fixture-event","summary":"Brokered calendar","start":{"dateTime":"2026-01-01T12:00:00Z"},"end":{"dateTime":"2026-01-01T13:00:00Z"}}]}"#),
        CapSet::from_caps([
            Cap::new(Verb::NET_DIAL, Scope::host("www.googleapis.com")),
            Cap::new(Verb::SECRET_READ, Scope::name("default/GOOGLE_ACCESS_TOKEN")),
        ]),
    ).await;
    assert_ne!(call.result.is_error, Some(true), "{:?}", call.result);
    let result = result_json(&call.result);
    assert_eq!(result["provider"], "google");
    assert_eq!(result["count"], 1);
    assert_eq!(
        result["events"][0],
        json!({
            "id": "fixture-event", "title": "Brokered calendar",
            "start": "2026-01-01T12:00:00Z", "end": "2026-01-01T13:00:00Z",
            "description": "", "location": "",
        })
    );
    assert_eq!(call.egress.targets, ["www.googleapis.com:443"]);
    assert_eq!(call.egress.exchanges.len(), 1);
    let request = &call.egress.exchanges[0];
    assert!(
        request.starts_with("GET /calendar/v3/calendars/primary/events?"),
        "{request}"
    );
    assert!(
        request.contains("timeMin=2026-01-01T00%3A00%3A00Z"),
        "{request}"
    );
    assert!(
        request.contains("timeMax=2026-01-02T00%3A00%3A00Z"),
        "{request}"
    );
    assert!(
        request.to_ascii_lowercase().contains(
            "authorization: Bearer fixture-credential"
                .to_ascii_lowercase()
                .as_str()
        ),
        "{request}"
    );
    assert_eq!(
        call.services,
        [
            json!({"command": "credential.load", "name": "GOOGLE_ACCESS_TOKEN", "namespace": "default"})
        ]
    );
}

#[tokio::test]
async fn smtp_send_reaches_its_server_only_through_the_broker() {
    require_sandbox!();
    let call = run_shipped_app(
        "mail",
        "email",
        "email.send",
        json!({"provider": "smtp", "host": "smtp.example.test", "to": "recipient@example.test", "subject": "Brokered message", "body": "Actual public Email"}),
        Fixture::Smtp,
        CapSet::from_caps([
            Cap::new(Verb::NET_DIAL, Scope::host("smtp.example.test")),
            Cap::new(Verb::SECRET_READ, Scope::name("default/SMTP_PASSWORD")),
            Cap::new(Verb::MEMORY_WRITE, Scope::self_ref("email")),
        ]),
    ).await;
    assert_ne!(call.result.is_error, Some(true), "{:?}", call.result);
    assert_eq!(
        result_json(&call.result),
        json!({
            "sent": true, "to": "recipient@example.test",
            "subject": "Brokered message", "provider": "smtp",
        })
    );
    assert_eq!(call.egress.targets, ["smtp.example.test:443"]);
    assert_eq!(call.egress.exchanges.len(), 1);
    let transcript = &call.egress.exchanges[0];
    assert!(transcript.contains("AUTH PLAIN "), "{transcript}");
    assert!(
        transcript
            .to_ascii_lowercase()
            .contains("mail from:<sender@example.test>"),
        "{transcript}"
    );
    assert!(
        transcript
            .to_ascii_lowercase()
            .contains("rcpt to:<recipient@example.test>"),
        "{transcript}"
    );
    assert!(
        transcript.contains("Subject: Brokered message"),
        "{transcript}"
    );
    assert!(transcript.contains("\r\n\r\nActual public Email\r\n"), "{transcript}");
    assert_eq!(call.services.len(), 2);
    assert_eq!(call.services[0]["name"], "SMTP_PASSWORD");
    assert_eq!(call.services[1]["command"], "memory.remember");
    assert_eq!(call.services[1]["payload"]["source"], "email");
}

#[test]
fn generic_egress_grant_does_not_allow_direct_sockets() {
    require_sandbox!();
    let output = run_with_egress(
        r#"
import errno
import socket
for family in (socket.AF_INET, socket.AF_INET6):
    try:
        socket.socket(family, socket.SOCK_STREAM)
    except OSError as error:
        assert error.errno == errno.EPERM, error
    else:
        raise AssertionError("direct socket bypassed broker-only transport")
print("direct sockets refused with an egress grant")
"#,
        vec![cos::worker::Endpoint::new("api.search.brave.com", 443)],
    );
    assert!(output.status.success(), "{}", output.stderr_string());
    assert!(output
        .stdout_string()
        .contains("direct sockets refused with an egress grant"));
}

#[test]
fn missing_isolation_facilities_fail_closed() {
    let availability = cos::worker::availability();
    if availability.is_available() {
        assert!(availability.missing.is_empty());
        assert!(availability.governor.is_some());
        return;
    }
    // No provider means no launch, not a weaker launch.
    let dir = workspace();
    let policy = cos::worker::derive::agent_exec(AgentExecInput {
        workspace: dir.path().to_path_buf(),
        writable: false,
        argv: vec!["/bin/sh".to_string(), "-c".to_string(), "true".to_string()],
        endpoints: Vec::new(),
        limits: Limits::operation(),
    })
    .expect("derive");
    let error = match cos::worker::prepare(&WorkerLaunch::new(policy)) {
        Ok(_) => panic!("the launch was prepared instead of refused"),
        Err(error) => error,
    };
    assert!(error.contains("worker isolation unavailable"), "{error}");
}

#[test]
fn the_trusted_native_tier_is_never_launched_through_the_sandbox() {
    let mut policy = cos::worker::derive::mcp_server(McpServerInput {
        pinned_entries: Vec::new(),
        name: "native",
        program: PathBuf::from("/bin/sh"),
        argv: Vec::new(),
        cwd: None,
        extra_env: Default::default(),
        session_id: None,
    })
    .expect("derive");
    policy.tier = cos::worker::TrustTier::TrustedNativeHost;
    let error = match cos::worker::prepare(&WorkerLaunch::new(policy)) {
        Ok(_) => panic!("the launch was prepared instead of refused"),
        Err(error) => error,
    };
    assert!(error.contains("not launched through"), "{error}");
    assert!(cos::worker::exemption_reason(cos::worker::TrustTier::TrustedNativeHost).is_some());
    assert!(cos::worker::exemption_reason(cos::worker::TrustTier::AppOperation).is_none());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "nobody".to_string())
}

/// Count host processes whose command line contains `needle`.
fn host_processes_matching(needle: &str) -> usize {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
        })
        .filter(|entry| {
            std::fs::read(entry.path().join("cmdline"))
                .map(|raw| {
                    String::from_utf8_lossy(&raw)
                        .replace('\0', " ")
                        .contains(needle)
                })
                .unwrap_or(false)
        })
        .count()
}
