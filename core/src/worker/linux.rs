//! Linux provider: bubblewrap namespaces + seccomp + a resource
//! governor.
//!
//! Layout of one launch, outermost first:
//!
//! ```text
//! launcher
//!  └─ pre_exec: setsid, PDEATHSIG, umask, rlimits, cgroup join,
//!               supplementary-group reset, uid/gid drop, seccomp fd
//!     └─ bwrap: user + mount + pid + ipc + uts + net namespaces,
//!               read-only root, explicit binds, all capabilities
//!               dropped, further user namespaces disabled, seccomp
//!               filter installed
//!        └─ worker (pid 1 of its own pid namespace)
//! ```
//!
//! Everything the worker can see is named by the policy. There is no
//! "inherit the rest" step anywhere in this file: the environment is
//! cleared and rebuilt, the filesystem starts from an empty tmpfs
//! root, the network namespace starts empty, and the only descriptors
//! that survive are the three standard streams plus the seccomp
//! program bubblewrap consumes during setup.

#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::policy::{LaunchPolicy, MountClass, MountMode, NetworkPolicy};
use super::provider::{
    Availability, Governor, LaunchResources, PreparedLaunch, WorkerLaunch, WorkerSandbox,
};

/// Descriptor bubblewrap reads the seccomp program from. Chosen high
/// enough that it cannot collide with the standard streams or with a
/// descriptor the launcher happens to hold.
const SECCOMP_FD: libc::c_int = 100;

/// First descriptor number the pinned bind sources are moved to.
const PINNED_FD_BASE: libc::c_int = 200;

/// Bind sources held open across the launch.
///
/// A path that was canonical, non-symlink and inside policy when it was
/// validated can be swapped for a symlink, a different directory or a
/// fresh mount before bubblewrap resolves it — a TOCTOU the whole mount
/// policy would otherwise rest on. Opening each source `O_PATH |
/// O_NOFOLLOW` pins the exact inode, re-checking `st_dev`/`st_ino`
/// against what validation saw proves nothing changed in between, and
/// binding `/proc/self/fd/N` makes bubblewrap use the pinned inode
/// rather than re-walking the name.
#[derive(Debug)]
pub struct PinnedSources {
    /// Mounts rewritten to point at the pinned descriptors.
    mounts: Vec<super::policy::Mount>,
    /// Held open until after `exec`; dropping them releases the pins.
    files: Vec<std::fs::File>,
}

impl PinnedSources {
    fn open(mounts: &[super::policy::Mount]) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::io::AsRawFd;

        let mut files = Vec::with_capacity(mounts.len());
        let mut pinned = Vec::with_capacity(mounts.len());
        for mount in mounts {
            let before = std::fs::symlink_metadata(&mount.source)
                .map_err(|error| format!("inspect worker mount source: {error}"))?;
            if before.file_type().is_symlink() {
                return Err("worker mount source became a symlink".to_string());
            }
            let file = open_path_nofollow(&mount.source)?;
            let after = file
                .metadata()
                .map_err(|error| format!("re-inspect worker mount source: {error}"))?;
            // Identity revalidation: the inode we pinned has to be the
            // one policy approved. A swap between the two calls changes
            // one of these.
            if after.dev() != before.dev() || after.ino() != before.ino() {
                return Err("worker mount source changed during launch setup".to_string());
            }
            let index = files.len();
            let target_fd = PINNED_FD_BASE + index as libc::c_int;
            pinned.push(super::policy::Mount {
                source: PathBuf::from(format!("/proc/self/fd/{target_fd}")),
                target: mount.target.clone(),
                mode: mount.mode,
                class: mount.class,
            });
            let _ = file.as_raw_fd();
            files.push(file);
        }
        Ok(Self {
            mounts: pinned,
            files,
        })
    }

    fn raw_fds(&self) -> Vec<libc::c_int> {
        use std::os::unix::io::AsRawFd;
        self.files.iter().map(|file| file.as_raw_fd()).collect()
    }
}

