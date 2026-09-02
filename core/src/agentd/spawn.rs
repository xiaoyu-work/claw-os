//! Privilege-dropping spawn for one `claw-agentd` worker.
//!
//! `clawd` is root, so the only thing that separates the agent runtime
//! from the broker's authority is what happens between `fork(2)` and
//! `execve(2)`. That window is handled entirely here, in one
//! `pre_exec` closure, and in this order:
//!
//! 1. `umask(0077)` so anything the worker creates is owner-only.
//! 2. `dup2` the job channel onto [`protocol::CHANNEL_FD`] and close
//!    every other inherited descriptor, so no root-owned socket, log
//!    file, queue lock or credential handle survives the `exec`.
//! 3. `setgroups(0, NULL)` — **before** the uid drop, while the child
//!    still has the privilege to do it. This is what removes `sudo`
//!    and every other supplementary group; without it a worker would
//!    inherit the broker's group membership and could open
//!    `/run/cos/clawd.sock`.
//! 4. `setresgid` to the dedicated extension execution group, then
//!    `setresuid` to the owner's account, real/effective/saved ids
//!    together, so broker-socket group membership cannot survive and
//!    nothing can be restored.
//! 5. Re-read every id and the supplementary group list from the kernel
//!    and abort the `exec` if any of it is wrong. The pinned broker
//!    socket and its ancestors are re-identified, and an actual
//!    connection must fail with `EACCES`/`EPERM`. `Command::uid` alone
//!    proves nothing; these are the checks that do.
//! 6. `PR_SET_PDEATHSIG` plus a `getppid` re-check so a worker cannot
//!    outlive the supervisor that leased it, then
//!    `PR_SET_NO_NEW_PRIVS` so no setuid binary can raise privilege
//!    again inside the worker.
//!
//! The environment is rebuilt from an allowlist rather than filtered,
//! and no credential value is ever placed in it: the worker reads the
//! owner's own credential store as the owner.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::protocol;

/// Environment keys copied from the broker when present. Everything
/// else is dropped: the worker starts from an empty environment.
const INHERITED_ENV_KEYS: &[&str] = &[
    "COS_APPS_DIR",
    "COS_AGENT_EXTENSIONS_DIR",
    "COS_BIN",
    "COS_CACHE_DIR",
    "COS_CONFIG_DIR",
    "COS_DATA_DIR",
    "COS_LOG_DIR",
    "COS_RUNTIME_DIR",
    "LANG",
    "LC_ALL",
    "RUST_LOG",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "TZ",
];

const WORKER_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
pub const ISOLATED_GROUP_ENV: &str = "COS_EXTENSION_EXEC_GROUP";
pub const DEFAULT_ISOLATED_GROUP: &str = "cos-extension";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
}

impl FileIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
        }
    }

    fn from_stat(stat: &libc::stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            uid: stat.st_uid,
            gid: stat.st_gid,
            mode: stat.st_mode,
        }
    }

    fn writable_by(self, uid: u32, gid: u32) -> bool {
        let bits = if self.uid == uid {
            (self.mode >> 6) & 0o7
        } else if self.gid == gid {
            (self.mode >> 3) & 0o7
        } else {
            self.mode & 0o7
        };
        bits & 0o2 != 0
    }
}

#[derive(Debug)]
struct SealedPath {
    path: CString,
    identity: FileIdentity,
}

/// Snapshot of the actual primary broker socket immediately before a task is
/// forked.
///
/// The socket inode is pinned with `O_PATH`, and every canonical ancestor is
/// recorded. The post-drop child verifies all identities before and after an
/// actual denied `connect(2)`. Since none of the ancestors may be owned or
/// writable by the task uid/gid, an untrusted process cannot swap the path
/// after that probe.
#[derive(Debug)]
struct BrokerSocketSeal {
    pinned_socket: OwnedFd,
    socket: SealedPath,
    ancestors: Vec<SealedPath>,
}

#[derive(Debug, Clone)]
pub struct ExecutionIsolation {
    execution_gid: u32,
    broker_socket: std::sync::Arc<BrokerSocketSeal>,
}

