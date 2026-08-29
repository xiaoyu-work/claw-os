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
//! 4. `setresgid` then `setresuid` to the owner's account, real,
//!    effective and saved ids together, so nothing can be restored.
//! 5. Re-read every id and the supplementary group list from the kernel
//!    and abort the `exec` if any of it is wrong. `Command::uid` alone
//!    proves nothing; this is the check that does.
//! 6. `PR_SET_PDEATHSIG` plus a `getppid` re-check so a worker cannot
//!    outlive the supervisor that leased it, then
//!    `PR_SET_NO_NEW_PRIVS` so no setuid binary can raise privilege
//!    again inside the worker.
//!
//! The environment is rebuilt from an allowlist rather than filtered,
//! and no credential value is ever placed in it: the worker reads the
//! owner's own credential store as the owner.

use std::ffi::CString;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::protocol;

/// Environment keys copied from the broker when present. Everything
/// else is dropped: the worker starts from an empty environment.
const INHERITED_ENV_KEYS: &[&str] = &[
    "COS_APPS_DIR",
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
pub fn spawn_worker(identity: &WorkerIdentity, _task_id: &str) -> Result<SpawnedWorker, String> {
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

    let channel_fd = worker_end.as_raw_fd();
    let expected_parent = unsafe { libc::getpid() };
    let uid = identity.uid;
    let gid = identity.gid;
    let enforce_groups = broker_is_root();
    unsafe {
        command.pre_exec(move || {
            libc::umask(0o077);
            place_channel_fd(channel_fd)?;
            close_inherited_descriptors();
            drop_to_owner(uid, gid)?;
            verify_dropped_identity(uid, gid, enforce_groups)?;
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
fn validate_root_owned_executable(path: &Path) -> Result<(), String> {
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
fn raw_error(errno: libc::c_int) -> std::io::Error {
    std::io::Error::from_raw_os_error(errno)
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
    let first = protocol::CHANNEL_FD + 1;
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

fn drop_to_owner(uid: u32, gid: u32) -> std::io::Result<()> {
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

fn verify_dropped_identity(uid: u32, gid: u32, enforce_groups: bool) -> std::io::Result<()> {
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
    // A stack buffer keeps this allocation-free. Any supplementary
    // group other than the primary gid means `setgroups` was defeated.
    if enforce_groups {
        let mut groups = [0 as libc::gid_t; 64];
        let count = unsafe { libc::getgroups(groups.len() as libc::c_int, groups.as_mut_ptr()) };
        if count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        for entry in groups.iter().take(count as usize) {
            if *entry != gid {
                return Err(raw_error(libc::EPERM));
            }
        }
    }
    Ok(())
}

fn harden_child(expected_parent: libc::pid_t) -> std::io::Result<()> {
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