/// `open(path, O_PATH | O_NOFOLLOW | O_CLOEXEC)`.
///
/// `O_PATH` gives a descriptor that names the inode without granting
/// read or write access, which is all a bind source needs; `O_NOFOLLOW`
/// refuses a final component that turned into a symlink.
fn open_path_nofollow(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "worker mount source is not representable".to_string())?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "pin worker mount source: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Path the narrow broker endpoint appears at inside the sandbox.
///
/// Deliberately the same path the real broker uses on the host, so
/// `cos` inside the sandbox needs no special case — and so the real
/// socket is *shadowed* rather than merely absent.
pub const SANDBOX_BROKER_SOCKET: &str = "/run/cos/clawd.sock";

/// Path the egress broker appears at inside the sandbox.
pub const SANDBOX_EGRESS_SOCKET: &str = "/run/cos/worker-egress.sock";

/// Read-only host paths every worker needs to run an interpreter.
/// `-try` semantics: a host that does not have one simply does not get
/// it bound, which is a smaller sandbox, never a larger one.
const SYSTEM_PATHS: &[&str] = &[
    "/usr",
    "/etc/alternatives",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/localtime",
    "/etc/nsswitch.conf",
    "/etc/ssl",
    "/etc/pki",
    "/etc/ca-certificates",
    "/etc/ca-certificates.conf",
    "/etc/python3",
    "/etc/python3.11",
    "/etc/python3.12",
    "/etc/python3.13",
];

pub struct LinuxSandbox;

impl WorkerSandbox for LinuxSandbox {
    fn name(&self) -> &'static str {
        "linux-bwrap"
    }

    fn availability(&self) -> Availability {
        let mut missing = Vec::new();
        match which("bwrap") {
            None => missing.push("bubblewrap (`bwrap`) is not installed".to_string()),
            Some(path) => {
                if !bwrap_disables_userns(&path) {
                    missing.push(
                        "bubblewrap is too old: `--disable-userns` (0.8+) is required so a \
                         worker cannot re-enter a user namespace"
                            .to_string(),
                    );
                }
            }
        }
        if !user_namespaces_enabled() {
            missing.push("unprivileged user namespaces are disabled".to_string());
        }
        if !seccomp_supported() {
            missing.push("seccomp filtering is unavailable on this kernel".to_string());
        }
        Availability {
            provider: "linux-bwrap",
            governor: Some(if super::cgroup::is_available() {
                Governor::Cgroup
            } else {
                Governor::Rlimit
            }),
            missing,
        }
    }

    fn prepare(&self, launch: &WorkerLaunch) -> Result<PreparedLaunch, String> {
        let policy = &launch.policy;
        let bwrap = which("bwrap")
            .ok_or_else(|| "worker isolation unavailable: `bwrap` is missing".to_string())?;
        let (uid, gid) = worker_identity()?;
        let id = super::runtime::launch_id();

        let mut resources = LaunchResources::empty();
        let mut mounts = policy.mounts.clone();

        if policy.broker || matches!(policy.network, NetworkPolicy::Brokered { .. }) {
            let dir = super::runtime::LaunchDir::create(&id, Some((uid, gid)))?;
            if policy.broker {
                let authority = launch.authority.as_ref().ok_or_else(|| {
                    "worker launch requests a broker endpoint without an authority".to_string()
                })?;
                let endpoint = super::broker::BrokerEndpoint::start(
                    dir.child("broker.sock"),
                    authority.clone(),
                    uid,
                )?;
                mounts.push(super::policy::Mount::read_write(
                    endpoint.socket_path().to_path_buf(),
                    PathBuf::from(SANDBOX_BROKER_SOCKET),
                    MountClass::BrokerIpc,
                ));
                resources.broker = Some(endpoint);
            }
            if let NetworkPolicy::Brokered { endpoints } = &policy.network {
                let egress = super::net_broker::EgressEndpoint::start(
                    dir.child("egress.sock"),
                    endpoints.clone(),
                    uid,
                )?;
                mounts.push(super::policy::Mount::read_write(
                    egress.socket_path().to_path_buf(),
                    PathBuf::from(SANDBOX_EGRESS_SOCKET),
                    MountClass::BrokerIpc,
                ));
                resources.egress = Some(egress);
            }
            resources.runtime_dir = Some(dir);
        }

        let pinned = PinnedSources::open(&mounts)?;
        let args = build_bwrap_args(policy, &pinned.mounts, uid, gid, &host_layout());
        let seccomp = seccomp_descriptor(policy.seccomp)?;
        let seccomp_raw = {
            use std::os::unix::io::AsRawFd;
            seccomp.as_raw_fd()
        };
        let cgroup = match super::cgroup::is_available() {
            true => Some(super::cgroup::create(
                &format!("cos-worker-{id}"),
                &policy.limits,
            )?),
            false => None,
        };
        let governor = if cgroup.is_some() {
            Governor::Cgroup
        } else {
            Governor::Rlimit
        };
        let cgroup_procs = cgroup
            .as_ref()
            .map(|scope| scope.path().join("cgroup.procs"));

        let mut command = Command::new(bwrap);
        command.args(&args);
        command.env_clear();
        for (name, value) in &policy.env {
            command.env(name, value);
        }
        install_pre_exec(
            &mut command,
            PreExecPlan {
                seccomp_fd: seccomp_raw,
                pinned_fds: pinned.raw_fds(),
                cgroup_procs,
                umask: policy.umask,
                limits: policy.limits,
                nproc_ceiling: current_task_count().saturating_add(policy.limits.pids_max as u64),
                identity: (uid, gid),
            },
        );

        let facts = {
            let mut facts = policy.audit_facts();
            facts["governor"] = serde_json::json!(governor.as_str());
            facts["provider"] = serde_json::json!("linux-bwrap");
            facts
        };
        resources.seccomp = Some(seccomp);
        resources.pinned = Some(pinned);
        resources.cgroup = cgroup;
        Ok(PreparedLaunch {
            command,
            facts,
            governor,
            resources,
        })
    }
}

