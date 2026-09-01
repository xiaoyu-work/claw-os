//! Real process-boundary checks for the `agentd` worker split.
//!
//! These spawn an actual `claw-agentd` child through the same
//! `agentd::spawn` path `clawd` uses and inspect the kernel's view of
//! it — `/proc/<pid>/status`, `/proc/<pid>/fd`, `/proc/<pid>/environ` —
//! rather than trusting a helper's return value.
//!
//! The suite runs unprivileged, so the uid/gid drop itself is a no-op
//! here (the child is already the test account). What it does verify on
//! any account is the part that is identical either way: descriptor
//! isolation, the rebuilt environment, `PR_SET_NO_NEW_PRIVS`, the grant
//! binding, and fail-closed behaviour on a protocol or identity
//! mismatch. `agentd::spawn`'s unit tests cover the ordering and
//! verification of the privileged drop.

#![cfg(all(unix, target_os = "linux"))]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use cos::agentd::grant::{GrantClaims, GrantSigner, SignedGrant, GRANT_AUDIENCE, GRANT_VERSION};
use cos::agentd::protocol::{
    self, Assignment, BrokerFrame, ExecutionCommit, FrameReader, JobSpec, WorkerFrame,
    WorkerOutcome,
};
use cos::agentd::spawn::{self, ExecutionIsolation, SpawnedWorker, WorkerIdentity};
use tokio::io::{AsyncWriteExt, BufReader};

const WORKER_BIN: &str = env!("CARGO_BIN_EXE_claw-agentd");
const EXTENSION_HOST_BIN: &str = env!("CARGO_BIN_EXE_claw-extension-host");
/// Marker placed in the *parent's* environment. The worker rebuilds its
/// environment from an allowlist, so this must never appear in the
/// child.
const LEAK_MARKER: &str = "COS_AGENTD_TEST_BROKER_SECRET";
const CGROUP_PROBE_GATE: &str = "COS_TEST_CGROUP_PROBE_GATE";
const CGROUP_PROBE_ROOT: &str = "COS_TEST_CGROUP_PROBE_ROOT";

struct Harness {
    _home: tempfile::TempDir,
    _data: tempfile::TempDir,
    runtime_dir: PathBuf,
    primary_path: PathBuf,
    _primary_listener: std::os::unix::net::UnixListener,
    leaked_path: PathBuf,
    leaked_fd: i32,
    identity: WorkerIdentity,
    extension_identity: cos::extension_host::identity::ExtensionIdentity,
    isolation: ExecutionIsolation,
    cgroup_root: PathBuf,
    containment: std::sync::Arc<cos::extension_host::spawn::ContainmentRoot>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.leaked_fd);
        }
        std::env::remove_var(LEAK_MARKER);
        std::env::remove_var("COS_EXTENSION_HOST_BIN");
        std::env::remove_var("COS_RUNTIME_DIR");
        std::env::remove_var(spawn::ISOLATED_GROUP_ENV);
        std::env::remove_var(cos::extension_host::spawn::CGROUP_ROOT_ENV);
        std::env::remove_var("COS_AGENTD_TEST_ASSIGNMENT_FAILPOINT");
        std::env::remove_var("COS_AGENTD_TEST_COMMIT_MARKER");
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
        let _ = std::fs::remove_dir(&self.cgroup_root);
    }
}

fn harness() -> Option<Harness> {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: secure worker spawn integration requires root");
        return None;
    }

    let uid = std::env::var("SUDO_UID").ok()?.parse::<u32>().ok()?;
    if uid == 0 {
        return None;
    }
    let identity = match spawn::resolve_identity(uid) {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!("skipping: resolve task identity: {error}");
            return None;
        }
    };

    let home = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("skipping: create home tempdir: {error}");
            return None;
        }
    };
    let data = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("skipping: create data tempdir: {error}");
            return None;
        }
    };
    let runtime_dir = PathBuf::from(format!(
        "/run/cat-{}",
        uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>()
    ));
    if let Err(error) = std::fs::create_dir(&runtime_dir) {
        eprintln!("skipping: create runtime dir: {error}");
        return None;
    }
    if let Err(error) =
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o751))
    {
        eprintln!("skipping: chmod runtime dir: {error}");
        return None;
    }
    let worker_bin = runtime_dir.join("claw-agentd");
    let extension_bin = runtime_dir.join("claw-extension-host");
    if let Err(error) = std::fs::copy(WORKER_BIN, &worker_bin) {
        eprintln!("skipping: copy worker binary: {error}");
        return None;
    }
    if let Err(error) = std::fs::copy(EXTENSION_HOST_BIN, &extension_bin) {
        eprintln!("skipping: copy extension binary: {error}");
        return None;
    }
    if let Err(error) =
        std::fs::set_permissions(&worker_bin, std::fs::Permissions::from_mode(0o755))
    {
        eprintln!("skipping: chmod worker binary: {error}");
        return None;
    }
    if let Err(error) =
        std::fs::set_permissions(&extension_bin, std::fs::Permissions::from_mode(0o755))
    {
        eprintln!("skipping: chmod extension binary: {error}");
        return None;
    }

    let primary_path = runtime_dir.join("clawd.sock");
    let primary_listener = match std::os::unix::net::UnixListener::bind(&primary_path) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("skipping: bind primary socket: {error}");
            return None;
        }
    };
    if let Err(error) =
        std::fs::set_permissions(&primary_path, std::fs::Permissions::from_mode(0o660))
    {
        eprintln!("skipping: chmod primary socket: {error}");
        return None;
    }
    let raw = match std::ffi::CString::new(primary_path.as_os_str().as_encoded_bytes()) {
        Ok(raw) => raw,
        Err(error) => {
            eprintln!("skipping: encode primary socket path: {error}");
            return None;
        }
    };
    if unsafe { libc::chown(raw.as_ptr(), 0, identity.gid) } != 0 {
        return None;
    }

    std::env::set_var("COS_AGENTD_BIN", &worker_bin);
    std::env::set_var("COS_EXTENSION_HOST_BIN", &extension_bin);
    std::env::set_var("COS_DATA_DIR", data.path());
    std::env::set_var("COS_RUNTIME_DIR", &runtime_dir);
    std::env::set_var(spawn::ISOLATED_GROUP_ENV, "nogroup");
    std::env::set_var(LEAK_MARKER, "broker-only-value");
    let execution_gid = match spawn::resolve_isolated_execution_gid() {
        Ok(gid) => gid,
        Err(error) => {
            eprintln!("skipping: resolve isolated group: {error}");
            return None;
        }
    };
    if execution_gid == identity.gid {
        return None;
    }
    let isolation = match ExecutionIsolation::capture(&primary_path, uid, execution_gid) {
        Ok(isolation) => isolation,
        Err(error) => {
            eprintln!("skipping: capture broker socket boundary: {error}");
            return None;
        }
    };
    let extension_identity = cos::extension_host::identity::ExtensionIdentity {
        uid: cos::extension_host::identity::FIRST_UID,
        gid: execution_gid,
        username: "cos-ext-00".to_string(),
    };
    let cgroup_root = PathBuf::from(format!(
        "/sys/fs/cgroup/cos-test-{}",
        uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(12)
            .collect::<String>()
    ));
    if let Err(error) = std::fs::create_dir(&cgroup_root) {
        eprintln!("skipping: create delegated cgroup root: {error}");
        return None;
    }
    std::env::set_var(cos::extension_host::spawn::CGROUP_ROOT_ENV, &cgroup_root);
    let containment = match cos::extension_host::spawn::ContainmentRoot::establish() {
        Ok(containment) => std::sync::Arc::new(containment),
        Err(error) => {
            eprintln!("skipping: establish extension containment: {error}");
            std::env::remove_var(cos::extension_host::spawn::CGROUP_ROOT_ENV);
            let _ = std::fs::remove_dir(&cgroup_root);
            return None;
        }
    };

    // A descriptor the broker holds without `O_CLOEXEC`, standing in
    // for a queue lock or audit handle. The worker must not inherit it.
    let leaked_path = home.path().join("broker-held.bin");
    let mut file = std::fs::File::create(&leaked_path).ok()?;
    file.write_all(b"broker").ok()?;
    drop(file);
    let c_path = std::ffi::CString::new(leaked_path.to_string_lossy().as_bytes()).ok()?;
    let leaked_fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
    assert!(leaked_fd >= 0, "failed to open the leak probe");

    Some(Harness {
        _home: home,
        _data: data,
        runtime_dir,
        primary_path,
        _primary_listener: primary_listener,
        leaked_path,
        leaked_fd,
        identity,
        extension_identity,
        isolation,
        cgroup_root,
        containment,
    })
}

fn test_broker_context(harness: &Harness) -> cos::agentd::supervisor::BrokerContext {
    cos::agentd::supervisor::BrokerContext::new(
        cos::clawd::state::DaemonState::try_new().expect("daemon state"),
        cos::clawd::transport::limits::Admission::new(
            cos::clawd::transport::limits::Limits::default(),
        ),
        harness.primary_path.clone(),
    )
    .expect("broker context")
    .with_test_identity_pool(
        cos::extension_host::identity::ExtensionIdentityPool::for_test(
            harness.isolation.execution_gid(),
        ),
    )
}

fn assignment(
    grant: SignedGrant,
    protocol_version: u32,
    task_id: &str,
    owner_uid: u32,
    home: &std::path::Path,
) -> Assignment {
    Assignment {
        protocol: protocol_version,
        grant,
        job: JobSpec {
            id: task_id.to_string(),
            prompt: "process boundary probe".to_string(),
            context: None,
            branch_context: None,
            session_id: None,
            max_turns: Some(1),
            owner_uid,
            owner_home: home.to_string_lossy().into_owned(),
        },
        consent_context: cos::caps::ConsentContext::Attended,
        session: None,
        presence: None,
        extension: None,
    }
}

fn grant_for(
    signer: &GrantSigner,
    task_id: &str,
    owner: &WorkerIdentity,
    execution_gid: u32,
    worker_pid: u32,
    start_time_ticks: Option<u64>,
) -> SignedGrant {
    let issued_at_ms = cos::agentd::grant::now_ms();
    signer.issue(GrantClaims {
        v: GRANT_VERSION,
        audience: GRANT_AUDIENCE.to_string(),
        broker_pid: std::process::id(),
        task_id: task_id.to_string(),
        session_id: None,
        owner_uid: owner.uid,
        client: cos::session::SessionClient::default(),
        presence: None,
        capability_generation: cos::agent::tools::exposure::capability_generation(
            &cos::caps::CapSet::new(),
        ),
        prepare_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        commit_nonce: "fedcba9876543210fedcba9876543210".to_string(),
        extension: None,
        owner_gid: execution_gid,
        worker_pid,
        worker_start_time_ticks: start_time_ticks,
        issued_at_ms,
        expires_at_ms: issued_at_ms + 120_000,
        routes: protocol::worker_routes(),
    })
}