impl ExecutionIsolation {
    pub fn capture(socket_path: &Path, owner_uid: u32, execution_gid: u32) -> Result<Self, String> {
        if !broker_is_root() {
            return Err(
                "secure agent isolation requires a root broker to set the dedicated execution gid"
                    .to_string(),
            );
        }
        if owner_uid == 0 || execution_gid == 0 {
            return Err("agent isolation requires non-root task and group identities".to_string());
        }

        let parent = socket_path
            .parent()
            .ok_or_else(|| "primary broker socket path has no parent".to_string())?;
        let name = socket_path
            .file_name()
            .ok_or_else(|| "primary broker socket path has no file name".to_string())?;
        let canonical_parent = std::fs::canonicalize(parent)
            .map_err(|error| format!("canonicalize primary broker socket parent: {error}"))?;
        let canonical_socket = canonical_parent.join(name);
        let socket = sealed_path(&canonical_socket)?;
        let socket_metadata = std::fs::symlink_metadata(&canonical_socket)
            .map_err(|error| format!("inspect primary broker socket: {error}"))?;
        if !socket_metadata.file_type().is_socket() {
            return Err(format!(
                "primary broker path is not a Unix socket: {}",
                canonical_socket.display()
            ));
        }
        if socket.identity.uid == owner_uid || socket.identity.writable_by(owner_uid, execution_gid)
        {
            return Err(format!(
                "task uid {owner_uid} with isolated gid {execution_gid} can modify the primary broker socket"
            ));
        }
        if socket.path.as_bytes().len()
            >= unsafe { std::mem::zeroed::<libc::sockaddr_un>().sun_path.len() }
        {
            return Err("primary broker socket path is too long for AF_UNIX".to_string());
        }

        let raw = unsafe {
            libc::open(
                socket.path.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(format!(
                "pin primary broker socket: {}",
                std::io::Error::last_os_error()
            ));
        }
        let pinned_socket = unsafe { OwnedFd::from_raw_fd(raw) };
        let pinned = fstat_identity(pinned_socket.as_raw_fd())
            .map_err(|error| format!("identify pinned primary broker socket: {error}"))?;
        if pinned != socket.identity {
            return Err("primary broker socket changed while it was being pinned".to_string());
        }

        let mut ancestors = Vec::new();
        let mut current = Some(canonical_parent.as_path());
        while let Some(path) = current {
            let sealed = sealed_path(path)?;
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|error| format!("inspect broker socket ancestor: {error}"))?;
            if !metadata.is_dir() {
                return Err(format!(
                    "primary broker socket ancestor is not a directory: {}",
                    path.display()
                ));
            }
            if sealed.identity.uid == owner_uid
                || sealed.identity.writable_by(owner_uid, execution_gid)
            {
                return Err(format!(
                    "task uid {owner_uid} can replace the primary broker socket through {}",
                    path.display()
                ));
            }
            ancestors.push(sealed);
            current = path.parent();
        }

        Ok(Self {
            execution_gid,
            broker_socket: std::sync::Arc::new(BrokerSocketSeal {
                pinned_socket,
                socket,
                ancestors,
            }),
        })
    }

    pub fn execution_gid(&self) -> u32 {
        self.execution_gid
    }

    pub(crate) fn verify_after_drop(&self, uid: u32) -> std::io::Result<()> {
        self.broker_socket
            .verify_after_drop(uid, self.execution_gid)
    }
}