/// Which of `/bin`, `/lib`, `/lib64`, `/sbin` are symlinks into
/// `/usr` on this host. Inspected once per launch by trusted code so
/// the sandbox root matches the host's merged-`/usr` layout.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostLayout {
    pub bin_is_symlink: bool,
    pub sbin_is_symlink: bool,
    pub lib_is_symlink: bool,
    pub lib64_is_symlink: bool,
    pub lib64_exists: bool,
}

fn host_layout() -> HostLayout {
    let symlink = |path: &str| {
        std::fs::symlink_metadata(path)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false)
    };
    HostLayout {
        bin_is_symlink: symlink("/bin"),
        sbin_is_symlink: symlink("/sbin"),
        lib_is_symlink: symlink("/lib"),
        lib64_is_symlink: symlink("/lib64"),
        lib64_exists: Path::new("/lib64").exists(),
    }
}

/// Build the bubblewrap argument vector.
///
/// Pure: it inspects nothing and allocates nothing outside the
/// returned vector, so the exact isolation contract can be asserted in
/// unit tests on any host.
pub fn build_bwrap_args(
    policy: &LaunchPolicy,
    mounts: &[super::policy::Mount],
    uid: u32,
    gid: u32,
    layout: &HostLayout,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    macro_rules! push {
        ($($value:expr),+ $(,)?) => {
            $( args.push($value.to_string()); )+
        };
    }

    // Namespaces. `--unshare-all` is deliberately not used: it implies
    // `--unshare-user-try`, which silently continues without a user
    // namespace on a host that forbids one. We name each namespace so
    // a host that cannot provide it fails the launch instead.
    // bubblewrap always creates a mount namespace; the rest are named
    // one by one so a host that cannot provide one fails the launch.
    push!("--unshare-user");
    push!("--unshare-pid");
    push!("--unshare-ipc");
    push!("--unshare-uts");
    push!("--unshare-cgroup-try");
    push!("--unshare-net");
    // Re-entering a user namespace is the standard way back to
    // CAP_SYS_ADMIN and therefore back to mount(2); refuse to launch if
    // the kernel cannot turn it off.
    push!("--disable-userns");
    push!("--assert-userns-disabled");
    push!("--cap-drop", "ALL");
    push!("--new-session");
    push!("--die-with-parent");
    push!("--hostname", "claw-worker");
    push!("--uid", uid);
    push!("--gid", gid);

    // Read-only system image.
    for path in SYSTEM_PATHS {
        push!("--ro-bind-try", path, path);
    }
    for (link, target, is_symlink, exists) in [
        ("/bin", "usr/bin", layout.bin_is_symlink, true),
        ("/sbin", "usr/sbin", layout.sbin_is_symlink, true),
        ("/lib", "usr/lib", layout.lib_is_symlink, true),
        (
            "/lib64",
            "usr/lib64",
            layout.lib64_is_symlink,
            layout.lib64_exists,
        ),
    ] {
        if !exists {
            continue;
        }
        if is_symlink {
            push!("--symlink", target, link);
        } else {
            push!("--ro-bind-try", link, link);
        }
    }

    // Private kernel and volatile filesystems. The procfs instance is
    // mounted inside the new pid namespace, so it lists the sandbox's
    // processes and nothing else.
    //
    // `--size` sets the size of the *next* argument, so it precedes the
    // `--tmpfs` it bounds. Emitting it afterwards silently leaves the
    // filesystem unbounded and applies the number to whatever came
    // next.
    push!("--proc", "/proc");
    push!("--dev", "/dev");
    push!("--size", policy.limits.file_size_bytes, "--tmpfs", "/run");
    push!("--size", policy.limits.file_size_bytes, "--tmpfs", "/tmp");
    push!(
        "--size",
        policy.limits.file_size_bytes,
        "--tmpfs",
        "/var/tmp"
    );
    push!("--dir", "/var");
    push!("--dir", "/home");

    for mount in mounts {
        push!(match (mount.class, mount.mode) {
            // A device node is only usable when the bind carries
            // `nodev` off, and only the desktop tier can hold one.
            (MountClass::Device, _) => "--dev-bind",
            (_, MountMode::ReadOnly) => "--ro-bind",
            (_, MountMode::ReadWrite) => "--bind",
        });
        push!(mount.source.display(), mount.target.display());
    }

    // The root tmpfs itself becomes read-only after every bind is in
    // place; the binds keep their own flags.
    push!("--remount-ro", "/");

    push!("--chdir", policy.workdir.display());
    push!("--seccomp", SECCOMP_FD);
    push!("--", policy.program.display());
    args.extend(policy.argv.iter().cloned());
    args
}