async fn send(worker: &mut SpawnedWorker, frame: &BrokerFrame) {
    let encoded = protocol::encode(frame).expect("encode");
    worker
        .channel
        .write_all(encoded.as_bytes())
        .await
        .expect("write frame");
    worker.channel.flush().await.expect("flush");
}

async fn prepare_and_commit(worker: &mut SpawnedWorker, assignment: Assignment) {
    send(worker, &BrokerFrame::Prepare(Box::new(assignment.clone()))).await;
    {
        let mut frames = FrameReader::new(BufReader::new(&mut worker.channel));
        let prepared =
            tokio::time::timeout(Duration::from_secs(30), frames.next_frame::<WorkerFrame>())
                .await
                .expect("prepare acknowledgement timed out")
                .expect("read prepare acknowledgement")
                .expect("prepare acknowledgement frame");
        let WorkerFrame::Prepared(prepared) = prepared else {
            panic!("worker executed before commit: {prepared:?}");
        };
        assert_eq!(
            prepared.prepare_nonce,
            assignment.grant.claims.prepare_nonce
        );
        assert_eq!(prepared.commit_nonce, assignment.grant.claims.commit_nonce);
    }
    send(
        worker,
        &BrokerFrame::Commit(Box::new(ExecutionCommit {
            protocol: protocol::PROTOCOL_VERSION,
            grant: assignment.grant.clone(),
            task_id: assignment.job.id.clone(),
            session_id: assignment.job.session_id.clone(),
            worker_pid: worker.pid,
            worker_start_time_ticks: worker.start_time_ticks,
            capability_generation: assignment.grant.claims.capability_generation.clone(),
            prepare_nonce: assignment.grant.claims.prepare_nonce.clone(),
            commit_nonce: assignment.grant.claims.commit_nonce.clone(),
        })),
    )
    .await;
}

fn proc_field(pid: u32, field: &str) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .map(|value| value.trim().to_string())
}

fn open_descriptor_targets(pid: u32) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return targets;
    };
    for entry in entries.flatten() {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            targets.push(target);
        }
    }
    targets
}

fn descriptor_flags(pid: u32, fd: i32) -> Option<u32> {
    let info = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")).ok()?;
    let encoded = info
        .lines()
        .find_map(|line| line.strip_prefix("flags:"))?
        .trim();
    u32::from_str_radix(encoded, 8).ok()
}