impl BrokerSocketSeal {
    fn verify_after_drop(&self, uid: u32, gid: u32) -> std::io::Result<()> {
        if self.socket.identity.uid == uid || self.socket.identity.writable_by(uid, gid) {
            return Err(raw_error(libc::EPERM));
        }
        verify_path_identity(&self.socket)?;
        for ancestor in &self.ancestors {
            if ancestor.identity.uid == uid || ancestor.identity.writable_by(uid, gid) {
                return Err(raw_error(libc::EPERM));
            }
            verify_path_identity(ancestor)?;
            verify_not_writable(ancestor)?;
        }
        if fstat_identity(self.pinned_socket.as_raw_fd())? != self.socket.identity {
            return Err(raw_error(libc::ESTALE));
        }

        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = self.socket.path.as_bytes_with_nul();
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr().cast::<libc::c_char>(),
                address.sun_path.as_mut_ptr(),
                bytes.len(),
            );
        }
        let connected = unsafe {
            libc::connect(
                fd,
                std::ptr::addr_of!(address).cast::<libc::sockaddr>(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        let connect_error = if connected == 0 {
            None
        } else {
            Some(std::io::Error::last_os_error())
        };
        unsafe {
            libc::close(fd);
        }
        match connect_error.and_then(|error| error.raw_os_error()) {
            Some(libc::EACCES | libc::EPERM) => {}
            _ => return Err(raw_error(libc::EPERM)),
        }

        verify_path_identity(&self.socket)?;
        for ancestor in &self.ancestors {
            verify_path_identity(ancestor)?;
            verify_not_writable(ancestor)?;
        }
        if fstat_identity(self.pinned_socket.as_raw_fd())? != self.socket.identity {
            return Err(raw_error(libc::ESTALE));
        }
        Ok(())
    }
}

fn verify_not_writable(path: &SealedPath) -> std::io::Result<()> {
    if unsafe { libc::access(path.path.as_ptr(), libc::W_OK) } == 0 {
        return Err(raw_error(libc::EPERM));
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EACCES | libc::EPERM) => Ok(()),
        _ => Err(raw_error(libc::EPERM)),
    }
}

fn sealed_path(path: &Path) -> Result<SealedPath, String> {
    let path_c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("path contains NUL: {}", path.display()))?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("path must not be a symlink: {}", path.display()));
    }
    Ok(SealedPath {
        path: path_c,
        identity: FileIdentity::from_metadata(&metadata),
    })
}

fn fstat_identity(fd: RawFd) -> std::io::Result<FileIdentity> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, std::ptr::addr_of_mut!(stat)) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(FileIdentity::from_stat(&stat))
}

fn verify_path_identity(path: &SealedPath) -> std::io::Result<()> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::lstat(path.path.as_ptr(), std::ptr::addr_of_mut!(stat)) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if FileIdentity::from_stat(&stat) != path.identity {
        return Err(raw_error(libc::ESTALE));
    }
    Ok(())
}

pub fn resolve_isolated_execution_gid() -> Result<u32, String> {
    let name =
        std::env::var(ISOLATED_GROUP_ENV).unwrap_or_else(|_| DEFAULT_ISOLATED_GROUP.to_string());
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid isolated execution group name `{name}`"));
    }
    let gid = group_gid(&name)?.ok_or_else(|| {
        format!(
            "isolated execution group `{name}` does not exist; reinstall the claw-os-agent package"
        )
    })?;
    if gid == 0 {
        return Err(format!(
            "isolated execution group `{name}` resolves to root gid 0"
        ));
    }
    Ok(gid)
}