struct PreExecPlan {
    seccomp_fd: libc::c_int,
    /// Bind-source descriptors, moved to `PINNED_FD_BASE + i` with
    /// `FD_CLOEXEC` cleared so bubblewrap can resolve
    /// `/proc/self/fd/<n>` after `execve`.
    pinned_fds: Vec<libc::c_int>,
    cgroup_procs: Option<PathBuf>,
    umask: u32,
    limits: super::policy::Limits,
    /// `RLIMIT_NPROC` ceiling, precomputed in the parent.
    nproc_ceiling: u64,
    identity: (u32, u32),
}

/// Everything that must happen in the forked child before `bwrap`
/// runs. Ordering matters and is load-bearing:
///
/// 1. `setsid` so the launcher can signal the entire process group;
/// 2. `PDEATHSIG` so a launcher crash kills the worker;
/// 3. cgroup join, while we still hold the launcher's identity;
/// 4. supplementary-group reset and uid/gid drop, if privileged;
/// 5. rlimits and umask;
/// 6. the seccomp descriptor moved to the number `bwrap` is told to
///    read, with `FD_CLOEXEC` cleared so it survives `execve`.
fn install_pre_exec(command: &mut Command, plan: PreExecPlan) {
    use std::os::unix::process::CommandExt;

    let PreExecPlan {
        seccomp_fd,
        pinned_fds,
        cgroup_procs,
        umask,
        limits,
        nproc_ceiling,
        identity: (uid, gid),
    } = plan;
    let parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 && libc::getpgrp() != libc::getpid() {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "worker launcher exited before sandbox setup completed",
                ));
            }
            if let Some(path) = &cgroup_procs {
                let pid = libc::getpid().to_string();
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .and_then(|mut file| file.write_all(pid.as_bytes()))?;
            }
            if libc::geteuid() == 0 {
                if libc::setgroups(0, std::ptr::null()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setgid(gid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setuid(uid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setuid(0) == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "worker privilege drop did not stick",
                    ));
                }
            }
            libc::umask(umask as libc::mode_t);
            apply_rlimits(&limits, nproc_ceiling)?;
            if libc::dup2(seccomp_fd, SECCOMP_FD) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // `dup2` clears `FD_CLOEXEC` on the new descriptor, which is
            // exactly what makes these survive into bubblewrap.
            for (index, source) in pinned_fds.iter().enumerate() {
                let target = PINNED_FD_BASE + index as libc::c_int;
                if libc::dup2(*source, target) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// POSIX ceilings that apply whether or not a cgroup is available.
/// They bound descriptor exhaustion, fork bombs, core dumps and file
/// growth; the cgroup, when present, adds memory and CPU on top.
fn apply_rlimits(limits: &super::policy::Limits, nproc_ceiling: u64) -> std::io::Result<()> {
    let set = |resource: libc::__rlimit_resource_t, value: u64| -> std::io::Result<()> {
        let limit = libc::rlimit {
            rlim_cur: value as libc::rlim_t,
            rlim_max: value as libc::rlim_t,
        };
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    };
    set(libc::RLIMIT_CORE, 0)?;
    set(libc::RLIMIT_NOFILE, limits.open_files)?;
    set(libc::RLIMIT_FSIZE, limits.file_size_bytes)?;
    // `RLIMIT_NPROC` counts every process the *real uid* already owns,
    // not just this sandbox's, so a bare `pids_max` here would refuse
    // to create the worker at all on a busy machine — and would do it
    // as `EAGAIN` from `clone`, which reads like a kernel fault rather
    // than a policy decision. The ceiling is therefore the current
    // count plus the policy's allowance, computed in the parent. A
    // cgroup, when one is available, enforces the same ceiling exactly
    // and this stays a backstop.
    set(libc::RLIMIT_NPROC, nproc_ceiling)?;
    if let Some(deadline) = limits.deadline() {
        // CPU seconds, not wall clock: the launcher owns the wall
        // clock. A spinning worker is stopped even if it never blocks.
        set(libc::RLIMIT_CPU, deadline.as_secs().max(1))?;
    }
    Ok(())
}

/// Tasks already owned by the account the worker runs as.
///
/// `RLIMIT_NPROC` is charged per real uid and counts *threads*, not
/// processes, so a process-level count under-reports badly on a
/// multi-threaded launcher and the resulting ceiling would refuse the
/// worker's very first `clone` with `EAGAIN`. Read in the launcher,
/// before `fork`, so `pre_exec` allocates nothing.
fn current_task_count() -> u64 {
    use std::os::unix::fs::MetadataExt;

    let uid = unsafe { libc::getuid() };
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
            entry
                .metadata()
                .map(|metadata| metadata.uid() == uid)
                .unwrap_or(false)
        })
        .map(|entry| {
            std::fs::read_dir(entry.path().join("task"))
                .map(|tasks| tasks.count() as u64)
                .unwrap_or(1)
        })
        .sum()
}