fn status_ids(pid: u32, key: &str) -> Vec<u32> {
    proc_field(pid, key)
        .map(|line| {
            line.split_whitespace()
                .filter_map(|value| value.parse::<u32>().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn process_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat[stat.rfind(')')? + 1..]
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

fn extension_cgroups(root: &std::path::Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("cos-extension-"))
        })
        .map(|entry| entry.path())
        .collect()
}

fn drop_command_identity(
    command: &mut std::process::Command,
    uid: u32,
    gid: u32,
) -> &mut std::process::Command {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(gid, gid, gid) != 0
                || libc::setresuid(uid, uid, uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

fn acl_probe_session(session_id: &str) -> cos::proc::SessionInfo {
    cos::proc::SessionInfo {
        session_id: session_id.to_string(),
        pid: std::process::id(),
        command: vec!["acl-probe".to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("extension-host".to_string()),
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: None,
        scope: None,
        priority: None,
        caps: Some(cos::caps::CapSet::new()),
        transient_caps: None,
        role: None,
        app_id: None,
        pending_bind: false,
        start_time_ticks: process_start(std::process::id()),
        client: cos::session::SessionClient::default(),
    }
}

fn install_daemonizing_host(harness: &Harness, exit_host: bool) -> PathBuf {
    let path = harness.runtime_dir.join("malicious-extension-host.py");
    let host_tail = if exit_host {
        "time.sleep(1)\nos._exit(23)\n"
    } else {
        "while True:\n    time.sleep(60)\n"
    };
    std::fs::write(
        &path,
        format!(
            "{}{}",
            concat!(
                "#!/usr/bin/python3\n",
                "import ctypes\n",
                "import os\n",
                "import time\n",
                "\n",
                "intermediate = os.fork()\n",
                "if intermediate == 0:\n",
                "    os.setsid()\n",
                "    escaped = os.fork()\n",
                "    if escaped != 0:\n",
                "        os._exit(0)\n",
                "    ctypes.CDLL(None).prctl(1, 0, 0, 0, 0)\n",
                "    marker = os.environ['COS_EXTENSION_CONTROL_SOCKET'] + '.escaped'\n",
                "    with open(marker, 'w', encoding='utf-8') as output:\n",
                "        output.write(str(os.getpid()))\n",
                "        output.flush()\n",
                "        os.fsync(output.fileno())\n",
                "    while True:\n",
                "        time.sleep(60)\n",
                "\n",
                "os.waitpid(intermediate, 0)\n",
            ),
            host_tail,
        ),
    )
    .expect("write daemonizing host");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod daemonizing host");
    path
}

fn install_crashing_control_host(harness: &Harness, marker: &std::path::Path) -> PathBuf {
    let path = harness.runtime_dir.join("crashing-control-host.py");
    let marker = serde_json::to_string(&marker.to_string_lossy()).expect("encode marker path");
    let script = format!(
        "{}{}{}",
        concat!(
            "#!/usr/bin/python3\n",
            "import os\n",
            "import socket\n",
            "\n",
            "def receive_exact(connection, size):\n",
            "    value = b''\n",
            "    while len(value) < size:\n",
            "        chunk = connection.recv(size - len(value))\n",
            "        if not chunk:\n",
            "            os._exit(21)\n",
            "        value += chunk\n",
            "    return value\n",
            "\n",
            "control = os.environ['COS_EXTENSION_CONTROL_SOCKET']\n",
            "try:\n",
            "    os.unlink(control)\n",
            "except FileNotFoundError:\n",
            "    pass\n",
            "listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n",
            "listener.bind(control)\n",
            "os.chmod(control, 0o660)\n",
            "listener.listen(1)\n",
            "connection, _ = listener.accept()\n",
            "header = receive_exact(connection, 10)\n",
            "length = int.from_bytes(header[6:10], 'big')\n",
            "receive_exact(connection, length)\n",
            "effect = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)\n",
            "effect.sendto(b'executed\\n', "
        ),
        marker,
        concat!(")\n", "os._exit(23)\n",)
    );
    std::fs::write(&path, script).expect("write crashing control host");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod crashing control host");
    path
}

fn install_process_isolation_probe(
    harness: &Harness,
    worker_pid: u32,
    ordinary_pid: u32,
    sibling_pid: u32,
) -> PathBuf {
    let path = harness.runtime_dir.join("process-isolation-host.py");
    let escape = harness.containment.path().join("cgroup.procs");
    let script = format!(
        r#"#!/usr/bin/python3
import ctypes
import errno
import json
import os
import time

libc = ctypes.CDLL(None, use_errno=True)

class IOVec(ctypes.Structure):
    _fields_ = [("base", ctypes.c_void_p), ("length", ctypes.c_size_t)]

def call(name, function):
    ctypes.set_errno(0)
    result = function()
    values[name] = [int(result), ctypes.get_errno()]

values = {{}}
targets = {{"worker": {worker_pid}, "ordinary": {ordinary_pid}, "sibling": {sibling_pid}}}
for name, pid in targets.items():
    call("ptrace_" + name, lambda pid=pid: libc.ptrace(16, pid, 0, 0))
    try:
        with open("/proc/%d/mem" % pid, "rb", buffering=0):
            values["mem_" + name] = [0, 0]
    except OSError as error:
        values["mem_" + name] = [-1, error.errno]

local = ctypes.c_long(7)
local_iov = IOVec(ctypes.addressof(local), ctypes.sizeof(local))
remote_iov = IOVec(ctypes.addressof(local), ctypes.sizeof(local))
call("ptrace_self", lambda: libc.ptrace(0, 0, 0, 0))
call("dumpable", lambda: libc.prctl(3, 0, 0, 0, 0))
call("process_vm_readv_self", lambda: libc.syscall({readv}, os.getpid(), ctypes.byref(local_iov), 1, ctypes.byref(remote_iov), 1, 0))
call("process_vm_writev_self", lambda: libc.syscall({writev}, os.getpid(), ctypes.byref(local_iov), 1, ctypes.byref(remote_iov), 1, 0))
call("kcmp_self", lambda: libc.syscall({kcmp}, os.getpid(), os.getpid(), 0, 0, 0))
call("pidfd_getfd", lambda: libc.syscall({pidfd_getfd}, -1, 0, 0))

try:
    with open({escape:?}, "w", encoding="utf-8") as output:
        output.write(str(os.getpid()))
    values["cgroup_escape"] = [0, 0]
except OSError as error:
    values["cgroup_escape"] = [-1, error.errno]

report = os.environ["COS_EXTENSION_CONTROL_SOCKET"] + ".isolation"
with open(report, "w", encoding="utf-8") as output:
    json.dump(values, output)
    output.flush()
    os.fsync(output.fileno())
while True:
    time.sleep(60)
"#,
        readv = libc::SYS_process_vm_readv,
        writev = libc::SYS_process_vm_writev,
        kcmp = libc::SYS_kcmp,
        pidfd_getfd = libc::SYS_pidfd_getfd,
    );
    std::fs::write(&path, script).expect("write process isolation host");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod process isolation host");
    path
}

fn install_private_tmp_probe(harness: &Harness) -> PathBuf {
    let path = harness.runtime_dir.join("private-tmp-host.py");
    let shared = harness.runtime_dir.join("shared-writable");
    std::fs::create_dir(&shared).expect("create shared writable fixture");
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777))
        .expect("chmod shared writable fixture");
    let persistent = serde_json::to_string(&shared.join("persistent").to_string_lossy())
        .expect("encode shared writable path");
    std::fs::write(
        &path,
        format!(
            "{}{}{}",
            concat!(
                "#!/usr/bin/python3\n",
                "import os\n",
                "import time\n",
                "shared = '/tmp/cos-extension-private-tmp-probe'\n",
                "existed = os.path.exists(shared)\n",
                "with open(shared, 'w', encoding='utf-8') as output:\n",
                "    output.write(os.environ['COS_EXTENSION_TASK_ID'])\n",
                "persistent_errno = 0\n",
                "try:\n",
                "    with open("
            ),
            persistent,
            concat!(
                ", 'w', encoding='utf-8') as output:\n",
                "        output.write('forbidden')\n",
                "except OSError as error:\n",
                "    persistent_errno = error.errno\n",
                "report = os.environ['COS_EXTENSION_CONTROL_SOCKET'] + '.tmp'\n",
                "with open(report, 'w', encoding='utf-8') as output:\n",
                "    output.write(('1' if existed else '0') + ':' + str(persistent_errno))\n",
                "    output.flush()\n",
                "    os.fsync(output.fileno())\n",
                "while True:\n",
                "    time.sleep(60)\n",
            )
        ),
    )
    .expect("write private tmp probe");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod private tmp probe");
    path
}

async fn wait_for_pid(path: &std::path::Path) -> u32 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(pid) = raw.trim().parse::<u32>() {
                let _ = std::fs::remove_file(path);
                return pid;
            }
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "daemonizing host did not publish its descendant pid"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_text(path: &std::path::Path) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(raw) = std::fs::read_to_string(path) {
            return raw;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "probe did not write {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn automatic_containment_root_child_probe() {
    let Some(gate) = std::env::var_os(CGROUP_PROBE_GATE).map(PathBuf::from) else {
        return;
    };
    let expected = PathBuf::from(std::env::var_os(CGROUP_PROBE_ROOT).expect("probe root"));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !gate.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "parent never released cgroup probe"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        std::env::var_os(cos::extension_host::spawn::CGROUP_ROOT_ENV).is_none(),
        "automatic probe must not use the configured-root path"
    );
    let containment =
        cos::extension_host::spawn::ContainmentRoot::establish().expect("automatic containment");
    assert_eq!(containment.path(), expected);
    let membership = std::fs::read_to_string("/proc/self/cgroup").expect("membership");
    assert!(
        membership.lines().any(|line| line.ends_with("/cos-broker")),
        "clawd probe did not move into its broker leaf: {membership}"
    );
}

#[test]
fn automatic_current_cgroup_setup_moves_broker_and_enables_controllers() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let probe_root = PathBuf::from(format!(
        "/sys/fs/cgroup/cos-auto-test-{}",
        uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(12)
            .collect::<String>()
    ));
    std::fs::create_dir(&probe_root).expect("create automatic cgroup probe");
    let stale = probe_root.join("cos-extension-stale");
    std::fs::create_dir(&stale).expect("create stale extension cgroup");
    let mut stale_child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn stale extension descendant");
    let stale_pid = stale_child.id();
    std::fs::write(stale.join("cgroup.procs"), stale_pid.to_string())
        .expect("move stale descendant into cgroup");
    let gate = harness.runtime_dir.join("release-cgroup-probe");
    let child = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "automatic_containment_root_child_probe",
            "--nocapture",
        ])
        .env(CGROUP_PROBE_GATE, &gate)
        .env(CGROUP_PROBE_ROOT, &probe_root)
        .env_remove(cos::extension_host::spawn::CGROUP_ROOT_ENV)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn automatic cgroup probe");
    let child_pid = child.id();
    std::fs::write(probe_root.join("cgroup.procs"), child_pid.to_string())
        .expect("move probe into delegated cgroup");
    std::fs::write(&gate, b"go").expect("release cgroup probe");
    let output = child.wait_with_output().expect("wait cgroup probe");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let stale_cleaned = loop {
        if stale_child
            .try_wait()
            .expect("stale child status")
            .is_some()
        {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !stale_cleaned {
        let _ = std::fs::write(probe_root.join("cgroup.kill"), b"1");
    }
    let _ = stale_child.wait();
    let stale_removed_by_setup = !stale.exists();
    let broker = probe_root.join("cos-broker");
    let _ = std::fs::write(probe_root.join("cgroup.kill"), b"1");
    let _ = std::fs::remove_dir(&stale);
    let _ = std::fs::remove_dir(&broker);
    let _ = std::fs::remove_dir(&probe_root);
    assert!(
        output.status.success(),
        "automatic cgroup setup failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stale_cleaned && !cos::proc::is_pid_alive(stale_pid),
        "automatic setup left a stale extension descendant alive"
    );
    assert!(
        stale_removed_by_setup,
        "automatic setup left a stale cgroup"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_inherits_no_broker_descriptor_environment_or_privilege() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut worker =
        spawn::spawn_worker(&harness.identity, &harness.isolation, "task-boundary").expect("spawn");
    let grant = grant_for(
        &signer,
        "task-boundary",
        &harness.identity,
        harness.isolation.execution_gid(),
        worker.pid,
        worker.start_time_ticks,
    );
    prepare_and_commit(
        &mut worker,
        assignment(
            grant,
            protocol::PROTOCOL_VERSION,
            "task-boundary",
            harness.identity.uid,
            &harness.identity.home,
        ),
    )
    .await;

    let SpawnedWorker {
        mut child,
        channel,
        pid,
        ..
    } = worker;
    let (reader, _writer) = channel.into_split();
    let mut frames = FrameReader::new(BufReader::new(reader));
    let hello = tokio::time::timeout(Duration::from_secs(30), frames.next_frame::<WorkerFrame>())
        .await
        .expect("handshake timed out")
        .expect("read handshake")
        .expect("a handshake frame");
    let WorkerFrame::Hello(hello) = hello else {
        panic!("the worker must handshake before anything else");
    };

    assert_eq!(hello.protocol, protocol::PROTOCOL_VERSION);
    assert_eq!(hello.pid, pid);
    assert_eq!(hello.uid, harness.identity.uid);
    assert_eq!(hello.euid, harness.identity.uid);
    assert_eq!(hello.gid, harness.isolation.execution_gid());
    assert_eq!(hello.egid, harness.isolation.execution_gid());
    assert!(hello.supplementary_groups.is_empty());
    assert!(!hello.dumpable, "agent worker remained dumpable");
    let primary = std::fs::metadata(&harness.primary_path).expect("primary broker socket");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        primary.gid(),
        harness.identity.gid,
        "fixture must model an owner whose primary gid can reach clawd"
    );
    assert_ne!(hello.gid, primary.gid());
    assert_ne!(hello.uid, 0, "the agent runtime must never run as root");
    assert!(hello.no_new_privs, "the worker reported no NNP");

    // Kernel's own view, not the worker's self-report.
    assert_eq!(
        proc_field(pid, "NoNewPrivs:").as_deref(),
        Some("1"),
        "PR_SET_NO_NEW_PRIVS is not set on the worker process"
    );
    assert_eq!(
        proc_field(pid, "Uid:")
            .and_then(|uid| uid.split_whitespace().next().map(str::to_string))
            .as_deref(),
        Some(harness.identity.uid.to_string().as_str())
    );

    let descriptors = open_descriptor_targets(pid);
    assert!(
        !descriptors.is_empty(),
        "expected to be able to inspect the worker's descriptors"
    );
    assert!(
        !descriptors.contains(&harness.leaked_path),
        "the worker inherited a broker-held descriptor: {descriptors:?}"
    );
    assert_ne!(
        descriptor_flags(pid, protocol::CHANNEL_FD).expect("inspect worker channel flags")
            & libc::O_CLOEXEC as u32,
        0,
        "the adopted worker channel must close across descendant exec"
    );

    let environ = std::fs::read(format!("/proc/{pid}/environ")).expect("read environ");
    let environ = String::from_utf8_lossy(&environ);
    assert!(
        !environ.contains(LEAK_MARKER),
        "the worker inherited the broker's environment"
    );
    // `/proc/<pid>/environ` exposes the exec-time environment block and may
    // retain a key after libc removes it. The worker unit test checks the live
    // environment; the descendant exec test checks that it is not propagated.
    assert!(!environ.contains(protocol::TASK_HINT_ENV));

    let same_uid_mem = std::process::Command::new("setpriv")
        .args([
            format!("--reuid={}", harness.identity.uid),
            format!("--regid={}", harness.identity.gid),
            "--clear-groups".to_string(),
            "python3".to_string(),
            "-c".to_string(),
            format!("open('/proc/{pid}/mem', 'rb', buffering=0)"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("probe worker mem access");
    assert!(
        !same_uid_mem.success(),
        "same-uid sibling opened the non-dumpable worker memory"
    );

    // The task still round-trips a result even with no provider
    // configured, which is what proves the queue's outcome now arrives
    // from outside the broker. Stream, progress and audit frames may
    // precede it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let mut saw_audit = false;
    loop {
        let frame = tokio::time::timeout_at(deadline, frames.next_frame::<WorkerFrame>())
            .await
            .expect("result timed out")
            .expect("read frame");
        match frame {
            Some(WorkerFrame::Result { task_id, outcome }) => {
                assert_eq!(task_id, "task-boundary");
                assert!(matches!(
                    *outcome,
                    WorkerOutcome::Ok(_) | WorkerOutcome::Error { .. }
                ));
                break;
            }

            Some(WorkerFrame::Audit { task_id, .. }) => {
                assert_eq!(task_id, "task-boundary");
                saw_audit = true;
            }
            Some(WorkerFrame::Stream { task_id, .. })
            | Some(WorkerFrame::Progress { task_id, .. })
            | Some(WorkerFrame::Heartbeat { task_id }) => {
                assert_eq!(task_id, "task-boundary");
            }
            other => panic!("expected a result frame, got {other:?}"),
        }
    }
    assert!(
        saw_audit,
        "the worker must forward its runtime audit to the broker"
    );

    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("worker did not exit")
        .expect("wait");
    assert!(status.success(), "worker exited with {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_for_another_worker_cannot_release_execution() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut first =
        spawn::spawn_worker(&harness.identity, &harness.isolation, "commit-first").expect("first");
    let mut second = spawn::spawn_worker(&harness.identity, &harness.isolation, "commit-second")
        .expect("second");
    let first_assignment = assignment(
        grant_for(
            &signer,
            "commit-first",
            &harness.identity,
            harness.isolation.execution_gid(),
            first.pid,
            first.start_time_ticks,
        ),
        protocol::PROTOCOL_VERSION,
        "commit-first",
        harness.identity.uid,
        &harness.identity.home,
    );
    let second_assignment = assignment(
        grant_for(
            &signer,
            "commit-second",
            &harness.identity,
            harness.isolation.execution_gid(),
            second.pid,
            second.start_time_ticks,
        ),
        protocol::PROTOCOL_VERSION,
        "commit-second",
        harness.identity.uid,
        &harness.identity.home,
    );
    send(
        &mut first,
        &BrokerFrame::Prepare(Box::new(first_assignment.clone())),
    )
    .await;
    send(
        &mut second,
        &BrokerFrame::Prepare(Box::new(second_assignment)),
    )
    .await;
    for worker in [&mut first, &mut second] {
        let mut frames = FrameReader::new(BufReader::new(&mut worker.channel));
        let frame = frames.next_frame::<WorkerFrame>().await.unwrap().unwrap();
        assert!(matches!(frame, WorkerFrame::Prepared(_)));
    }
    send(
        &mut second,
        &BrokerFrame::Commit(Box::new(ExecutionCommit {
            protocol: protocol::PROTOCOL_VERSION,
            grant: first_assignment.grant.clone(),
            task_id: first_assignment.job.id.clone(),
            session_id: first_assignment.job.session_id.clone(),
            worker_pid: first.pid,
            worker_start_time_ticks: first.start_time_ticks,
            capability_generation: first_assignment.grant.claims.capability_generation.clone(),
            prepare_nonce: first_assignment.grant.claims.prepare_nonce.clone(),
            commit_nonce: first_assignment.grant.claims.commit_nonce.clone(),
        })),
    )
    .await;
    let status = tokio::time::timeout(Duration::from_secs(30), second.child.wait())
        .await
        .expect("cross-worker commit did not terminate")
        .expect("wait for second worker");
    assert!(!status.success(), "cross-worker commit was accepted");
    let _ = first.child.start_kill();
    unsafe {
        spawn::terminate_worker_group(first.pid, libc::SIGKILL);
    }
    let _ = first.child.wait().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_host_uses_dedicated_gid_and_cannot_open_primary_broker() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let paths =
        cos::extension_host::spawn::HostPaths::create(&harness.identity).expect("host paths");
    let _private_listener = cos::extension_host::broker::bind_listener(
        &paths,
        harness.extension_identity.uid,
        harness.isolation.execution_gid(),
    )
    .expect("private broker listener");
    let expires = cos::agentd::grant::now_ms() + 60_000;
    let mut host = cos::extension_host::spawn::spawn_host(
        &harness.identity,
        &harness.extension_identity,
        &harness.isolation,
        &harness.containment,
        "gid-boundary",
        None,
        None,
        std::process::id(),
        {
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id()))
                .expect("test process stat");
            stat[stat.rfind(')').unwrap() + 1..]
                .split_whitespace()
                .nth(19)
                .and_then(|value| value.parse::<u64>().ok())
        },
        "0123456789abcdef0123456789abcdef",
        expires,
        paths,
    )
    .expect("spawn host");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(host.child.try_wait().expect("host status").is_none());
    assert_eq!(
        status_ids(host.pid, "Uid:"),
        vec![harness.extension_identity.uid; 4]
    );
    assert_ne!(harness.extension_identity.uid, harness.identity.uid);
    assert_eq!(
        status_ids(host.pid, "Gid:"),
        vec![harness.isolation.execution_gid(); 4]
    );
    assert!(
        status_ids(host.pid, "Groups:").is_empty(),
        "extension host retained supplementary groups"
    );
    let descriptors = open_descriptor_targets(host.pid);
    assert!(
        !descriptors
            .iter()
            .any(|target| target.ends_with("cgroup.procs")),
        "extension host inherited its cgroup attachment descriptor: {descriptors:?}"
    );
    assert!(
        !descriptors
            .iter()
            .any(|target| target.starts_with(&host.paths.dir)),
        "extension host inherited a broker path descriptor: {descriptors:?}"
    );
    let environ = std::fs::read(format!("/proc/{}/environ", host.pid)).expect("host environment");
    let environ = String::from_utf8_lossy(&environ);
    assert!(
        !environ.contains(cos::extension_host::spawn::CGROUP_ROOT_ENV),
        "extension host inherited broker cgroup configuration"
    );
    assert!(
        !environ.contains(harness.identity.home.to_string_lossy().as_ref()),
        "extension host inherited the task owner's home path"
    );
    assert!(
        environ.contains(host.paths.control_dir.to_string_lossy().as_ref()),
        "extension host did not receive its controlled task-local home"
    );
    let socket = std::fs::metadata(&harness.primary_path).expect("primary socket");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(socket.gid(), harness.identity.gid);
    assert_ne!(socket.gid(), harness.isolation.execution_gid());
    let members = std::fs::read_to_string(host.cgroup.path().join("cgroup.procs"))
        .expect("read host cgroup membership");
    assert!(
        members.lines().any(|member| member == host.pid.to_string()),
        "host was not attached before spawn returned: {members}"
    );
    for (name, expected) in [
        ("pids.max", "128"),
        ("memory.max", "1073741824"),
        ("memory.oom.group", "1"),
        ("cpu.max", "100000 100000"),
    ] {
        let actual = std::fs::read_to_string(host.cgroup.path().join(name))
            .unwrap_or_else(|error| panic!("read {name}: {error}"));
        assert_eq!(actual.trim(), expected, "{name}");
    }

    let cgroup_path = host.cgroup.path().to_path_buf();
    host.cgroup.cleanup().await.expect("clean host cgroup");
    let _ = host.child.wait().await;
    host.cleanup_private_mounts()
        .expect("clean private tmp mounts");
    assert!(
        !cgroup_path.exists(),
        "task completion left its containment cgroup behind"
    );
    host.paths.cleanup().expect("clean host paths");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_owned_roots_are_hardened_before_an_extension_task() {
    use std::os::unix::fs::MetadataExt;

    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let logs = harness.runtime_dir.join("service-logs");
    std::fs::create_dir(&logs).expect("create service logs");
    for path in [&harness.runtime_dir, harness._data.path(), &logs] {
        let raw = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("root path");
        assert_eq!(
            unsafe { libc::chown(raw.as_ptr(), 0, harness.identity.gid) },
            0,
            "model systemd root group"
        );
    }
    let previous_log = std::env::var_os("COS_LOG_DIR");
    std::env::set_var("COS_LOG_DIR", &logs);
    cos::storage::harden_clawd_runtime().expect("harden runtime root");
    cos::storage::harden_clawd_state().expect("harden state roots");
    assert_eq!(std::fs::metadata(&harness.runtime_dir).unwrap().gid(), 0);
    assert_eq!(std::fs::metadata(harness._data.path()).unwrap().gid(), 0);
    assert_eq!(std::fs::metadata(&logs).unwrap().gid(), 0);
    assert_eq!(
        std::fs::metadata(&harness.primary_path).unwrap().gid(),
        harness.identity.gid,
        "hardening roots must not change primary socket access"
    );

    let paths =
        cos::extension_host::spawn::HostPaths::create(&harness.identity).expect("host paths");
    let _listener = cos::extension_host::broker::bind_listener(
        &paths,
        harness.extension_identity.uid,
        harness.isolation.execution_gid(),
    )
    .expect("private broker listener");
    let mut host = cos::extension_host::spawn::spawn_host(
        &harness.identity,
        &harness.extension_identity,
        &harness.isolation,
        &harness.containment,
        "service-root-task",
        None,
        None,
        std::process::id(),
        process_start(std::process::id()),
        "0123456789abcdef0123456789abcdef",
        cos::agentd::grant::now_ms() + 60_000,
        paths,
    )
    .expect("launch extension task after service root hardening");
    assert!(host.child.try_wait().expect("host status").is_none());
    host.cgroup.cleanup().await.expect("clean host cgroup");
    let _ = host.child.wait().await;
    host.cleanup_private_mounts()
        .expect("clean private tmp mounts");
    host.paths.cleanup().expect("clean host paths");

    match previous_log {
        Some(value) => std::env::set_var("COS_LOG_DIR", value),
        None => std::env::remove_var("COS_LOG_DIR"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_uid_hosts_receive_distinct_private_tmp_mounts() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let probe = install_private_tmp_probe(&harness);
    std::env::set_var(cos::extension_host::spawn::HOST_BINARY_ENV, &probe);
    let start_time = {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id()))
            .expect("test process stat");
        stat[stat.rfind(')').unwrap() + 1..]
            .split_whitespace()
            .nth(19)
            .and_then(|value| value.parse::<u64>().ok())
    };
    let mut hosts = Vec::new();
    let mut listeners = Vec::new();
    for task in ["private-tmp-a", "private-tmp-b"] {
        let paths =
            cos::extension_host::spawn::HostPaths::create(&harness.identity).expect("host paths");
        listeners.push(
            cos::extension_host::broker::bind_listener(
                &paths,
                harness.extension_identity.uid,
                harness.isolation.execution_gid(),
            )
            .expect("private broker listener"),
        );
        let report = PathBuf::from(format!("{}.tmp", paths.control_socket.to_string_lossy()));
        let host = cos::extension_host::spawn::spawn_host(
            &harness.identity,
            &harness.extension_identity,
            &harness.isolation,
            &harness.containment,
            task,
            None,
            None,
            std::process::id(),
            start_time,
            "0123456789abcdef0123456789abcdef",
            cos::agentd::grant::now_ms() + 60_000,
            paths,
        )
        .expect("spawn private tmp probe");
        let result = wait_for_text(&report).await;
        let (tmp_seen, persistent_errno) = result.split_once(':').expect("tmp probe result");
        assert_eq!(tmp_seen, "0");
        assert_ne!(
            persistent_errno, "0",
            "host wrote outside its task-private mount tree"
        );
        hosts.push(host);
    }
    assert!(
        !harness
            .runtime_dir
            .join("shared-writable/persistent")
            .exists(),
        "extension left persistent state outside its task tree"
    );
    drop(listeners);
    hosts[0]
        .mount_private_tmp_test_child()
        .expect("mount nested private tmpfs");
    for (index, host) in hosts.iter_mut().enumerate() {
        host.cgroup.cleanup().await.expect("clean host cgroup");
        let _ = host.child.wait().await;
        if index == 0 {
            assert!(
                host.cleanup_private_mounts().is_err(),
                "nested mount must block parent tmpfs cleanup"
            );
            host.unmount_private_tmp_test_child()
                .expect("remove nested private tmpfs");
        }
        host.cleanup_private_mounts()
            .expect("clean private tmp mounts");
        host.paths.cleanup().expect("clean host paths");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_uid_and_seccomp_block_process_injection_and_cgroup_escape() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let pool = cos::extension_host::identity::ExtensionIdentityPool::for_test(
        harness.isolation.execution_gid(),
    );
    assert_eq!(pool.len(), 64);
    let first = pool.acquire(harness.identity.uid).expect("first uid lease");
    let second = pool
        .acquire(harness.identity.uid)
        .expect("second uid lease");
    assert_ne!(first.identity().uid, second.identity().uid);
    let independent_pool = cos::extension_host::identity::ExtensionIdentityPool::for_test(
        harness.isolation.execution_gid(),
    );
    let third = independent_pool
        .acquire(harness.identity.uid)
        .expect("cross-pool uid lease");
    assert_ne!(third.identity().uid, first.identity().uid);
    assert_ne!(third.identity().uid, second.identity().uid);
    first.release().expect("release first identity");
    second.release().expect("release second identity");
    third.release().expect("release cross-process identity");

    let mut worker = spawn::spawn_worker(&harness.identity, &harness.isolation, "ptrace-target")
        .expect("worker");
    let worker_pid = worker.pid;

    let mut ordinary_command = std::process::Command::new("sleep");
    ordinary_command.arg("60");
    let mut ordinary = drop_command_identity(
        &mut ordinary_command,
        harness.identity.uid,
        harness.identity.gid,
    )
    .spawn()
    .expect("ordinary same-user process");

    let sibling_uid = harness.extension_identity.uid + 1;
    let mut sibling_command = std::process::Command::new("sleep");
    sibling_command.arg("60");
    let mut sibling = drop_command_identity(
        &mut sibling_command,
        sibling_uid,
        harness.isolation.execution_gid(),
    )
    .spawn()
    .expect("sibling extension process");

    let probe = install_process_isolation_probe(&harness, worker_pid, ordinary.id(), sibling.id());
    std::env::set_var(cos::extension_host::spawn::HOST_BINARY_ENV, &probe);
    let paths =
        cos::extension_host::spawn::HostPaths::create(&harness.identity).expect("host paths");
    let _private_listener = cos::extension_host::broker::bind_listener(
        &paths,
        harness.extension_identity.uid,
        harness.isolation.execution_gid(),
    )
    .expect("activate secure host paths");
    let report = PathBuf::from(format!(
        "{}.isolation",
        paths.control_socket.to_string_lossy()
    ));
    let mut host = cos::extension_host::spawn::spawn_host(
        &harness.identity,
        &harness.extension_identity,
        &harness.isolation,
        &harness.containment,
        "process-isolation",
        None,
        None,
        std::process::id(),
        process_start(std::process::id()),
        "0123456789abcdef0123456789abcdef",
        cos::agentd::grant::now_ms() + 60_000,
        paths,
    )
    .expect("spawn process isolation probe");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let values: serde_json::Value = loop {
        if let Ok(raw) = std::fs::read_to_string(&report) {
            if let Ok(values) = serde_json::from_str(&raw) {
                break values;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "extension isolation probe did not report"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let _ = std::fs::remove_file(&report);

    let host_uids = status_ids(host.pid, "Uid:");
    let seccomp = proc_field(host.pid, "Seccomp:");
    host.cgroup.cleanup().await.expect("clean probe cgroup");
    let _ = host.child.wait().await;
    host.cleanup_private_mounts()
        .expect("clean private tmp mounts");
    host.paths.cleanup().expect("clean host paths");
    let _ = worker.child.start_kill();
    unsafe {
        spawn::terminate_worker_group(worker_pid, libc::SIGKILL);
    }
    let _ = worker.child.wait().await;
    let _ = ordinary.kill();
    let _ = ordinary.wait();
    let _ = sibling.kill();
    let _ = sibling.wait();

    assert_eq!(host_uids, vec![harness.extension_identity.uid; 4]);
    assert_eq!(seccomp.as_deref(), Some("2"));
    for name in [
        "ptrace_worker",
        "ptrace_ordinary",
        "ptrace_sibling",
        "ptrace_self",
        "process_vm_readv_self",
        "process_vm_writev_self",
        "kcmp_self",
        "pidfd_getfd",
    ] {
        assert_eq!(
            values[name],
            serde_json::json!([-1, libc::EPERM]),
            "{name}: {values}"
        );
    }
    for name in ["mem_worker", "mem_ordinary", "mem_sibling", "cgroup_escape"] {
        assert_eq!(values[name][0], -1, "{name}: {values}");
        assert!(
            matches!(
                values[name][1].as_i64(),
                Some(value) if value == i64::from(libc::EACCES)
                    || value == i64::from(libc::EPERM)
                    || value == i64::from(libc::EROFS)
            ),
            "{name}: {values}"
        );
    }
    assert_ne!(harness.extension_identity.uid, harness.identity.uid);
    assert_ne!(harness.extension_identity.uid, sibling_uid);
}

#[tokio::test(flavor = "current_thread")]
async fn root_path_setup_resists_legacy_symlinks_hardlinks_and_replacement_races() {
    use std::os::unix::fs::{symlink, FileTypeExt, MetadataExt};

    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let sentinel = harness.runtime_dir.join("root-sentinel");
    std::fs::write(&sentinel, b"root-only").expect("write sentinel");
    std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o640))
        .expect("chmod sentinel");
    let original = std::fs::metadata(&sentinel).expect("sentinel metadata");
    assert_eq!(original.uid(), 0);
    assert_eq!(original.gid(), 0);

    let owner = harness
        .runtime_dir
        .join("extension-hosts")
        .join(harness.identity.uid.to_string());
    std::fs::create_dir_all(&owner).expect("create legacy owner tree");
    let owner_raw = std::ffi::CString::new(owner.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::chown(
                owner_raw.as_ptr(),
                harness.identity.uid,
                harness.identity.gid,
            )
        },
        0
    );
    std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700))
        .expect("chmod legacy owner");
    symlink(&sentinel, owner.join("sentinel-symlink")).expect("legacy symlink");
    std::fs::hard_link(&sentinel, owner.join("sentinel-hardlink")).expect("legacy hardlink");

    let mut legacy_racer = std::process::Command::new("sh");
    legacy_racer.args([
        "-c",
        &format!(
            "while :; do ln -sf '{}' '{}/race-link' 2>/dev/null; rm -f '{}/race-link' 2>/dev/null; done",
            sentinel.display(),
            owner.display(),
            owner.display()
        ),
    ]);
    let mut legacy_racer = drop_command_identity(
        &mut legacy_racer,
        harness.identity.uid,
        harness.identity.gid,
    )
    .spawn()
    .expect("spawn legacy path racer");
    std::thread::sleep(Duration::from_millis(25));

    let paths =
        cos::extension_host::spawn::HostPaths::create(&harness.identity).expect("secure paths");
    let _ = legacy_racer.kill();
    let _ = legacy_racer.wait();

    let owner_meta = std::fs::metadata(&owner).expect("owner root metadata");
    assert_eq!(owner_meta.uid(), 0);
    assert_eq!(owner_meta.gid(), 0);
    assert_eq!(owner_meta.mode() & 0o7777, 0o711);
    assert!(!owner.join("sentinel-symlink").exists());
    assert!(!owner.join("sentinel-hardlink").exists());

    let mut socket_racer = std::process::Command::new("sh");
    socket_racer.args([
        "-c",
        &format!(
            "while :; do ln -sf '{}' '{}' 2>/dev/null; rm -f '{}' 2>/dev/null; done",
            sentinel.display(),
            paths.broker_socket.display(),
            paths.broker_socket.display()
        ),
    ]);
    let mut socket_racer = drop_command_identity(
        &mut socket_racer,
        harness.identity.uid,
        harness.identity.gid,
    )
    .spawn()
    .expect("spawn broker socket racer");
    let listener = cos::extension_host::broker::bind_listener(
        &paths,
        harness.extension_identity.uid,
        harness.isolation.execution_gid(),
    )
    .expect("bind broker listener through pinned task directory");
    let _ = socket_racer.kill();
    let _ = socket_racer.wait();

    let task_meta = std::fs::metadata(&paths.dir).expect("task directory metadata");
    assert_eq!(task_meta.uid(), 0);
    assert_eq!(task_meta.gid(), 0);
    assert_eq!(task_meta.mode() & 0o7777, 0o711);
    let control_meta = std::fs::metadata(&paths.control_dir).expect("control directory metadata");
    assert_eq!(control_meta.uid(), harness.extension_identity.uid);
    assert_eq!(control_meta.gid(), harness.isolation.execution_gid());
    assert_eq!(control_meta.mode() & 0o7777, 0o710);
    let socket_meta = std::fs::symlink_metadata(&paths.broker_socket).expect("broker socket");
    assert!(socket_meta.file_type().is_socket());
    assert_eq!(socket_meta.uid(), harness.extension_identity.uid);
    assert_eq!(socket_meta.gid(), harness.isolation.execution_gid());
    assert_eq!(socket_meta.mode() & 0o7777, 0o600);

    let after = std::fs::metadata(&sentinel).expect("sentinel after races");
    assert_eq!(after.uid(), original.uid());
    assert_eq!(after.gid(), original.gid());
    assert_eq!(after.mode() & 0o7777, original.mode() & 0o7777);
    assert_eq!(
        std::fs::read(&sentinel).expect("read sentinel"),
        b"root-only"
    );

    drop(listener);
    let control_symlink = std::process::Command::new("setpriv")
        .args([
            format!("--reuid={}", harness.extension_identity.uid),
            format!("--regid={}", harness.isolation.execution_gid()),
            "--clear-groups".to_string(),
            "ln".to_string(),
            "-s".to_string(),
            sentinel.to_string_lossy().into_owned(),
            paths.control_socket.to_string_lossy().into_owned(),
        ])
        .status()
        .expect("create adversarial control symlink");
    assert!(control_symlink.success());
    paths.cleanup().expect("clean adversarial host paths");
    let after_cleanup = std::fs::metadata(&sentinel).expect("sentinel after cleanup");
    assert_eq!(after_cleanup.uid(), original.uid());
    assert_eq!(after_cleanup.gid(), original.gid());
    assert_eq!(after_cleanup.mode() & 0o7777, original.mode() & 0o7777);
}

#[test]
fn symlinked_extension_owner_root_fails_without_mutating_its_target() {
    use std::os::unix::fs::{symlink, MetadataExt};

    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let target = harness.runtime_dir.join("sentinel-directory");
    std::fs::create_dir(&target).expect("create sentinel directory");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750))
        .expect("chmod sentinel directory");
    let base = harness.runtime_dir.join("extension-hosts");
    std::fs::create_dir(&base).expect("create extension base");
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o711))
        .expect("chmod extension base");
    let owner = base.join(harness.identity.uid.to_string());
    symlink(&target, &owner).expect("symlink owner root");
    let original = std::fs::metadata(&target).expect("sentinel identity");

    let error = cos::extension_host::spawn::HostPaths::create(&harness.identity)
        .expect_err("symlinked owner root must fail closed");
    assert!(
        error.contains("pin owner extension directory") || error.contains("Too many levels"),
        "{error}"
    );
    let after = std::fs::metadata(&target).expect("sentinel after refusal");
    assert_eq!(after.uid(), original.uid());
    assert_eq!(after.gid(), original.gid());
    assert_eq!(after.mode() & 0o7777, original.mode() & 0o7777);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_mount_cleanup_quarantines_uid_until_restart_recovery() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let quarantine = harness.runtime_dir.join("extension-quarantine");
    let pool = cos::extension_host::identity::ExtensionIdentityPool::for_test_with_quarantine(
        harness.isolation.execution_gid(),
        quarantine.clone(),
    )
    .expect("create quarantine pool");
    let mut lease = pool
        .acquire(harness.identity.uid)
        .expect("acquire quarantined identity");
    lease
        .begin_task(harness.identity.uid)
        .expect("record active identity");
    let paths =
        cos::extension_host::spawn::HostPaths::create(&harness.identity).expect("host paths");
    lease
        .record_task(harness.identity.uid, paths.task_name())
        .expect("record task paths");
    let listener = cos::extension_host::broker::bind_listener(
        &paths,
        lease.identity().uid,
        harness.isolation.execution_gid(),
    )
    .expect("bind extension listener");

    let source = harness.runtime_dir.join("mount-source");
    let mountpoint = paths.control_dir.join("mounted");
    std::fs::create_dir(&source).expect("create mount source");
    std::fs::create_dir(&mountpoint).expect("create mountpoint");
    std::fs::write(source.join("residual"), b"quarantined").expect("write mounted residual");
    let source_raw =
        std::ffi::CString::new(source.as_os_str().as_encoded_bytes()).expect("source path");
    let mount_raw =
        std::ffi::CString::new(mountpoint.as_os_str().as_encoded_bytes()).expect("mount path");
    assert_eq!(
        unsafe {
            libc::mount(
                source_raw.as_ptr(),
                mount_raw.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        },
        0,
        "bind mount failed: {}",
        std::io::Error::last_os_error()
    );

    let first_uid = lease.identity().uid;
    drop(listener);
    drop(paths);
    drop(lease);
    drop(pool);
    assert!(quarantine.join(format!("{first_uid}.state")).exists());

    let blocked = cos::extension_host::identity::ExtensionIdentityPool::for_test_with_quarantine(
        harness.isolation.execution_gid(),
        quarantine.clone(),
    )
    .expect("reload quarantine pool");
    let alternate = blocked
        .acquire(harness.identity.uid)
        .expect("use a different identity while first is quarantined");
    assert_ne!(alternate.identity().uid, first_uid);
    alternate.release().expect("release alternate identity");
    drop(blocked);

    assert_eq!(
        unsafe { libc::umount2(mount_raw.as_ptr(), 0) },
        0,
        "unmount recovery fixture: {}",
        std::io::Error::last_os_error()
    );
    let recovered = cos::extension_host::identity::ExtensionIdentityPool::for_test_with_quarantine(
        harness.isolation.execution_gid(),
        quarantine,
    )
    .expect("recover quarantined identity");
    let reused = recovered
        .acquire(harness.identity.uid)
        .expect("reuse recovered identity");
    assert_eq!(reused.identity().uid, first_uid);
    reused.release().expect("release recovered identity");
}