fn group_gid(name: &str) -> Result<Option<u32>, String> {
    use std::ffi::CStr;

    let name = CString::new(name).map_err(|_| "group name contains NUL".to_string())?;
    const BUF_SIZE: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; BUF_SIZE];
    let mut group: libc::group = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe {
        libc::getgrnam_r(
            name.as_ptr(),
            &mut group,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 {
        return Err(format!(
            "group lookup failed: {}",
            std::io::Error::from_raw_os_error(rc)
        ));
    }
    if result.is_null() {
        return Ok(None);
    }
    if group.gr_name.is_null() {
        return Err("group lookup returned no name".to_string());
    }
    let _ = unsafe { CStr::from_ptr(group.gr_name) }
        .to_str()
        .map_err(|_| "group name is not UTF-8".to_string())?;
    Ok(Some(group.gr_gid))
}

#[derive(Debug)]
pub struct SpawnedWorker {
    pub child: tokio::process::Child,
    pub channel: tokio::net::UnixStream,
    pub pid: u32,
    pub start_time_ticks: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct WorkerIdentity {
    pub uid: u32,
    pub gid: u32,
    pub username: String,
    pub home: PathBuf,
}

/// Resolve the owner account the worker must run as.
///
/// A task owned by root is refused outright: there is no account to
/// drop to, so running it would put the model/tool loop back in a root
/// process. The caller must fail such a task before any provider, MCP
/// client or worker process is initialised.
pub fn resolve_identity(owner_uid: u32) -> Result<WorkerIdentity, String> {
    if owner_uid == 0 {
        return Err(ROOT_OWNER_REFUSAL.to_string());
    }
    let home = crate::paths::verified_home_for_uid(owner_uid)?;
    let (gid, username) = account_for_uid(owner_uid)?;
    if gid == 0 {
        return Err(format!(
            "refusing to run an agent worker for uid {owner_uid} with primary gid 0"
        ));
    }
    Ok(WorkerIdentity {
        uid: owner_uid,
        gid,
        username,
        home,
    })
}

/// Single wording for the root-owner refusal so the task error, the
/// audit record and the tests all agree.
pub const ROOT_OWNER_REFUSAL: &str = "refusing to run an agent task owned by root: the agent \
     runtime must run as a non-root account. Submit the task from a non-root user, or give the \
     agent its own unprivileged account.";

/// Path to the worker executable. `COS_AGENTD_BIN` wins for tests and
/// dev trees; otherwise the binary installed beside `clawd`, then the
/// packaged location.
pub fn worker_binary_path() -> PathBuf {
    if let Some(path) = std::env::var_os("COS_AGENTD_BIN") {
        return PathBuf::from(path);
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let sibling = dir.join("claw-agentd");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("/usr/local/bin/claw-agentd")
}

/// True when the supervisor itself is privileged — the production
/// configuration, where the child must be forced down to the owner's
/// account. A dev tree or test harness running as an ordinary user
/// cannot call `setgroups`/`setresuid` at all; there the child is
/// already unprivileged and the drop is a no-op.
pub fn broker_is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Fork and exec the worker with the broker end of a private channel
/// retained here. Returns before the assignment is written, so the
/// caller can bind the grant to the pid the kernel actually allocated.
pub fn spawn_worker(
    identity: &WorkerIdentity,
    isolation: &ExecutionIsolation,
    _task_id: &str,
) -> Result<SpawnedWorker, String> {
    let binary = worker_binary_path();
    if !binary.exists() {
        return Err(format!(
            "agent worker binary is not installed at {}",
            binary.display()
        ));
    }
    if broker_is_root() {
        validate_root_owned_executable(&binary)?;
    }

    let (broker_end, worker_end) = std::os::unix::net::UnixStream::pair()
        .map_err(|error| format!("create agentd channel: {error}"))?;
    broker_end
        .set_nonblocking(true)
        .map_err(|error| format!("configure agentd channel: {error}"))?;

    let mut command = std::process::Command::new(&binary);
    command.arg("--worker");
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.current_dir(&identity.home);
    command.env_clear();
    command.env("HOME", &identity.home);
    command.env("USER", &identity.username);
    command.env("LOGNAME", &identity.username);
    command.env("PATH", WORKER_PATH);
    command.env("SHELL", "/bin/sh");
    command.env(protocol::CHANNEL_FD_ENV, protocol::CHANNEL_FD.to_string());
    for key in INHERITED_ENV_KEYS {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(debug_assertions)]
    if let Some(value) = std::env::var_os("COS_AGENTD_TEST_COMMIT_MARKER") {
        command.env("COS_AGENTD_TEST_COMMIT_MARKER", value);
    }

    let channel_fd = worker_end.as_raw_fd();
    let expected_parent = unsafe { libc::getpid() };
    let uid = identity.uid;
    let gid = isolation.execution_gid();
    let enforce_groups = broker_is_root();
    let isolation = isolation.clone();
    unsafe {
        command.pre_exec(move || {
            libc::umask(0o077);
            place_channel_fd(channel_fd)?;
            close_inherited_descriptors();
            drop_to_owner(uid, gid)?;
            verify_dropped_identity(uid, gid, enforce_groups)?;
            isolation.verify_after_drop(uid)?;
            harden_child(expected_parent)?;
            Ok(())
        });
    }

    let child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", binary.display()))?;
    // The worker holds the only other reference; dropping ours here is
    // what makes the channel report EOF when the worker exits.
    drop(worker_end);

    let pid = child
        .id()
        .ok_or_else(|| "agent worker exited before it could be identified".to_string())?;
    let start_time_ticks = crate::proc::read_start_time_ticks_pub(pid);
    let channel = tokio::net::UnixStream::from_std(broker_end)
        .map_err(|error| format!("register agentd channel: {error}"))?;

    Ok(SpawnedWorker {
        child,
        channel,
        pid,
        start_time_ticks,
    })
}

/// Refuse to exec a worker image any non-root account could have
/// replaced. Mirrors the App-runner check so both privileged launch
/// paths agree on what "trusted image" means.
pub(crate) fn validate_root_owned_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "agent worker binary must not be a symlink: {}",
            path.display()
        ));
    }
    if metadata.uid() != 0 {
        return Err(format!(
            "agent worker binary {} is not owned by root",
            path.display()
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "agent worker binary {} is group or world writable",
            path.display()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// post-fork, pre-exec
//
// Everything below runs in the forked child between `fork` and `execve`.
// Only the calling thread survives the fork, so any allocator or lock a
// dropped thread was holding stays locked forever. Nothing here may
// allocate, format, log, or take a lock: errors are reported as bare
// `errno` values via `Error::from_raw_os_error`, which `std` then writes
// to its exec-status pipe as a raw integer.
// ---------------------------------------------------------------------------

/// Allocation-free error for a post-fork failure that has no `errno` of
/// its own. The parent surfaces it as an ordinary spawn failure.
pub(crate) fn raw_error(errno: libc::c_int) -> std::io::Error {
    std::io::Error::from_raw_os_error(errno)
}

pub(crate) fn set_process_undumpable() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(format!(
            "set process non-dumpable: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn place_channel_fd(channel_fd: RawFd) -> std::io::Result<()> {
    if channel_fd == protocol::CHANNEL_FD {
        // `dup2(fd, fd)` is a no-op and would leave FD_CLOEXEC set.
        if unsafe { libc::fcntl(protocol::CHANNEL_FD, libc::F_SETFD, 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        return Ok(());
    }
    if unsafe { libc::dup2(channel_fd, protocol::CHANNEL_FD) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Mark every descriptor above the channel close-on-exec. `std` opens
/// its own handles `O_CLOEXEC`, but the daemon also holds `flock`
/// sentinels, listener sockets and library-opened handles; doing this
/// explicitly is what makes "no root-only descriptor survives into the
/// worker image" a property rather than an assumption.
///
/// Marking rather than closing matters: `std` reports a failed
/// `pre_exec` to the parent over a descriptor in this range, and that
/// descriptor has to stay usable until `execve` replaces the image.
fn close_inherited_descriptors() {
    mark_inherited_descriptors_cloexec(protocol::CHANNEL_FD + 1);
}

/// Mark every inherited descriptor at or above `first` close-on-exec.
///
/// The extension host has no broker descriptor at all and calls this with
/// `3`; the agent worker preserves fd 3 and calls it with `4`.
pub(crate) fn mark_inherited_descriptors_cloexec(first: RawFd) {
    #[cfg(target_os = "linux")]
    {
        const CLOSE_RANGE_CLOEXEC: libc::c_uint = 4;
        let rc = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                first as libc::c_uint,
                libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            )
        };
        if rc == 0 {
            return;
        }

    }
    let mut fd = first;
    while fd < 4096 {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags >= 0 {
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
        fd += 1;
    }
}

pub(crate) fn mark_inherited_descriptors_cloexec_except(first: RawFd, preserve: RawFd) {
    let mut fd = first;
    while fd < 4096 {
        if fd != preserve {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags >= 0 {
                unsafe {
                    libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
            }
        }
        fd += 1;
    }
}

pub(crate) fn drop_to_owner(uid: u32, gid: u32) -> std::io::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        // Unprivileged supervisor (dev tree / tests): there is nothing
        // to drop, and `setgroups` would fail with EPERM. The identity
        // check below still runs, and `verify_dropped_identity` refuses
        // to continue if the child is not already the target account.
        return Ok(());
    }
    // Supplementary groups first: after `setresuid` the child no longer
    // has the privilege to clear them.
    if unsafe { libc::setgroups(0, std::ptr::null()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::setresgid(gid, gid, gid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::setresuid(uid, uid, uid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn verify_dropped_identity(
    uid: u32,
    gid: u32,
    enforce_groups: bool,
) -> std::io::Result<()> {
    let mut ruid: libc::uid_t = 0;
    let mut euid: libc::uid_t = 0;
    let mut suid: libc::uid_t = 0;
    if unsafe { libc::getresuid(&mut ruid, &mut euid, &mut suid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if ruid != uid || euid != uid || suid != uid {
        return Err(raw_error(libc::EPERM));
    }
    let mut rgid: libc::gid_t = 0;
    let mut egid: libc::gid_t = 0;
    let mut sgid: libc::gid_t = 0;
    if unsafe { libc::getresgid(&mut rgid, &mut egid, &mut sgid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if rgid != gid || egid != gid || sgid != gid {
        return Err(raw_error(libc::EPERM));
    }
    // A stack buffer keeps this allocation-free. Production clears the
    // list completely before the uid drop; retaining even the primary
    // gid as a supplementary group would make group-based socket access
    // ambiguous and is refused.
    if enforce_groups {
        let mut groups = [0 as libc::gid_t; 64];
        let count = unsafe { libc::getgroups(groups.len() as libc::c_int, groups.as_mut_ptr()) };
        if count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if count != 0 {
            return Err(raw_error(libc::EPERM));
        }
    }
    Ok(())
}

pub(crate) fn harden_child(expected_parent: libc::pid_t) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // Closes the race where the supervisor died between `fork` and
        // the `prctl` above, which would leave the signal armed against
        // a parent that is already gone.
        if unsafe { libc::getppid() } != expected_parent {
            return Err(raw_error(libc::ESRCH));
        }
        // New session and process group before the runtime starts, so a
        // cancelled or crashed task can be terminated as a whole group
        // and no App or MCP descendant survives it. Also drops any
        // controlling terminal the daemon happened to hold.
        if unsafe { libc::setsid() } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = expected_parent;
    }
    Ok(())
}

/// Signal the worker's whole process group, so App and MCP descendants
/// it started do not survive a cancellation, a lease expiry or a crash.
///
/// Safe against pid recycling only while the caller still holds the
/// unreaped child: the pid cannot be reused until it is waited on. The
/// group id is re-read from the kernel and the signal is sent only when
/// it is the worker's own session, which `setsid` in `pre_exec`
/// guarantees — so a worker that somehow shares a group is never used
/// as a lever to signal unrelated processes.
///
/// # Safety
///
/// `pid` must still be an unreaped child of this process.
pub unsafe fn terminate_worker_group(pid: u32, signal: libc::c_int) {
    let pid = pid as libc::pid_t;
    let pgid = libc::getpgid(pid);
    if pgid != pid {
        return;
    }
    libc::kill(-pgid, signal);
}

fn account_for_uid(uid: u32) -> Result<(u32, String), String> {
    use std::ffi::CStr;

    const BUF_SIZE: usize = 16 * 1024;
    let mut buf = vec![0 as libc::c_char; BUF_SIZE];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid as libc::uid_t,
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return Err(format!(
            "passwd lookup failed for agent task owner uid {uid}"
        ));
    }
    if pwd.pw_name.is_null() {
        return Err(format!("passwd entry for uid {uid} has no name"));
    }
    let username = unsafe { CStr::from_ptr(pwd.pw_name) }
        .to_str()
        .map_err(|_| format!("passwd name for uid {uid} is not UTF-8"))?
        .to_string();
    // Reject anything that could not survive a `CString` round-trip
    // into the child environment.
    CString::new(username.as_bytes())
        .map_err(|_| format!("passwd name for uid {uid} contains NUL"))?;
    Ok((pwd.pw_gid as u32, username))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/spawn.rs"
    ));
}