/// Write the seccomp program into a pipe and hand back the read end.
/// bubblewrap reads the descriptor to EOF during setup, so the write
/// end is closed before the child is spawned.
fn seccomp_descriptor(profile: super::policy::SeccompProfile) -> Result<std::fs::File, String> {
    use std::os::unix::io::FromRawFd;

    let program = super::seccomp::encoded(profile);
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(format!(
            "create seccomp pipe: {}",
            std::io::Error::last_os_error()
        ));
    }
    let (read, write) = unsafe {
        (
            std::fs::File::from_raw_fd(fds[0]),
            std::fs::File::from_raw_fd(fds[1]),
        )
    };
    let mut write = write;
    write
        .write_all(&program)
        .map_err(|error| format!("write seccomp program: {error}"))?;
    drop(write);
    Ok(read)
}

/// The account a worker runs as. Root is never a valid answer: a
/// worker that starts as uid 0 could keep capabilities in its own user
/// namespace no matter what the mount policy says.
fn worker_identity() -> Result<(u32, u32), String> {
    if let Some(uid) = crate::paths::current_owner_uid_override() {
        if uid == 0 {
            return Err("refusing to run a worker as root".to_string());
        }
        return Ok((uid, primary_gid(uid)?));
    }
    let uid = unsafe { libc::geteuid() };
    if uid == 0 {
        return Err("refusing to run a worker as root without an owner identity".to_string());
    }
    Ok((uid, unsafe { libc::getegid() }))
}