#[test]
fn extension_identity_pool_exhausts_without_sharing_and_reuses_after_release() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let pool = cos::extension_host::identity::ExtensionIdentityPool::for_test(
        harness.isolation.execution_gid(),
    );
    let mut leases = Vec::new();
    for _ in 0..pool.len() {
        leases.push(
            pool.acquire(harness.identity.uid)
                .expect("acquire unique extension identity"),
        );
    }
    let mut uids = leases
        .iter()
        .map(|lease| lease.identity().uid)
        .collect::<Vec<_>>();
    uids.sort_unstable();
    uids.dedup();
    assert_eq!(uids.len(), pool.len());
    assert!(pool.acquire(harness.identity.uid).is_err());
    for lease in leases {
        lease.release().expect("release exhausted identity");
    }
    let reused = pool
        .acquire(harness.identity.uid.saturating_add(1))
        .expect("reuse identity for a different owner");
    assert_eq!(
        reused.identity().uid,
        cos::extension_host::identity::FIRST_UID
    );
    reused.release().expect("release reused identity");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mandatory_cgroup_kills_host_first_double_fork_setsid_and_cleared_pdeathsig() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let malicious = install_daemonizing_host(&harness, true);
    std::env::set_var(cos::extension_host::spawn::HOST_BINARY_ENV, &malicious);
    let paths =
        cos::extension_host::spawn::HostPaths::create(&harness.identity).expect("host paths");
    let _private_listener = cos::extension_host::broker::bind_listener(
        &paths,
        harness.extension_identity.uid,
        harness.isolation.execution_gid(),
    )
    .expect("activate secure host paths");
    let escaped_marker = PathBuf::from(format!(
        "{}.escaped",
        paths.control_socket.to_string_lossy()
    ));
    let expires = cos::agentd::grant::now_ms() + 60_000;
    let mut host = cos::extension_host::spawn::spawn_host(
        &harness.identity,
        &harness.extension_identity,
        &harness.isolation,
        &harness.containment,
        "daemonized-descendant",
        None,
        None,
        std::process::id(),
        process_start(std::process::id()),
        "0123456789abcdef0123456789abcdef",
        expires,
        paths,
    )
    .expect("spawn daemonizing host");
    let escaped = wait_for_pid(&escaped_marker).await;
    let status = tokio::time::timeout(Duration::from_secs(5), host.child.wait())
        .await
        .expect("malicious host did not exit first")
        .expect("wait malicious host");
    assert_eq!(status.code(), Some(23));
    assert!(
        cos::proc::is_pid_alive(escaped),
        "daemonized descendant did not survive long enough to test host-first cleanup"
    );
    assert!(
        !host.cgroup.is_empty().expect("inspect populated cgroup"),
        "escaped descendant left the mandatory cgroup"
    );

    let cgroup_path = host.cgroup.path().to_path_buf();
    host.cgroup
        .cleanup()
        .await
        .expect("kill daemonized descendant cgroup");
    for _ in 0..100 {
        if !cos::proc::is_pid_alive(escaped) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !cos::proc::is_pid_alive(escaped),
        "setsid/double-fork descendant survived cgroup.kill"
    );
    assert!(!cgroup_path.exists(), "empty cgroup was not removed");
    host.cleanup_private_mounts()
        .expect("clean private tmp mounts");
    host.paths.cleanup().expect("clean host paths");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mandatory_cgroup_kill_covers_active_cancellation_and_descendants() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let malicious = install_daemonizing_host(&harness, false);
    std::env::set_var(cos::extension_host::spawn::HOST_BINARY_ENV, &malicious);
    let paths =
        cos::extension_host::spawn::HostPaths::create(&harness.identity).expect("host paths");
    let _private_listener = cos::extension_host::broker::bind_listener(
        &paths,
        harness.extension_identity.uid,
        harness.isolation.execution_gid(),
    )
    .expect("activate secure host paths");
    let escaped_marker = PathBuf::from(format!(
        "{}.escaped",
        paths.control_socket.to_string_lossy()
    ));
    let mut host = cos::extension_host::spawn::spawn_host(
        &harness.identity,
        &harness.extension_identity,
        &harness.isolation,
        &harness.containment,
        "cancel-daemonized-descendant",
        None,
        None,
        std::process::id(),
        process_start(std::process::id()),
        "0123456789abcdef0123456789abcdef",
        cos::agentd::grant::now_ms() + 60_000,
        paths,
    )
    .expect("spawn cancellation probe");
    let escaped = wait_for_pid(&escaped_marker).await;
    assert!(host.child.try_wait().expect("host status").is_none());
    assert!(cos::proc::is_pid_alive(escaped));

    host.cgroup.kill_all().expect("cancel extension cgroup");
    host.cgroup
        .cleanup()
        .await
        .expect("verify cancelled cgroup is empty");
    let _ = tokio::time::timeout(Duration::from_secs(5), host.child.wait())
        .await
        .expect("cancelled host did not exit");
    for _ in 0..100 {
        if !cos::proc::is_pid_alive(escaped) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !cos::proc::is_pid_alive(escaped),
        "daemonized child survived active cancellation"
    );
    host.cleanup_private_mounts()
        .expect("clean private tmp mounts");
    host.paths.cleanup().expect("clean host paths");
}

