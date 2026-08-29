//! Privilege drop and lifetime control for `claw-extension-host`.

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::agentd::spawn::{ExecutionIsolation, WorkerIdentity};

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
pub const EXECUTION_GID_ENV: &str = "COS_EXTENSION_EXECUTION_GID";
pub const CGROUP_ROOT_ENV: &str = "CLAWD_EXTENSION_CGROUP_ROOT";

const HOST_PATH: &str = "/usr/local/bin/claw-extension-host";
const SAFE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const HOST_NOFILE_LIMIT: libc::rlim_t = 128;
const HOST_NPROC_LIMIT: libc::rlim_t = 512;
const HOST_ADDRESS_SPACE_LIMIT: libc::rlim_t = 2 * 1024 * 1024 * 1024;
const HOST_FILE_SIZE_LIMIT: libc::rlim_t = 256 * 1024 * 1024;
const CGROUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const REQUIRED_CONTROLLERS: [&str; 3] = ["cpu", "memory", "pids"];
const CGROUP_LIMITS: [(&str, &str); 4] = [
    ("pids.max", "128"),
    ("memory.max", "1073741824"),
    ("memory.oom.group", "1"),
    ("cpu.max", "100000 100000"),
];
const BROKER_CGROUP: &str = "cos-broker";

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
    pub fn create(identity: &WorkerIdentity, execution_gid: u32) -> Result<Self, String> {
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
                if unsafe { libc::chown(path.as_ptr(), identity.uid, execution_gid) } != 0 {
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
    pub cgroup: ResourceGroup,
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
    isolation: &ExecutionIsolation,
    containment: &ContainmentRoot,
    task_id: &str,
    task_session_id: Option<&str>,
    host_session_id: Option<&str>,
    worker_pid: u32,
    worker_start_time_ticks: Option<u64>,
    lease_nonce: &str,
    expires_at_ms: u64,
    paths: HostPaths,
) -> Result<SpawnedExtensionHost, String> {
    let mut cgroup = match ResourceGroup::create(containment, task_id) {
        Ok(cgroup) => cgroup,
        Err(error) => {
            paths.cleanup();
            return Err(error);
        }
    };
    let binary = host_binary_path();
    if !binary.exists() {
        let cleanup = cgroup.cleanup_blocking();
        paths.cleanup();
        let mut error = format!(
            "extension host binary is not installed at {}",
            binary.display()
        );
        if let Err(cleanup) = cleanup {
            error.push_str(&format!("; containment cleanup failed: {cleanup}"));
        }
        return Err(error);
    }
    if crate::agentd::spawn::broker_is_root() {
        if let Err(error) = crate::agentd::spawn::validate_root_owned_executable(&binary) {
            let cleanup = cgroup.cleanup_blocking();
            paths.cleanup();
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => format!("{error}; containment cleanup failed: {cleanup}"),
            });
        }
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
    command.env(EXECUTION_GID_ENV, isolation.execution_gid().to_string());
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
    let gid = isolation.execution_gid();
    let child_isolation = isolation.clone();
    let cgroup_procs_fd = cgroup.procs_fd();
    let try_namespaces = std::env::var("CLAWD_EXTENSION_HOST_NAMESPACES")
        .map(|value| !matches!(value.trim(), "0" | "off" | "false" | "no"))
        .unwrap_or(true);
    unsafe {
        command.pre_exec(move || {
            attach_current_process(cgroup_procs_fd)?;
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
            child_isolation.verify_after_drop(uid)?;
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
        .map_err(|error| {
            let cleanup = cgroup.cleanup_blocking();
            paths.cleanup();
            match cleanup {
                Ok(()) => format!("spawn {}: {error}", binary.display()),
                Err(cleanup) => format!(
                    "spawn {}: {error}; containment cleanup failed: {cleanup}",
                    binary.display()
                ),
            }
        })?;
    let Some(pid) = child.id() else {
        let _ = child.start_kill();
        let cleanup = cgroup.cleanup_blocking();
        paths.cleanup();
        return Err(match cleanup {
            Ok(()) => "extension host exited before it could be identified".to_string(),
            Err(cleanup) => format!(
                "extension host exited before it could be identified; containment cleanup failed: {cleanup}"
            ),
        });
    };
    if let Err(error) = cgroup.verify_member(pid) {
        let _ = child.start_kill();
        let cleanup = cgroup.cleanup_blocking();
        paths.cleanup();
        return Err(match cleanup {
            Ok(()) => format!("extension host containment verification failed: {error}"),
            Err(cleanup) => format!(
                "extension host containment verification failed: {error}; containment cleanup failed: {cleanup}"
            ),
        });
    }
    let start_time_ticks = crate::proc::read_start_time_ticks_pub(pid);
    let binding = ExtensionBinding {
        protocol: protocol::PROTOCOL_VERSION,
        task_id: task_id.to_string(),
        session_id: task_session_id.map(ToOwned::to_owned),
        owner_uid: identity.uid,
        owner_gid: gid,
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
        let cleanup = cgroup.cleanup_blocking();
        paths.cleanup();
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; containment cleanup failed: {cleanup}"),
        });
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
pub struct ContainmentRoot {
    path: PathBuf,
}

impl ContainmentRoot {
    /// Establish a delegated cgroup-v2 subtree before any extension host can
    /// be spawned.
    ///
    /// A configured root must already be an empty, root-owned delegated
    /// cgroup. Otherwise the daemon moves itself into a broker leaf under its
    /// systemd unit cgroup, enables the required controllers on the now-empty
    /// parent, and reserves sibling children for extension tasks.
    pub fn establish() -> Result<Self, String> {
        #[cfg(not(target_os = "linux"))]
        {
            return Err("extension containment requires Linux cgroup v2".to_string());
        }
        #[cfg(target_os = "linux")]
        {
            if unsafe { libc::geteuid() } != 0 {
                return Err("extension containment requires a root broker".to_string());
            }
            let configured = std::env::var_os(CGROUP_ROOT_ENV).map(PathBuf::from);
            let (root, broker_leaf) = match configured {
                Some(path) => {
                    let root = validate_cgroup_root(&path, false)?;
                    require_no_processes(&root)?;
                    (root, false)
                }
                None => (prepare_current_cgroup_root()?, true),
            };
            enable_required_controllers(&root)?;
            cleanup_stale_groups(&root, broker_leaf)?;
            Ok(Self { path: root })
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug)]
pub struct ResourceGroup {
    path: PathBuf,
    procs: OwnedFd,
    active: bool,
}

impl ResourceGroup {
    fn create(root: &ContainmentRoot, task_id: &str) -> Result<Self, String> {
        let digest = crate::crypto::sha256_hex(task_id.as_bytes());
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let name = format!("cos-extension-{}-{}", &digest[..16], &nonce[..12]);
        let path = root.path.join(name);
        std::fs::create_dir(&path)
            .map_err(|error| format!("create extension containment group: {error}"))?;

        let created = (|| {
            configure_limits(&path)?;
            // Prove the kernel implements cgroup.kill before admitting a
            // process. Writing to an empty cgroup is harmless and fails on
            // kernels/filesystems that only expose a placeholder.
            write_control(&path.join("cgroup.kill"), b"1")
                .map_err(|error| format!("verify extension cgroup.kill: {error}"))?;
            if !group_is_empty(&path)? {
                return Err("new extension containment group is unexpectedly populated".to_string());
            }
            let procs = open_write(&path.join("cgroup.procs"))
                .map_err(|error| format!("open extension cgroup.procs: {error}"))?;
            Ok(Self {
                path: path.clone(),
                procs,
                active: true,
            })
        })();
        match created {
            Ok(group) => Ok(group),
            Err(error) => {
                let cleanup = remove_rejected_cgroup(&path);
                Err(match cleanup {
                    Ok(()) => error,
                    Err(cleanup) => {
                        format!("{error}; failed to remove rejected containment group: {cleanup}")
                    }
                })
            }
        }
    }

    fn procs_fd(&self) -> RawFd {
        self.procs.as_raw_fd()
    }

    fn verify_member(&self, pid: u32) -> Result<(), String> {
        let members = read_pids(&self.path.join("cgroup.procs"))?;
        if !members.contains(&pid) {
            return Err(format!(
                "host pid {pid} is not a member of {}",
                self.path.display()
            ));
        }
        let process_group = process_cgroup_path(pid)?;
        let expected = self
            .path
            .strip_prefix("/sys/fs/cgroup")
            .map_err(|_| "extension cgroup is outside the unified hierarchy".to_string())?;
        let expected = Path::new("/").join(expected);
        if process_group != expected {
            return Err(format!(
                "host pid {pid} reports cgroup {} instead of {}",
                process_group.display(),
                expected.display()
            ));
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kill_all(&self) -> Result<(), String> {
        write_control(&self.path.join("cgroup.kill"), b"1")
            .map_err(|error| format!("kill extension cgroup {}: {error}", self.path.display()))
    }

    pub fn is_empty(&self) -> Result<bool, String> {
        group_is_empty(&self.path)
    }

    pub async fn cleanup(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        self.kill_all()?;
        let deadline = tokio::time::Instant::now() + CGROUP_CLEANUP_TIMEOUT;
        loop {
            if self.is_empty()? {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "extension cgroup {} remained populated after cgroup.kill",
                    self.path.display()
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        std::fs::remove_dir(&self.path).map_err(|error| {
            format!(
                "remove empty extension cgroup {}: {error}",
                self.path.display()
            )
        })?;
        self.active = false;
        Ok(())
    }

    fn cleanup_blocking(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        cleanup_cgroup_blocking(&self.path, CGROUP_CLEANUP_TIMEOUT)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for ResourceGroup {
    fn drop(&mut self) {
        if self.active {
            if let Err(error) = self.cleanup_blocking() {
                tracing::error!(
                    cgroup = %self.path.display(),
                    error = %error,
                    "extension containment dropped while still populated"
                );
            }
        }
    }
}

fn attach_current_process(procs_fd: RawFd) -> std::io::Result<()> {
    let mut digits = [0u8; 10];
    let mut value = unsafe { libc::getpid() } as u32;
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let bytes = &digits[start..];
    loop {
        let written =
            unsafe { libc::write(procs_fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len()) };
        if written == bytes.len() as isize {
            return Ok(());
        }
        if written < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return if written < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Err(crate::agentd::spawn::raw_error(libc::EIO))
        };
    }
}

#[cfg(target_os = "linux")]
fn prepare_current_cgroup_root() -> Result<PathBuf, String> {
    let current = current_cgroup_path()?;
    if current.file_name().and_then(|name| name.to_str()) == Some(BROKER_CGROUP) {
        let root = current
            .parent()
            .ok_or_else(|| "broker cgroup has no delegated parent".to_string())?
            .to_path_buf();
        validate_cgroup_root(&root, true)?;
        let members = read_pids(&current.join("cgroup.procs"))?;
        if members != [std::process::id()] {
            return Err("broker cgroup contains an unexpected process".to_string());
        }
        return Ok(root);
    }

    let root = validate_cgroup_root(&current, true)?;
    let members = read_pids(&root.join("cgroup.procs"))?;
    if members != [std::process::id()] {
        return Err(format!(
            "delegated cgroup {} must contain only clawd before containment setup",
            root.display()
        ));
    }
    let broker = root.join(BROKER_CGROUP);
    if broker.exists() {
        cleanup_cgroup_blocking(&broker, CGROUP_CLEANUP_TIMEOUT)
            .map_err(|error| format!("clean stale broker cgroup: {error}"))?;
    }
    std::fs::create_dir(&broker).map_err(|error| format!("create broker cgroup leaf: {error}"))?;
    if let Err(error) = std::fs::write(broker.join("cgroup.procs"), std::process::id().to_string())
    {
        let _ = std::fs::remove_dir(&broker);
        return Err(format!("move clawd into broker cgroup leaf: {error}"));
    }
    require_no_processes(&root)?;
    let broker_members = read_pids(&broker.join("cgroup.procs"))?;
    if broker_members != [std::process::id()] {
        return Err("clawd did not enter its dedicated broker cgroup".to_string());
    }
    Ok(root)
}

#[cfg(target_os = "linux")]
fn current_cgroup_path() -> Result<PathBuf, String> {
    let raw = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|error| format!("read broker cgroup membership: {error}"))?;
    let relative = raw
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| "unified cgroup-v2 membership is unavailable".to_string())?;
    if relative.split('/').any(|part| part == "..") {
        return Err("broker cgroup membership contains a parent traversal".to_string());
    }
    Ok(Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')))
}

#[cfg(target_os = "linux")]
fn validate_cgroup_root(path: &Path, allow_mount_root: bool) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;

    let mount = std::fs::canonicalize("/sys/fs/cgroup")
        .map_err(|error| format!("canonicalize cgroup-v2 mount: {error}"))?;
    let root = std::fs::canonicalize(path)
        .map_err(|error| format!("canonicalize extension cgroup root: {error}"))?;
    if (!allow_mount_root && root == mount) || !root.starts_with(&mount) {
        return Err(format!(
            "extension cgroup root must be a delegated child of {}",
            mount.display()
        ));
    }
    let metadata = std::fs::symlink_metadata(&root)
        .map_err(|error| format!("inspect extension cgroup root: {error}"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
    {
        return Err(format!(
            "extension cgroup root has unsafe ownership or mode: {}",
            root.display()
        ));
    }
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    let raw = std::ffi::CString::new(root.as_os_str().as_encoded_bytes())
        .map_err(|_| "extension cgroup root contains NUL".to_string())?;
    if unsafe { libc::statfs(raw.as_ptr(), &mut stat) } != 0 {
        return Err(format!(
            "inspect extension cgroup filesystem: {}",
            std::io::Error::last_os_error()
        ));
    }
    const CGROUP2_SUPER_MAGIC: libc::c_long = 0x6367_7270;
    if stat.f_type as libc::c_long != CGROUP2_SUPER_MAGIC {
        return Err("extension containment root is not on cgroup v2".to_string());
    }
    for file in [
        "cgroup.controllers",
        "cgroup.subtree_control",
        "cgroup.procs",
    ] {
        if !root.join(file).is_file() {
            return Err(format!(
                "extension cgroup root is missing {file}: {}",
                root.display()
            ));
        }
    }
    Ok(root)
}

#[cfg(target_os = "linux")]
fn enable_required_controllers(root: &Path) -> Result<(), String> {
    let available = read_words(&root.join("cgroup.controllers"))?;
    for controller in REQUIRED_CONTROLLERS {
        if !available.iter().any(|value| value == controller) {
            return Err(format!(
                "delegated cgroup {} lacks the {controller} controller",
                root.display()
            ));
        }
    }
    write_control(&root.join("cgroup.subtree_control"), b"+cpu +memory +pids")
        .map_err(|error| format!("enable extension cgroup controllers: {error}"))?;
    let enabled = read_words(&root.join("cgroup.subtree_control"))?;
    for controller in REQUIRED_CONTROLLERS {
        if !enabled.iter().any(|value| value == controller) {
            return Err(format!(
                "delegated cgroup {} did not enable the {controller} controller",
                root.display()
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn cleanup_stale_groups(root: &Path, broker_leaf: bool) -> Result<(), String> {
    for entry in std::fs::read_dir(root)
        .map_err(|error| format!("list delegated extension cgroups: {error}"))?
    {
        let entry = entry.map_err(|error| format!("read delegated cgroup entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect delegated cgroup entry: {error}"))?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(format!(
                "delegated cgroup child has a non-UTF-8 name: {}",
                entry.path().display()
            ));
        };
        if broker_leaf && name == BROKER_CGROUP {
            continue;
        }
        if !name.starts_with("cos-extension-") {
            return Err(format!(
                "delegated extension cgroup root contains an unexpected child: {}",
                entry.path().display()
            ));
        }
        cleanup_cgroup_blocking(&entry.path(), CGROUP_CLEANUP_TIMEOUT)
            .map_err(|error| format!("clean stale extension cgroup: {error}"))?;
    }
    Ok(())
}

fn configure_limits(path: &Path) -> Result<(), String> {
    for (name, value) in CGROUP_LIMITS {
        let file = path.join(name);
        if !file.is_file() {
            return Err(format!(
                "extension containment group is missing required limit {name}"
            ));
        }
        write_control(&file, value.as_bytes())
            .map_err(|error| format!("write extension cgroup limit {name}: {error}"))?;
        let actual = std::fs::read_to_string(&file)
            .map_err(|error| format!("read extension cgroup limit {name}: {error}"))?;
        if actual.trim() != value {
            return Err(format!(
                "extension cgroup limit {name} read back as `{}` instead of `{value}`",
                actual.trim()
            ));
        }
    }
    Ok(())
}

fn remove_rejected_cgroup(path: &Path) -> Result<(), String> {
    if !group_is_empty(path)? {
        return Err(format!(
            "rejected containment cgroup {} became populated",
            path.display()
        ));
    }
    std::fs::remove_dir(path).map_err(|error| {
        format!(
            "remove rejected containment cgroup {}: {error}",
            path.display()
        )
    })
}

fn cleanup_cgroup_blocking(path: &Path, timeout: Duration) -> Result<(), String> {
    write_control(&path.join("cgroup.kill"), b"1")
        .map_err(|error| format!("write cgroup.kill for {}: {error}", path.display()))?;
    let deadline = Instant::now() + timeout;
    loop {
        if group_is_empty(path)? {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "cgroup {} remained populated after cgroup.kill",
                path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::remove_dir(path)
        .map_err(|error| format!("remove empty cgroup {}: {error}", path.display()))
}

fn group_is_empty(path: &Path) -> Result<bool, String> {
    let events = std::fs::read_to_string(path.join("cgroup.events"))
        .map_err(|error| format!("read cgroup.events for {}: {error}", path.display()))?;
    let populated = events
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some("populated"))
                .then(|| fields.next())
                .flatten()
        })
        .ok_or_else(|| format!("cgroup.events omitted populated for {}", path.display()))?;
    let members = read_pids(&path.join("cgroup.procs"))?;
    match populated {
        "0" => Ok(members.is_empty()),
        "1" => Ok(false),
        value => Err(format!(
            "cgroup.events reported invalid populated value `{value}`"
        )),
    }
}

fn process_cgroup_path(pid: u32) -> Result<PathBuf, String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .map_err(|error| format!("read host cgroup membership: {error}"))?;
    let relative = raw
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| "host has no unified cgroup-v2 membership".to_string())?;
    Ok(PathBuf::from(relative))
}

fn require_no_processes(path: &Path) -> Result<(), String> {
    let members = read_pids(&path.join("cgroup.procs"))?;
    if members.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "delegated extension cgroup {} contains processes: {members:?}",
            path.display()
        ))
    }
}

fn read_pids(path: &Path) -> Result<Vec<u32>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut pids = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.trim()
                .parse::<u32>()
                .map_err(|error| format!("invalid pid in {}: {error}", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    pids.sort_unstable();
    Ok(pids)
}

fn read_words(path: &Path) -> Result<Vec<String>, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))
        .map(|raw| raw.split_whitespace().map(str::to_string).collect())
}

fn open_write(path: &Path) -> std::io::Result<OwnedFd> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let raw = std::os::fd::IntoRawFd::into_raw_fd(file);
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn write_control(path: &Path, value: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.write_all(value)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/spawn.rs"
    ));
}
