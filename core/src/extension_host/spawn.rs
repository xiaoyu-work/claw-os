//! Privilege drop and lifetime control for `claw-extension-host`.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::agentd::spawn::WorkerIdentity;

use super::protocol::{self, ExtensionBinding};

pub const HOST_BINARY_ENV: &str = "COS_EXTENSION_HOST_BIN";
pub const TASK_ENV: &str = "COS_EXTENSION_TASK_ID";
pub const TASK_SESSION_ENV: &str = "COS_EXTENSION_TASK_SESSION";
pub const HOST_SESSION_ENV: &str = "COS_EXTENSION_HOST_SESSION";
pub const WORKER_PID_ENV: &str = "COS_EXTENSION_WORKER_PID";
pub const WORKER_START_ENV: &str = "COS_EXTENSION_WORKER_START";
pub const LEASE_NONCE_ENV: &str = "COS_EXTENSION_LEASE_NONCE";
pub const LEASE_EXPIRES_ENV: &str = "COS_EXTENSION_LEASE_EXPIRES_MS";
pub const CONTROL_SOCKET_ENV: &str = "COS_EXTENSION_CONTROL_SOCKET";
pub const ENFORCE_GROUPS_ENV: &str = "COS_EXTENSION_ENFORCE_GROUPS";

const HOST_PATH: &str = "/usr/local/bin/claw-extension-host";
const SAFE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const HOST_NOFILE_LIMIT: libc::rlim_t = 128;
const HOST_NPROC_LIMIT: libc::rlim_t = 512;
const HOST_ADDRESS_SPACE_LIMIT: libc::rlim_t = 2 * 1024 * 1024 * 1024;
const HOST_FILE_SIZE_LIMIT: libc::rlim_t = 256 * 1024 * 1024;

const INHERITED_ENV_KEYS: &[&str] = &[
    "COS_APPS_DIR",
    "COS_BIN",
    "COS_CACHE_DIR",
    "COS_CONFIG_DIR",
    "COS_LOG_DIR",
    "COS_SDK_PYTHON_DIR",
    "LANG",
    "LC_ALL",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "TZ",
];

#[derive(Debug, Clone)]
pub struct HostPaths {
    pub dir: PathBuf,
    pub control_socket: PathBuf,
    pub broker_socket: PathBuf,
}