#[test]
fn routed_registry_is_readable_but_not_writable_by_authorized_identities() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    const PROBE_UID: u32 = 424_242;
    let session_id = "acl-probe-session";
    cos::proc::register_session_for_owner(acl_probe_session(session_id), PROBE_UID)
        .expect("register routed ACL probe");
    cos::storage::install_routed_extension_reader(PROBE_UID, harness.extension_identity.uid)
        .expect("grant extension registry reader");
    let registry = format!("/run/cos/caps/{PROBE_UID}/proc/registry.json");

    for (uid, gid) in [
        (PROBE_UID, harness.identity.gid),
        (
            harness.extension_identity.uid,
            harness.isolation.execution_gid(),
        ),
    ] {
        let read = std::process::Command::new("setpriv")
            .args([
                format!("--reuid={uid}"),
                format!("--regid={gid}"),
                "--clear-groups".to_string(),
                "cat".to_string(),
                registry.clone(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run routed ACL reader");
        assert!(
            read.success(),
            "uid {uid} gid {gid} could not read registry"
        );

        let write = std::process::Command::new("setpriv")
            .args([
                format!("--reuid={uid}"),
                format!("--regid={gid}"),
                "--clear-groups".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                format!("printf forbidden >> '{}'", registry),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run routed ACL writer");
        assert!(
            !write.success(),
            "uid {uid} gid {gid} modified the routed registry"
        );
    }

    for (uid, gid) in [
        (harness.identity.uid, harness.isolation.execution_gid()),
        (
            harness.extension_identity.uid + 1,
            harness.isolation.execution_gid(),
        ),
        (424_243, 424_243),
    ] {
        let denied = std::process::Command::new("setpriv")
            .args([
                format!("--reuid={uid}"),
                format!("--regid={gid}"),
                "--clear-groups".to_string(),
                "cat".to_string(),
                registry.clone(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run unauthorized routed ACL reader");
        assert!(
            !denied.success(),
            "unrelated uid {uid} gid {gid} read routed registry"
        );
    }
    cos::storage::remove_routed_extension_reader(PROBE_UID, harness.extension_identity.uid)
        .expect("revoke extension registry reader");
    let revoked = std::process::Command::new("setpriv")
        .args([
            format!("--reuid={}", harness.extension_identity.uid),
            format!("--regid={}", harness.isolation.execution_gid()),
            "--clear-groups".to_string(),
            "cat".to_string(),
            registry.clone(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run revoked registry reader");
    assert!(
        !revoked.success(),
        "revoked extension uid retained ACL access"
    );
    cos::proc::deregister_session_for_owner(session_id, PROBE_UID);
    let _ = std::fs::remove_dir_all(format!("/run/cos/caps/{PROBE_UID}"));
}

#[test]
fn broker_socket_replacement_after_capture_aborts_the_exec() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    std::fs::remove_file(&harness.primary_path).expect("unlink pinned socket path");
    let _replacement =
        std::os::unix::net::UnixListener::bind(&harness.primary_path).expect("replacement socket");
    std::fs::set_permissions(
        &harness.primary_path,
        std::fs::Permissions::from_mode(0o660),
    )
    .expect("replacement mode");
    let raw = std::ffi::CString::new(harness.primary_path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::chown(raw.as_ptr(), 0, harness.identity.gid) },
        0
    );

    let error = spawn::spawn_worker(&harness.identity, &harness.isolation, "socket-swapped")
        .expect_err("socket replacement must abort before exec");
    assert!(
        error.contains("Stale file handle")
            || error.contains("Operation not permitted")
            || error.contains("os error 116"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_task_is_reported_as_cancelled_by_the_worker() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut worker =
        spawn::spawn_worker(&harness.identity, &harness.isolation, "task-cancel").expect("spawn");
    let grant = grant_for(
        &signer,
        "task-cancel",
        &harness.identity,
        harness.isolation.execution_gid(),
        worker.pid,
        worker.start_time_ticks,
    );
    prepare_and_commit(
        &mut worker,
        assignment(
            grant,
            protocol::PROTOCOL_VERSION,
            "task-cancel",
            harness.identity.uid,
            &harness.identity.home,
        ),
    )
    .await;
    // Queued behind the assignment, so the worker observes it as soon
    // as it starts watching the channel — whatever the run itself does,
    // the outcome must be a cancellation.
    send(
        &mut worker,
        &BrokerFrame::Cancel {
            task_id: "task-cancel".to_string(),
        },
    )
    .await;

    let SpawnedWorker {
        mut child, channel, ..
    } = worker;
    let (reader, _writer) = channel.into_split();
    let mut frames = FrameReader::new(BufReader::new(reader));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let frame = tokio::time::timeout_at(deadline, frames.next_frame::<WorkerFrame>())
            .await
            .expect("cancellation timed out")
            .expect("read frame");
        match frame {
            Some(WorkerFrame::Result { task_id, outcome }) => {
                assert_eq!(task_id, "task-cancel");
                assert!(
                    matches!(*outcome, WorkerOutcome::Cancelled),
                    "a cancelled task must not report a normal outcome: {outcome:?}"
                );
                break;
            }
            Some(_) => continue,
            None => panic!("the worker closed its channel without reporting a cancellation"),
        }
    }

    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("worker did not exit")
        .expect("wait");
    assert!(status.success(), "worker exited with {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_supervisor_runs_a_task_end_to_end_through_a_real_worker() {
    use cos::agent::service::{JobStatus, Store};
    use cos::agentd::supervisor::{run_with_store_and_broker, SupervisorConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let queue = tempfile::tempdir().expect("tempdir");
    let owner_home = tempfile::tempdir().expect("tempdir");
    let raw_home =
        std::ffi::CString::new(owner_home.path().as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::chown(
                raw_home.as_ptr(),
                harness.identity.uid,
                harness.identity.gid,
            )
        },
        0,
        "chown owner home"
    );
    let store = Store::with_root(queue.path().to_path_buf()).expect("store");
    let job = store
        .submit(
            "supervisor end-to-end probe".to_string(),
            None,
            Some(1),
            Some(harness.identity.uid),
            // A config-less home keeps the probe hermetic: the worker
            // resolves the owner's config/credentials from here, so no
            // real provider is ever contacted.
            Some(owner_home.path().to_string_lossy().into_owned()),
        )
        .expect("submit");

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = SupervisorConfig {
        poll: Duration::from_millis(50),
        max_workers: 1,
        lease: Duration::from_secs(120),
        heartbeat_grace: Duration::from_secs(60),
        ..SupervisorConfig::default()
    };
    let broker = test_broker_context(&harness);
    let supervisor = tokio::spawn(run_with_store_and_broker(
        config,
        shutdown.clone(),
        store.clone(),
        broker,
    ));

    // The whole production sequence runs here: claim → spawn a real
    // `claw-agentd` → handshake → assignment → result → finish.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    let finished = loop {
        if let Some((_, current)) = store.locate(&job.id).expect("locate") {
            if matches!(
                current.status,
                JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled
            ) {
                break current;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the supervisor never finished the task"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    shutdown.store(true, Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_secs(30), supervisor).await;

    // The task was executed by a separate process, not by this one.
    let worker_pid = finished.worker_pid.expect("a worker pid");
    assert_ne!(
        worker_pid,
        std::process::id(),
        "the task must be executed outside the supervising process"
    );
    assert!(finished.started_at.is_some());
    assert!(finished.finished_at.is_some());
    // With no provider configured for the owner, the honest outcome is
    // a provider failure reported *through the worker* — which is the
    // point: the queue's result now arrives from outside the broker.
    assert_eq!(finished.status, JobStatus::Error);
    let error = finished.error.unwrap_or_default();
    assert!(
        error.contains("provider"),
        "expected a provider failure from the worker, got: {error}"
    );
    assert!(
        extension_cgroups(&harness.cgroup_root).is_empty(),
        "task completion left an extension cgroup behind"
    );
    let released_pool = cos::extension_host::identity::ExtensionIdentityPool::for_test(
        harness.isolation.execution_gid(),
    );
    let released = released_pool
        .acquire(harness.identity.uid)
        .expect("completed task released its extension uid");
    assert_eq!(
        released.identity().uid,
        cos::extension_host::identity::FIRST_UID
    );
    released.release().expect("release reused identity");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assignment_crash_boundaries_requeue_only_before_durable_commit() {
    use cos::agent::service::{ExecutionPhase, JobStatus, Store};
    use cos::agentd::supervisor::{run_with_store_and_broker, SupervisorConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    for (point, replay_safe) in [
        ("before_prepare", true),
        ("after_prepare", true),
        ("after_prepared", true),
        ("after_commit_record", false),
        ("during_commit", false),
        ("after_commit_send", false),
        ("after_worker_commit", false),
    ] {
        let queue = tempfile::tempdir().expect("queue");
        let store = Store::with_root(queue.path().to_path_buf()).expect("store");
        let job = store
            .submit(
                format!("assignment failpoint {point}"),
                None,
                Some(1),
                Some(harness.identity.uid),
                Some(harness.identity.home.to_string_lossy().into_owned()),
            )
            .expect("submit");
        std::env::set_var("COS_AGENTD_TEST_ASSIGNMENT_FAILPOINT", point);

        let marker = harness.runtime_dir.join(format!("commit-{point}.marker"));
        std::fs::write(&marker, b"").expect("create commit marker");
        let raw =
            std::ffi::CString::new(marker.as_os_str().as_encoded_bytes()).expect("marker path");
        assert_eq!(
            unsafe { libc::chown(raw.as_ptr(), harness.identity.uid, harness.identity.gid,) },
            0,
            "chown commit marker"
        );
        std::env::set_var("COS_AGENTD_TEST_COMMIT_MARKER", &marker);

        let shutdown = Arc::new(AtomicBool::new(false));
        let config = SupervisorConfig {
            poll: Duration::from_millis(25),
            max_workers: 1,
            lease: Duration::from_secs(120),
            heartbeat_grace: Duration::from_secs(30),
            ..SupervisorConfig::default()
        };
        let supervisor = tokio::spawn(run_with_store_and_broker(
            config,
            shutdown.clone(),
            store.clone(),
            test_broker_context(&harness),
        ));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        let finished = loop {
            if let Some((_, current)) = store.locate(&job.id).expect("locate") {
                if matches!(
                    current.status,
                    JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled
                ) {
                    break current;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "failpoint {point} did not resolve"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        shutdown.store(true, Ordering::SeqCst);
        let _ = tokio::time::timeout(Duration::from_secs(10), supervisor).await;

        if replay_safe {
            assert_eq!(
                finished.recovery_count, 1,
                "{point} did not perform exactly one safe retry: {:?}",
                finished.error
            );
            assert_ne!(
                finished.execution_phase,
                ExecutionPhase::Indeterminate,
                "{point} was incorrectly treated as an ambiguous execution"
            );
            assert_eq!(
                std::fs::read_to_string(&marker)
                    .expect("read commit marker")
                    .lines()
                    .count(),
                1,
                "{point} allowed execution before commit or retried more than once"
            );
        } else {
            assert_eq!(finished.recovery_count, 0, "{point} was replayed");
            assert_eq!(
                finished.execution_phase,
                ExecutionPhase::Indeterminate,
                "{point} was not terminal indeterminate"
            );
            assert!(
                finished.error.as_deref().is_some_and(
                    |error| error.contains("indeterminate") || error.contains("ambiguous")
                ),
                "{point}: {:?}",
                finished.error
            );
            assert!(
                store.claim_one().expect("requeue probe").is_none(),
                "{point} re-entered the pending queue"
            );
            let commits = std::fs::read_to_string(&marker)
                .expect("read commit marker")
                .lines()
                .count();
            if matches!(point, "after_commit_record" | "during_commit") {
                assert_eq!(commits, 0, "{point} delivered a commit unexpectedly");
            } else {
                assert!(
                    commits <= 1,
                    "{point} delivered or replayed multiple commits"
                );
            }
            if point == "after_worker_commit" {
                assert_eq!(commits, 1, "worker did not observe exactly one commit");
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_crash_after_control_side_effect_is_terminal_without_requeue() {
    use cos::agent::service::{ExecutionPhase, JobStatus, Store};
    use cos::agentd::supervisor::{run_with_store_and_broker, SupervisorConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let owner_home = tempfile::tempdir().expect("owner home");
    let raw_home =
        std::ffi::CString::new(owner_home.path().as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::chown(
                raw_home.as_ptr(),
                harness.identity.uid,
                harness.identity.gid,
            )
        },
        0,
        "chown owner home"
    );
    let marker_dir = harness.runtime_dir.join("host-side-effect");
    std::fs::create_dir(&marker_dir).expect("create side-effect directory");
    let raw_marker = std::ffi::CString::new(marker_dir.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::chown(raw_marker.as_ptr(), 0, harness.isolation.execution_gid(),) },
        0,
        "chown side-effect directory"
    );
    std::fs::set_permissions(&marker_dir, std::fs::Permissions::from_mode(0o730))
        .expect("chmod side-effect directory");
    let marker = marker_dir.join("host-side-effect.sock");
    let side_effect =
        std::os::unix::net::UnixDatagram::bind(&marker).expect("bind side-effect socket");
    let raw_socket = std::ffi::CString::new(marker.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::chown(raw_socket.as_ptr(), 0, harness.isolation.execution_gid(),) },
        0,
        "chown side-effect socket"
    );
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o620))
        .expect("chmod side-effect socket");
    let malicious = install_crashing_control_host(&harness, &marker);
    std::env::set_var(cos::extension_host::spawn::HOST_BINARY_ENV, &malicious);
    let queue = tempfile::tempdir().expect("queue");
    let store = Store::with_root(queue.path().to_path_buf()).expect("store");
    let job = store
        .submit(
            "host crash side-effect probe".to_string(),
            None,
            Some(1),
            Some(harness.identity.uid),
            Some(owner_home.path().to_string_lossy().into_owned()),
        )
        .expect("submit");
    let shutdown = Arc::new(AtomicBool::new(false));
    let config = SupervisorConfig {
        poll: Duration::from_millis(25),
        max_workers: 1,
        lease: Duration::from_secs(120),
        heartbeat_grace: Duration::from_secs(30),
        ..SupervisorConfig::default()
    };
    let broker = test_broker_context(&harness);
    let supervisor = tokio::spawn(run_with_store_and_broker(
        config,
        shutdown.clone(),
        store.clone(),
        broker,
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let finished = loop {
        if let Some((_, current)) = store.locate(&job.id).expect("locate") {
            if matches!(current.status, JobStatus::Error | JobStatus::Cancelled) {
                break current;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "host crash did not resolve the task"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    shutdown.store(true, Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_secs(10), supervisor).await;

    assert_eq!(finished.status, JobStatus::Error);
    assert_eq!(finished.execution_phase, ExecutionPhase::Indeterminate);
    assert_eq!(finished.recovery_count, 0);
    let error = finished.error.unwrap_or_default();
    assert!(
        error.contains("extension host exited early")
            || error.contains("worker closed its channel"),
        "unexpected terminal error: {error}"
    );
    assert!(
        store.claim_one().expect("requeue probe").is_none(),
        "post-assignment host crash re-entered the queue"
    );
    side_effect
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set side-effect timeout");
    let mut effect = [0u8; 32];
    let received = side_effect.recv(&mut effect).expect("side-effect datagram");
    assert_eq!(&effect[..received], b"executed\n");
    assert!(
        matches!(
            side_effect.recv(&mut effect),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
        ),
        "the hosted side effect ran more than once"
    );
    assert!(
        extension_cgroups(&harness.cgroup_root).is_empty(),
        "terminal host crash left containment behind"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_cgroup_fails_before_worker_or_extension_execution() {
    use cos::agent::service::{JobStatus, Store};
    use cos::agentd::supervisor::{run_with_store_and_broker, SupervisorConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let Some(harness) = harness() else {
        eprintln!("skipping: no usable root isolation harness");
        return;
    };
    let invalid_root = harness.runtime_dir.join("not-a-cgroup");
    std::fs::create_dir(&invalid_root).expect("create invalid cgroup root");
    std::env::set_var(cos::extension_host::spawn::CGROUP_ROOT_ENV, &invalid_root);
    let queue = tempfile::tempdir().expect("queue");
    let store = Store::with_root(queue.path().to_path_buf()).expect("store");
    let job = store
        .submit(
            "containment fail-closed probe".to_string(),
            None,
            Some(1),
            Some(harness.identity.uid),
            Some(harness.identity.home.to_string_lossy().into_owned()),
        )
        .expect("submit");
    let shutdown = Arc::new(AtomicBool::new(false));
    let config = SupervisorConfig {
        poll: Duration::from_millis(25),
        max_workers: 1,
        ..SupervisorConfig::default()
    };
    let broker = test_broker_context(&harness);
    let supervisor = tokio::spawn(run_with_store_and_broker(
        config,
        shutdown.clone(),
        store.clone(),
        broker,
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let finished = loop {
        if let Some((_, current)) = store.locate(&job.id).expect("locate") {
            if current.status == JobStatus::Error {
                break current;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "unavailable containment did not fail the task"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    shutdown.store(true, Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_secs(10), supervisor).await;

    assert_eq!(finished.worker_pid, Some(std::process::id()));
    assert!(
        finished
            .error
            .unwrap_or_default()
            .contains("containment boundary unavailable"),
        "unexpected fail-closed error"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_root_owned_task_never_spawns_a_worker_or_initialises_a_provider() {
    use cos::agent::service::{JobStatus, Store};
    use cos::agentd::supervisor::{run_with_store_and_broker, SupervisorConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let Some(_harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let queue = tempfile::tempdir().expect("tempdir");
    let probe = tempfile::tempdir().expect("tempdir");
    // A stand-in worker image that records the fact it ran. Refusing a
    // root-owned task must happen before anything is executed, so this
    // marker must never appear.
    let marker = probe.path().join("worker-ran");
    let fake_worker = probe.path().join("claw-agentd");
    std::fs::write(
        &fake_worker,
        format!("#!/bin/sh\ntouch {}\n", marker.display()),
    )
    .expect("write probe");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_worker, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    std::env::set_var("COS_AGENTD_BIN", &fake_worker);

    let store = Store::with_root(queue.path().to_path_buf()).expect("store");
    let job = store
        .submit(
            "root-owned probe".to_string(),
            None,
            Some(1),
            Some(0),
            Some("/root".to_string()),
        )
        .expect("submit");

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = SupervisorConfig {
        poll: Duration::from_millis(50),
        max_workers: 1,
        ..SupervisorConfig::default()
    };
    let broker = test_broker_context(&_harness);
    let supervisor = tokio::spawn(run_with_store_and_broker(
        config,
        shutdown.clone(),
        store.clone(),
        broker,
    ));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let finished = loop {
        if let Some((_, current)) = store.locate(&job.id).expect("locate") {
            if matches!(current.status, JobStatus::Error | JobStatus::Cancelled) {
                break current;
            }
            assert_ne!(
                current.status,
                JobStatus::Ok,
                "a root-owned task must never succeed"
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the supervisor never resolved the root-owned task"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    shutdown.store(true, Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_secs(30), supervisor).await;

    std::env::set_var("COS_AGENTD_BIN", WORKER_BIN);

    assert_eq!(finished.status, JobStatus::Error);
    let error = finished.error.unwrap_or_default();
    assert!(
        error.contains("non-root"),
        "the refusal must say what to do instead: {error}"
    );
    assert!(
        !marker.exists(),
        "a worker process was started for a root-owned task"
    );
    // Never rebound to a worker, because none was ever forked.
    assert_eq!(finished.worker_pid, Some(std::process::id()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_without_the_approval_route_refuses_to_start() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut worker = spawn::spawn_worker(&harness.identity, &harness.isolation, "task-noapproval")
        .expect("spawn");
    // Every route except permission mediation. Without it the worker
    // could run but would be unable to reach consent for any denied
    // capability, so it must refuse rather than start half-blind.
    let routes: Vec<String> = protocol::worker_routes()
        .into_iter()
        .filter(|route| route != protocol::ROUTE_APPROVAL)
        .collect();
    let issued_at_ms = cos::agentd::grant::now_ms();
    let grant = signer.issue(GrantClaims {
        v: GRANT_VERSION,
        audience: GRANT_AUDIENCE.to_string(),
        broker_pid: std::process::id(),
        task_id: "task-noapproval".to_string(),
        session_id: None,
        owner_uid: harness.identity.uid,
        client: cos::session::SessionClient::default(),
        presence: None,
        capability_generation: cos::agent::tools::exposure::capability_generation(
            &cos::caps::CapSet::new(),
        ),
        prepare_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        commit_nonce: "fedcba9876543210fedcba9876543210".to_string(),
        extension: None,
        owner_gid: harness.isolation.execution_gid(),
        worker_pid: worker.pid,
        worker_start_time_ticks: worker.start_time_ticks,
        issued_at_ms,
        expires_at_ms: issued_at_ms + 120_000,
        routes,
    });
    send(
        &mut worker,
        &BrokerFrame::Prepare(Box::new(assignment(
            grant,
            protocol::PROTOCOL_VERSION,
            "task-noapproval",
            harness.identity.uid,
            &harness.identity.home,
        ))),
    )
    .await;

    let status = tokio::time::timeout(Duration::from_secs(30), worker.child.wait())
        .await
        .expect("worker did not exit")
        .expect("wait");
    assert!(
        !status.success(),
        "a grant without permission mediation must fail closed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_same_uid_sibling_without_the_channel_cannot_become_a_worker() {
    // The channel is a private descriptor, not a rendezvous point: a
    // process of the same account that simply runs the binary has no
    // job, no grant and no way to obtain either.
    let mut sibling = tokio::process::Command::new(WORKER_BIN)
        .arg("--worker")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sibling");
    let status = tokio::time::timeout(Duration::from_secs(30), sibling.wait())
        .await
        .expect("sibling did not exit")
        .expect("wait");
    assert!(
        !status.success(),
        "a worker started outside clawd must refuse to run"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_refuses_a_grant_minted_for_another_process() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut worker =
        spawn::spawn_worker(&harness.identity, &harness.isolation, "task-stolen").expect("spawn");
    // Bound to a different pid: the worker must not run the job.
    let grant = grant_for(
        &signer,
        "task-stolen",
        &harness.identity,
        harness.isolation.execution_gid(),
        worker.pid.wrapping_add(1),
        worker.start_time_ticks,
    );
    send(
        &mut worker,
        &BrokerFrame::Prepare(Box::new(assignment(
            grant,
            protocol::PROTOCOL_VERSION,
            "task-stolen",
            harness.identity.uid,
            &harness.identity.home,
        ))),
    )
    .await;

    let status = tokio::time::timeout(Duration::from_secs(30), worker.child.wait())
        .await
        .expect("worker did not exit")
        .expect("wait");
    assert!(
        !status.success(),
        "a worker must fail closed on a grant minted for another process"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_protocol_version_mismatch_fails_closed() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut worker =
        spawn::spawn_worker(&harness.identity, &harness.isolation, "task-mixed").expect("spawn");
    let grant = grant_for(
        &signer,
        "task-mixed",
        &harness.identity,
        harness.isolation.execution_gid(),
        worker.pid,
        worker.start_time_ticks,
    );
    send(
        &mut worker,
        &BrokerFrame::Prepare(Box::new(assignment(
            grant,
            protocol::PROTOCOL_VERSION + 1,
            "task-mixed",
            harness.identity.uid,
            &harness.identity.home,
        ))),
    )
    .await;

    let status = tokio::time::timeout(Duration::from_secs(30), worker.child.wait())
        .await
        .expect("worker did not exit")
        .expect("wait");
    assert!(
        !status.success(),
        "a mixed old/new install must fail closed rather than run the task"
    );
}