fn primary_gid(uid: u32) -> Result<u32, String> {
    const BUFFER: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; BUFFER];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let code = unsafe {
        libc::getpwuid_r(
            uid,
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if code != 0 || result.is_null() {
        return Err(format!("passwd lookup failed for worker uid {uid}"));
    }
    Ok(passwd.pw_gid)
}

fn which(program: &str) -> Option<PathBuf> {
    for root in ["/usr/bin", "/bin", "/usr/local/bin", "/usr/sbin", "/sbin"] {
        let candidate = Path::new(root).join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

fn user_namespaces_enabled() -> bool {
    match std::fs::read_to_string("/proc/sys/user/max_user_namespaces") {
        Ok(value) => value.trim().parse::<u64>().unwrap_or(0) > 0,
        // The knob only exists when USER_NS is configured; a kernel
        // without it still supports the namespace.
        Err(_) => true,
    }
}

/// Does this `bwrap` support `--disable-userns`?
///
/// Without it a worker that already has code execution can call
/// `unshare(CLONE_NEWUSER)`, regain `CAP_SYS_ADMIN` in the fresh
/// namespace and mount over the policy's read-only binds — the classic
/// escape. bubblewrap grew the flag in 0.8, so an older one is a
/// missing facility rather than a smaller sandbox.
///
/// Probed once and cached: the answer cannot change while the process
/// runs, and an App launch must not pay for a `--help` every time.
fn bwrap_disables_userns(path: &Path) -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        Command::new(path)
            .arg("--help")
            .stdin(std::process::Stdio::null())
            .output()
            .map(|output| {
                let text = String::from_utf8_lossy(&output.stdout).into_owned()
                    + &String::from_utf8_lossy(&output.stderr);
                text.contains("--disable-userns")
            })
            .unwrap_or(false)
    })
}

fn seccomp_supported() -> bool {
    // `seccomp(SECCOMP_GET_ACTION_AVAIL)` with a null argument returns
    // EFAULT on a kernel that implements the call and ENOSYS on one
    // that does not, so it probes support without installing anything.
    const SECCOMP_GET_ACTION_AVAIL: libc::c_uint = 2;
    let result = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_GET_ACTION_AVAIL,
            0 as libc::c_uint,
            std::ptr::null::<libc::c_void>(),
        )
    };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOSYS)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/linux.rs"
    ));
}