impl HostPaths {
    pub fn create(identity: &WorkerIdentity) -> Result<Self, String> {
        let base = crate::paths::runtime_dir().join("extension-hosts");
        let owner_root = base.join(identity.uid.to_string());
        let dir = owner_root.join(uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&base)
            .map_err(|error| format!("create extension-host runtime root: {error}"))?;
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o711))
            .map_err(|error| format!("protect extension-host runtime root: {error}"))?;
        std::fs::create_dir_all(&owner_root)
            .map_err(|error| format!("create owner extension-host runtime: {error}"))?;
        std::fs::set_permissions(&owner_root, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protect owner extension-host runtime: {error}"))?;
        std::fs::create_dir(&dir)
            .map_err(|error| format!("create extension-host runtime directory: {error}"))?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protect extension-host runtime directory: {error}"))?;
        if unsafe { libc::geteuid() } == 0 {
            for path in [&owner_root, &dir] {
                let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
                    .map_err(|_| "extension-host runtime path contains NUL".to_string())?;
                if unsafe { libc::chown(path.as_ptr(), identity.uid, identity.gid) } != 0 {
                    return Err(format!(
                        "chown extension-host runtime directory: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
        }
        Ok(Self {
            control_socket: dir.join("control.sock"),
            broker_socket: dir.join("broker.sock"),
            dir,
        })
    }

    pub fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.control_socket);
        let _ = std::fs::remove_file(&self.broker_socket);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

#[derive(Debug)]
pub struct SpawnedExtensionHost {
    pub child: tokio::process::Child,
    pub pid: u32,
    pub start_time_ticks: Option<u64>,
    pub binding: ExtensionBinding,
    pub paths: HostPaths,
    pub cgroup: Option<ResourceGroup>,
}

pub fn host_binary_path() -> PathBuf {
    if let Some(path) = std::env::var_os(HOST_BINARY_ENV) {
        return PathBuf::from(path);
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let sibling = dir.join("claw-extension-host");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from(HOST_PATH)
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_host(
    identity: &WorkerIdentity,
    task_id: &str,
    task_session_id: Option<&str>,
    host_session_id: Option<&str>,
    worker_pid: u32,
    worker_start_time_ticks: Option<u64>,
    lease_nonce: &str,
    expires_at_ms: u64,
    paths: HostPaths,
) -> Result<SpawnedExtensionHost, String> {
    let binary = host_binary_path();
    if !binary.exists() {
        paths.cleanup();
        return Err(format!(
            "extension host binary is not installed at {}",
            binary.display()
        ));
    }
    if crate::agentd::spawn::broker_is_root() {
        crate::agentd::spawn::validate_root_owned_executable(&binary)?;
    }
    let enforce_groups = crate::agentd::spawn::broker_is_root();

    let mut command = std::process::Command::new(&binary);
    command.arg("--host");
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.current_dir(&identity.home);
    command.env_clear();
    command.env("HOME", &identity.home);
    command.env("USER", &identity.username);
    command.env("LOGNAME", &identity.username);
    command.env("PATH", SAFE_PATH);
    command.env("SHELL", "/bin/sh");
    command.env("COS_PERMS_MODE", "strict");
    command.env(
        "COS_PROC_DATA_DIR",
        std::env::var_os("COS_PROC_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/run/cos/caps").join(identity.uid.to_string())),
    );
    command.env(
        "COS_DATA_DIR",
        std::env::var_os("COS_USER_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| identity.home.join(".local").join("share").join("cos")),
    );
    command.env(TASK_ENV, task_id);
    command.env(WORKER_PID_ENV, worker_pid.to_string());
    command.env(
        WORKER_START_ENV,
        worker_start_time_ticks
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
    command.env(LEASE_NONCE_ENV, lease_nonce);
    command.env(LEASE_EXPIRES_ENV, expires_at_ms.to_string());
    command.env(CONTROL_SOCKET_ENV, &paths.control_socket);
    command.env(ENFORCE_GROUPS_ENV, if enforce_groups { "1" } else { "0" });
    command.env(protocol::BROKER_SOCKET_ENV, &paths.broker_socket);
    if let Some(task_session_id) = task_session_id {
        command.env(TASK_SESSION_ENV, task_session_id);
    }
    if let Some(host_session_id) = host_session_id {
        command.env(HOST_SESSION_ENV, host_session_id);
        command.env("COS_SESSION", host_session_id);
    }
    for key in INHERITED_ENV_KEYS {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }

    let expected_parent = unsafe { libc::getpid() };
    let uid = identity.uid;
    let gid = identity.gid;
    let try_namespaces = std::env::var("CLAWD_EXTENSION_HOST_NAMESPACES")
        .map(|value| !matches!(value.trim(), "0" | "off" | "false" | "no"))
        .unwrap_or(true);
    unsafe {
        command.pre_exec(move || {
            libc::umask(0o077);
            crate::agentd::spawn::mark_inherited_descriptors_cloexec(3);
            if try_namespaces {
                // IPC and UTS isolation do not change filesystem or network
                // reachability. They are opportunistic because some kernels
                // or containers deny namespace creation.
                let _ = libc::unshare(libc::CLONE_NEWIPC | libc::CLONE_NEWUTS);
            }
            crate::agentd::spawn::drop_to_owner(uid, gid)?;
            crate::agentd::spawn::verify_dropped_identity(uid, gid, enforce_groups)?;
            apply_resource_limits()?;
            crate::agentd::spawn::harden_child(expected_parent)?;
            if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", binary.display()))?;
    let pid = child
        .id()
        .ok_or_else(|| "extension host exited before it could be identified".to_string())?;
    let start_time_ticks = crate::proc::read_start_time_ticks_pub(pid);
    let cgroup = ResourceGroup::attach(task_id, pid);
    let binding = ExtensionBinding {
        protocol: protocol::PROTOCOL_VERSION,
        task_id: task_id.to_string(),
        session_id: task_session_id.map(ToOwned::to_owned),
        owner_uid: identity.uid,
        owner_gid: identity.gid,
        worker_pid,
        worker_start_time_ticks,
        host_pid: pid,
        host_start_time_ticks: start_time_ticks,
        lease_nonce: lease_nonce.to_string(),
        expires_at_ms,
        control_socket: paths.control_socket.to_string_lossy().into_owned(),
        broker_socket: paths.broker_socket.to_string_lossy().into_owned(),
    };
    if let Err(error) = binding.validate_shape() {
        let _ = child.start_kill();
        paths.cleanup();
        return Err(error);
    }
    Ok(SpawnedExtensionHost {
        child,
        pid,
        start_time_ticks,
        binding,
        paths,
        cgroup,
    })
}

fn apply_resource_limits() -> std::io::Result<()> {
    set_limit(libc::RLIMIT_CORE, 0)?;
    set_limit(libc::RLIMIT_NOFILE, HOST_NOFILE_LIMIT)?;
    set_limit(libc::RLIMIT_NPROC, HOST_NPROC_LIMIT)?;
    set_limit(libc::RLIMIT_AS, HOST_ADDRESS_SPACE_LIMIT)?;
    set_limit(libc::RLIMIT_FSIZE, HOST_FILE_SIZE_LIMIT)
}

#[cfg(target_env = "gnu")]
type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(target_env = "gnu"))]
type RlimitResource = libc::c_int;

fn set_limit(resource: RlimitResource, value: libc::rlim_t) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    if unsafe { libc::setrlimit(resource, &limit) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[derive(Debug)]
pub struct ResourceGroup {
    path: PathBuf,
}

impl ResourceGroup {
    fn attach(task_id: &str, pid: u32) -> Option<Self> {
        if unsafe { libc::geteuid() } != 0 {
            return None;
        }
        let root = Self::current_cgroup_path()?;
        if !root.join("cgroup.controllers").is_file() {
            return None;
        }
        let name = format!(
            "cos-extension-{}-{pid}",
            crate::crypto::sha256_hex(task_id.as_bytes())
                .chars()
                .take(16)
                .collect::<String>()
        );
        let path = root.join(name);
        if std::fs::create_dir(&path).is_err() {
            return None;
        }
        for (name, value) in [
            ("pids.max", "128"),
            ("memory.max", "1073741824"),
            ("cpu.max", "100000 100000"),
        ] {
            if path.join(name).is_file() {
                let _ = std::fs::write(path.join(name), value);
            }
        }
        if std::fs::write(path.join("cgroup.procs"), pid.to_string()).is_err() {
            let _ = std::fs::remove_dir(&path);
            return None;
        }
        Some(Self { path })
    }

    fn current_cgroup_path() -> Option<PathBuf> {
        let raw = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        let relative = raw.lines().find_map(|line| line.strip_prefix("0::"))?;
        Some(Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')))
    }

    pub fn kill_all(&self) {
        let _ = std::fs::write(self.path.join("cgroup.kill"), b"1");
    }
}

impl Drop for ResourceGroup {
    fn drop(&mut self) {
        self.kill_all();
        let _ = std::fs::remove_dir(&self.path);
    }
}

/// Kill a host and every descendant, including children that called
/// `setsid(2)` to leave the host's process group.
///
/// # Safety
///
/// `pid` must still identify an unreaped child owned by the caller. Keeping
/// the child unreaped prevents pid reuse while the process tree is inspected
/// and signalled.
pub unsafe fn terminate_host_tree(pid: u32, cgroup: Option<&ResourceGroup>) {
    if let Some(cgroup) = cgroup {
        cgroup.kill_all();
    }
    for _ in 0..4 {
        let mut descendants = descendants_of(pid);
        descendants.sort_unstable_by(|a, b| b.cmp(a));
        for child in descendants {
            libc::kill(child as libc::pid_t, libc::SIGKILL);
        }
        let pgid = libc::getpgid(pid as libc::pid_t);
        if pgid == pid as libc::pid_t {
            libc::kill(-pgid, libc::SIGKILL);
        }
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn descendants_of(root: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut parents = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        if let Some(parent) = status.lines().find_map(|line| {
            line.strip_prefix("PPid:")
                .and_then(|value| value.trim().parse::<u32>().ok())
        }) {
            parents.push((pid, parent));
        }
    }
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, ppid) in &parents {
            if *ppid == parent && !found.contains(pid) {
                found.push(*pid);
                frontier.push(*pid);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/spawn.rs"
    ));
}
