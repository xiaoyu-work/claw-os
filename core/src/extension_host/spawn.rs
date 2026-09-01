//! Privilege drop and lifetime control for `claw-extension-host`.

use std::ffi::{CStr, CString};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::agentd::spawn::{ExecutionIsolation, WorkerIdentity};
use crate::extension_host::identity::ExtensionIdentity;

use super::protocol::{self, ExtensionBinding};

pub const HOST_BINARY_ENV: &str = "COS_EXTENSION_HOST_BIN";
pub const TASK_ENV: &str = "COS_EXTENSION_TASK_ID";
pub const TASK_SESSION_ENV: &str = "COS_EXTENSION_TASK_SESSION";
pub const HOST_SESSION_ENV: &str = "COS_EXTENSION_HOST_SESSION";
pub const WORKER_UID_ENV: &str = "COS_EXTENSION_WORKER_UID";
pub const WORKER_PID_ENV: &str = "COS_EXTENSION_WORKER_PID";
pub const WORKER_START_ENV: &str = "COS_EXTENSION_WORKER_START";
pub const LEASE_NONCE_ENV: &str = "COS_EXTENSION_LEASE_NONCE";
pub const LEASE_EXPIRES_ENV: &str = "COS_EXTENSION_LEASE_EXPIRES_MS";
pub const CONTROL_SOCKET_ENV: &str = "COS_EXTENSION_CONTROL_SOCKET";
pub const ENFORCE_GROUPS_ENV: &str = "COS_EXTENSION_ENFORCE_GROUPS";
pub const EXECUTION_GID_ENV: &str = "COS_EXTENSION_EXECUTION_GID";
pub const EXTENSION_UID_ENV: &str = "COS_EXTENSION_EXECUTION_UID";
pub const CGROUP_ROOT_ENV: &str = "CLAWD_EXTENSION_CGROUP_ROOT";

const HOST_PATH: &str = "/usr/local/bin/claw-extension-host";
const SAFE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const HOST_NOFILE_LIMIT: libc::rlim_t = 128;
const HOST_NPROC_LIMIT: libc::rlim_t = 512;
const HOST_ADDRESS_SPACE_LIMIT: libc::rlim_t = 2 * 1024 * 1024 * 1024;
const HOST_FILE_SIZE_LIMIT: libc::rlim_t = 256 * 1024 * 1024;
const CGROUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const REQUIRED_CONTROLLERS: [&str; 3] = ["cpu", "memory", "pids"];
const PRIVATE_TMP_PATHS: [&CStr; 4] = [c"/tmp", c"/var/tmp", c"/dev/shm", c"/run/lock"];
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
    "COS_SDK_PYTHON_DIR",
    "LANG",
    "LC_ALL",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "TZ",
];

const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;
const PATHS_ACTIVE: u8 = 0;
const PATHS_CLEANING: u8 = 1;
const PATHS_CLEANED: u8 = 2;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[repr(C)]
struct MountAttr {
    attr_set: u64,
    attr_clr: u64,
    propagation: u64,
    userns_fd: u64,
}

const MOUNT_ATTR_RDONLY: u64 = 0x0000_0001;
const AT_RECURSIVE: libc::c_int = 0x8000;

#[derive(Debug)]
struct HostPathHandles {
    owner_dir: OwnedFd,
    task_dir: OwnedFd,
    control_dir: OwnedFd,
    task_name: CString,
    activated: AtomicBool,
    cleanup_state: AtomicU8,
}

#[derive(Debug, Clone)]
pub struct HostPaths {
    pub dir: PathBuf,
    pub control_dir: PathBuf,
    pub control_socket: PathBuf,
    pub broker_socket: PathBuf,
    handles: Arc<HostPathHandles>,
}

impl HostPaths {
    pub fn create(identity: &WorkerIdentity) -> Result<Self, String> {
        Self::create_named(identity, &Self::new_task_name())
    }

    pub(crate) fn new_task_name() -> String {
        uuid::Uuid::new_v4().simple().to_string()
    }

    pub(crate) fn create_named(identity: &WorkerIdentity, task_name: &str) -> Result<Self, String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("extension-host paths require a root broker".to_string());
        }
        if task_name.len() != 32
            || !task_name
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("extension task directory name is invalid".to_string());
        }
        let base = crate::paths::runtime_dir().join("extension-hosts");
        let owner_root = base.join(identity.uid.to_string());
        let runtime = open_absolute_dir(&crate::paths::runtime_dir())?;
        require_root_dir(&runtime, "extension runtime root")?;
        let base_dir = ensure_root_child_dir(runtime.as_raw_fd(), c"extension-hosts", 0o711)?;
        let owner_name = CString::new(identity.uid.to_string())
            .map_err(|_| "extension owner directory name contains NUL".to_string())?;
        let (owner_dir, legacy_writable) = ensure_owner_root(base_dir.as_raw_fd(), &owner_name)?;
        if legacy_writable {
            let mount_id = fd_mount_id(owner_dir.as_raw_fd())
                .map_err(|error| format!("inspect legacy extension mount: {error}"))?;
            remove_dir_contents(owner_dir.as_raw_fd(), mount_id)
                .map_err(|error| format!("clean legacy task-writable extension tree: {error}"))?;
        }

        let task_name = CString::new(task_name)
            .map_err(|_| "extension task directory name contains NUL".to_string())?;
        mkdirat_new(owner_dir.as_raw_fd(), &task_name, 0o700)
            .map_err(|error| format!("create extension task directory: {error}"))?;
        let setup = (|| {
            let task_dir = openat2_dir(owner_dir.as_raw_fd(), &task_name)
                .map_err(|error| format!("pin extension task directory: {error}"))?;
            set_dir_identity(&task_dir, 0, 0, 0o711)
                .map_err(|error| format!("protect extension task directory: {error}"))?;

            mkdirat_new(task_dir.as_raw_fd(), c"control", 0o700)
                .map_err(|error| format!("create extension control directory: {error}"))?;
            let control_dir = openat2_dir(task_dir.as_raw_fd(), c"control")
                .map_err(|error| format!("pin extension control directory: {error}"))?;
            set_dir_identity(&control_dir, 0, 0, 0o700)
                .map_err(|error| format!("protect extension control directory: {error}"))?;
            Ok::<_, String>((task_dir, control_dir))
        })();
        let (task_dir, control_dir) = match setup {
            Ok(setup) => setup,
            Err(error) => {
                if let Ok(task_dir) = openat2_dir(owner_dir.as_raw_fd(), &task_name) {
                    if let Ok(mount_id) = fd_mount_id(task_dir.as_raw_fd()) {
                        let _ = remove_dir_contents(task_dir.as_raw_fd(), mount_id);
                    }
                }
                let _ = unlinkat_if_present(owner_dir.as_raw_fd(), &task_name, true);
                return Err(error);
            }
        };

        let dir = owner_root.join(task_name.to_string_lossy().as_ref());
        let control_dir_path = dir.join("control");
        Ok(Self {
            control_socket: control_dir_path.join("control.sock"),
            broker_socket: dir.join("broker.sock"),
            control_dir: control_dir_path,
            dir,
            handles: Arc::new(HostPathHandles {
                owner_dir,
                task_dir,
                control_dir,
                task_name,
                activated: AtomicBool::new(false),
                cleanup_state: AtomicU8::new(PATHS_ACTIVE),
            }),
        })
    }

    pub(crate) fn task_dir_fd(&self) -> RawFd {
        self.handles.task_dir.as_raw_fd()
    }

    #[doc(hidden)]
    pub fn task_name(&self) -> &str {
        self.handles
            .task_name
            .to_str()
            .expect("UUID task directory names are ASCII")
    }

    pub(crate) fn activate(&self, uid: u32, gid: u32) -> Result<(), String> {
        if self.handles.activated.swap(true, Ordering::SeqCst) {
            return Err("extension-host paths were already activated".to_string());
        }
        set_dir_identity(&self.handles.control_dir, uid, gid, 0o710)
            .map_err(|error| format!("activate extension control directory: {error}"))
    }

    pub fn cleanup(&self) -> Result<(), String> {
        match self.handles.cleanup_state.compare_exchange(
            PATHS_ACTIVE,
            PATHS_CLEANING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => {}
            Err(PATHS_CLEANED) => return Ok(()),
            Err(_) => return Err("extension-host paths are already being cleaned".to_string()),
        }
        let cleanup = cleanup_task_directory(
            self.handles.owner_dir.as_raw_fd(),
            self.handles.task_dir.as_raw_fd(),
            &self.handles.task_name,
        )
        .map_err(|error| {
            format!(
                "clean extension-host runtime paths {}: {error}",
                self.dir.display()
            )
        });
        self.handles.cleanup_state.store(
            if cleanup.is_ok() {
                PATHS_CLEANED
            } else {
                PATHS_ACTIVE
            },
            Ordering::SeqCst,
        );
        cleanup
    }

    pub(crate) fn recover(owner_uid: u32, task_name: &str) -> Result<(), String> {
        if task_name.len() != 32
            || !task_name
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("quarantined extension task name is invalid".to_string());
        }
        let runtime = open_absolute_dir(&crate::paths::runtime_dir())?;
        require_root_dir(&runtime, "extension runtime root")?;
        let Some(base_dir) = openat2_dir_if_present(runtime.as_raw_fd(), c"extension-hosts")?
        else {
            return Ok(());
        };
        require_root_dir(&base_dir, "extension-host runtime directory")?;
        let owner_name = CString::new(owner_uid.to_string())
            .map_err(|_| "extension owner directory name contains NUL".to_string())?;
        let Some(owner_dir) = openat2_dir_if_present(base_dir.as_raw_fd(), &owner_name)? else {
            return Ok(());
        };
        require_root_dir(&owner_dir, "extension owner directory")?;
        let task_name = CString::new(task_name)
            .map_err(|_| "extension task directory name contains NUL".to_string())?;
        let Some(task_dir) = openat2_dir_if_present(owner_dir.as_raw_fd(), &task_name)? else {
            return Ok(());
        };
        require_root_dir(&task_dir, "extension task directory")?;
        cleanup_task_directory(owner_dir.as_raw_fd(), task_dir.as_raw_fd(), &task_name)
            .map_err(|error| format!("recover quarantined extension task: {error}"))
    }
}

fn open_absolute_dir(path: &Path) -> Result<OwnedFd, String> {
    if !path.is_absolute() {
        return Err(format!(
            "extension runtime root is not absolute: {}",
            path.display()
        ));
    }
    let relative = path
        .strip_prefix("/")
        .map_err(|_| "extension runtime root has no filesystem root".to_string())?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "extension runtime root has unsafe components: {}",
            path.display()
        ));
    }
    let root =
        open_path_dir(Path::new("/")).map_err(|error| format!("open filesystem root: {error}"))?;
    let relative = CString::new(relative.as_os_str().as_bytes())
        .map_err(|_| "extension runtime root contains NUL".to_string())?;
    openat2_dir(root.as_raw_fd(), &relative)
        .map_err(|error| format!("pin extension runtime root: {error}"))
}

fn open_path_dir(path: &Path) -> std::io::Result<OwnedFd> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn openat2_dir(parent: RawFd, name: &CStr) -> std::io::Result<OwnedFd> {
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent,
            name.as_ptr(),
            std::ptr::addr_of!(how),
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
    }
}

fn openat2_dir_if_present(parent: RawFd, name: &CStr) -> Result<Option<OwnedFd>, String> {
    match openat2_dir(parent, name) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(format!("open extension directory: {error}")),
    }
}

fn mkdirat_new(parent: RawFd, name: &CStr, mode: libc::mode_t) -> std::io::Result<()> {
    if unsafe { libc::mkdirat(parent, name.as_ptr(), mode) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn ensure_root_child_dir(
    parent: RawFd,
    name: &CStr,
    mode: libc::mode_t,
) -> Result<OwnedFd, String> {
    match mkdirat_new(parent, name, mode) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {}
        Err(error) => return Err(format!("create directory: {error}")),
    }
    let dir = openat2_dir(parent, name).map_err(|error| format!("open directory: {error}"))?;
    set_dir_identity(&dir, 0, 0, mode).map_err(|error| format!("secure directory: {error}"))?;
    Ok(dir)
}

fn ensure_owner_root(parent: RawFd, name: &CStr) -> Result<(OwnedFd, bool), String> {
    match mkdirat_new(parent, name, 0o700) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {}
        Err(error) => return Err(format!("create owner extension directory: {error}")),
    }
    let dir = openat2_dir(parent, name)
        .map_err(|error| format!("pin owner extension directory: {error}"))?;
    let before = fstat(dir.as_raw_fd())
        .map_err(|error| format!("inspect owner extension directory: {error}"))?;
    if before.st_mode & libc::S_IFMT != libc::S_IFDIR || before.st_nlink < 2 {
        return Err("owner extension path is not a normal directory".to_string());
    }
    let legacy_writable = before.st_uid != 0 || before.st_gid != 0 || before.st_mode & 0o022 != 0;
    set_dir_identity(&dir, 0, 0, 0o711)
        .map_err(|error| format!("secure owner extension directory: {error}"))?;
    Ok((dir, legacy_writable))
}

fn require_root_dir(dir: &OwnedFd, label: &str) -> Result<(), String> {
    let stat = fstat(dir.as_raw_fd()).map_err(|error| format!("inspect {label}: {error}"))?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_uid != 0
        || stat.st_gid != 0
        || stat.st_mode & 0o022 != 0
    {
        return Err(format!(
            "{label} must be a root-owned, non-writable directory"
        ));
    }
    Ok(())
}

fn set_dir_identity(dir: &OwnedFd, uid: u32, gid: u32, mode: libc::mode_t) -> std::io::Result<()> {
    if unsafe { libc::fchown(dir.as_raw_fd(), uid, gid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fchmod(dir.as_raw_fd(), mode) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stat = fstat(dir.as_raw_fd())?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_uid != uid
        || stat.st_gid != gid
        || stat.st_mode & 0o7777 != mode
    {
        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
    }
    Ok(())
}

fn fstat(fd: RawFd) -> std::io::Result<libc::stat> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, std::ptr::addr_of_mut!(stat)) } == 0 {
        Ok(stat)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn cleanup_task_directory(
    owner_dir: RawFd,
    task_dir: RawFd,
    task_name: &CStr,
) -> std::io::Result<()> {
    let mount_id = fd_mount_id(task_dir)?;
    remove_dir_contents(task_dir, mount_id)?;
    unlinkat_if_present(owner_dir, task_name, true)?;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            owner_dir,
            task_name.as_ptr(),
            std::ptr::addr_of_mut!(stat),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Err(std::io::Error::from_raw_os_error(libc::EEXIST));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(())
    } else {
        Err(error)
    }
}

fn remove_dir_contents(dir: RawFd, root_mount_id: u64) -> std::io::Result<()> {
    let duplicate = openat2_dir(dir, c".")?;
    let stream = unsafe { libc::fdopendir(std::os::fd::IntoRawFd::into_raw_fd(duplicate)) };
    if stream.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    loop {
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                dir,
                name.as_ptr(),
                std::ptr::addr_of_mut!(stat),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::closedir(stream);
            }
            return Err(error);
        }
        if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            let child = match openat2_dir(dir, name) {
                Ok(child) => child,
                Err(error) => {
                    unsafe {
                        libc::closedir(stream);
                    }
                    return Err(error);
                }
            };
            let child_mount_id = match fd_mount_id(child.as_raw_fd()) {
                Ok(mount_id) => mount_id,
                Err(error) => {
                    unsafe {
                        libc::closedir(stream);
                    }
                    return Err(error);
                }
            };
            if child_mount_id != root_mount_id {
                unsafe {
                    libc::closedir(stream);
                }
                return Err(std::io::Error::from_raw_os_error(libc::EXDEV));
            }
            if let Err(error) = remove_dir_contents(child.as_raw_fd(), root_mount_id) {
                unsafe {
                    libc::closedir(stream);
                }
                return Err(error);
            }
            if let Err(error) = unlinkat_if_present(dir, name, true) {
                unsafe {
                    libc::closedir(stream);
                }
                return Err(error);
            }
        } else {
            if let Err(error) = unlinkat_if_present(dir, name, false) {
                unsafe {
                    libc::closedir(stream);
                }
                return Err(error);
            }
        }
    }
    if unsafe { libc::closedir(stream) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn fd_mount_id(fd: RawFd) -> std::io::Result<u64> {
    let content = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}"))?;
    content
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EIO))
}

fn unlinkat_if_present(parent: RawFd, name: &CStr, directory: bool) -> std::io::Result<()> {
    let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
    if unsafe { libc::unlinkat(parent, name.as_ptr(), flags) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(())
    } else {
        Err(error)
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
    private_mounts: PrivateMountNamespace,
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
    owner: &WorkerIdentity,
    extension: &ExtensionIdentity,
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
    if extension.uid == 0
        || extension.uid == owner.uid
        || extension.gid != isolation.execution_gid()
    {
        let _ = paths.cleanup();
        return Err("extension execution identity is not isolated from the task owner".to_string());
    }
    if let Err(error) = validate_private_mount_targets() {
        let _ = paths.cleanup();
        return Err(error);
    }
    let seccomp = match process_isolation_filter() {
        Ok(filter) => Arc::new(filter),
        Err(error) => {
            return Err(combine_cleanup_error(error, Ok(()), paths.cleanup()));
        }
    };
    let mut cgroup = match ResourceGroup::create(containment, task_id) {
        Ok(cgroup) => cgroup,
        Err(error) => {
            return Err(combine_cleanup_error(error, Ok(()), paths.cleanup()));
        }
    };
    let binary = host_binary_path();
    if !binary.exists() {
        let cleanup = cgroup.cleanup_blocking();
        return Err(combine_cleanup_error(
            format!(
                "extension host binary is not installed at {}",
                binary.display()
            ),
            cleanup,
            paths.cleanup(),
        ));
    }
    if crate::agentd::spawn::broker_is_root() {
        if let Err(error) = crate::agentd::spawn::validate_root_owned_executable(&binary) {
            let cleanup = cgroup.cleanup_blocking();
            return Err(combine_cleanup_error(error, cleanup, paths.cleanup()));
        }
    }
    let enforce_groups = crate::agentd::spawn::broker_is_root();

    let mut command = std::process::Command::new(&binary);
    command.arg("--host");
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.current_dir(&paths.control_dir);
    command.env_clear();
    command.env("HOME", &paths.control_dir);
    command.env("XDG_RUNTIME_DIR", paths.control_dir.join("runtime"));
    command.env("USER", &extension.username);
    command.env("LOGNAME", &extension.username);
    command.env("PATH", SAFE_PATH);
    command.env("SHELL", "/bin/sh");
    command.env("TMPDIR", "/tmp");
    command.env("TMP", "/tmp");
    command.env("TEMP", "/tmp");
    command.env("COS_PERMS_MODE", "strict");
    command.env("COS_EXTENSION_CHILD_ISOLATION", "1");
    command.env(
        "COS_PROC_DATA_DIR",
        std::env::var_os("COS_PROC_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/run/cos/caps").join(owner.uid.to_string())),
    );
    command.env("COS_DATA_DIR", paths.control_dir.join("data"));
    command.env("COS_CACHE_DIR", paths.control_dir.join("cache"));
    command.env("COS_LOG_DIR", paths.control_dir.join("log"));
    command.env(TASK_ENV, task_id);
    command.env(WORKER_UID_ENV, owner.uid.to_string());
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
    command.env(EXTENSION_UID_ENV, extension.uid.to_string());
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
    let uid = extension.uid;
    let gid = isolation.execution_gid();
    let child_isolation = isolation.clone();
    let cgroup_procs_fd = cgroup.procs_fd();
    let writable_task_path = CString::new(paths.dir.as_os_str().as_bytes())
        .map_err(|_| "extension task path contains NUL".to_string())?;
    let try_namespaces = std::env::var("CLAWD_EXTENSION_HOST_NAMESPACES")
        .map(|value| !matches!(value.trim(), "0" | "off" | "false" | "no"))
        .unwrap_or(true);
    unsafe {
        command.pre_exec(move || {
            attach_current_process(cgroup_procs_fd)?;
            libc::umask(0o077);
            crate::agentd::spawn::mark_inherited_descriptors_cloexec(3);
            setup_private_mount_namespace(&writable_task_path)?;
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
            install_process_isolation_seccomp(seccomp.as_slice())?;
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
            combine_cleanup_error(
                format!("spawn {}: {error}", binary.display()),
                cleanup,
                paths.cleanup(),
            )
        })?;
    let Some(pid) = child.id() else {
        let _ = child.start_kill();
        let cleanup = cgroup.cleanup_blocking();
        return Err(combine_cleanup_error(
            "extension host exited before it could be identified".to_string(),
            cleanup,
            paths.cleanup(),
        ));
    };
    if let Err(error) = cgroup.verify_member(pid) {
        let _ = child.start_kill();
        let cleanup = cgroup.cleanup_blocking();
        return Err(combine_cleanup_error(
            format!("extension host containment verification failed: {error}"),
            cleanup,
            paths.cleanup(),
        ));
    }
    let mut private_mounts = match PrivateMountNamespace::capture(pid, &paths.dir) {
        Ok(namespace) => namespace,
        Err(error) => {
            let _ = child.start_kill();
            let cleanup = cgroup.cleanup_blocking();
            return Err(combine_cleanup_error(
                format!("capture extension mount namespace: {error}"),
                cleanup,
                paths.cleanup(),
            ));
        }
    };
    let start_time_ticks = crate::proc::read_start_time_ticks_pub(pid);
    let binding = ExtensionBinding {
        protocol: protocol::PROTOCOL_VERSION,
        task_id: task_id.to_string(),
        session_id: task_session_id.map(ToOwned::to_owned),
        owner_uid: owner.uid,
        extension_uid: extension.uid,
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
        let mounts = if cleanup.is_ok() {
            private_mounts.cleanup()
        } else {
            Err("private mounts retained because containment cleanup failed".to_string())
        };
        let paths_cleanup = if mounts.is_ok() {
            paths.cleanup()
        } else {
            Err("task state retained because private mounts remain".to_string())
        };
        let mut error = combine_cleanup_error(error, cleanup, paths_cleanup);
        if let Err(mounts) = mounts {
            error.push_str(&format!("; private-mount cleanup failed: {mounts}"));
        }
        return Err(error);
    }
    Ok(SpawnedExtensionHost {
        child,
        pid,
        start_time_ticks,
        binding,
        paths,
        cgroup,
        private_mounts,
    })
}

impl SpawnedExtensionHost {
    #[doc(hidden)]
    pub fn cleanup_private_mounts(&mut self) -> Result<(), String> {
        self.private_mounts.cleanup()
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn mount_private_tmp_test_child(&self) -> Result<(), String> {
        self.private_mounts.run_test_helper(true)
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn unmount_private_tmp_test_child(&self) -> Result<(), String> {
        self.private_mounts.run_test_helper(false)
    }
}

fn validate_private_mount_targets() -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    for path in PRIVATE_TMP_PATHS {
        let path = Path::new(
            path.to_str()
                .map_err(|_| "private tmp mount path is not UTF-8".to_string())?,
        );
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("inspect private tmp mount {}: {error}", path.display()))?;
        let mode = metadata.mode() & 0o7777;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != 0
            || metadata.gid() != 0
            || mode & 0o022 != 0 && mode & 0o1000 == 0
        {
            return Err(format!(
                "private tmp mount target has unsafe identity: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn setup_private_mount_namespace(writable_task_path: &CStr) -> std::io::Result<()> {
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe {
        libc::mount(
            writable_task_path.as_ptr(),
            writable_task_path.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    set_mount_read_only(c"/", true)?;
    set_mount_read_only(writable_task_path, false)?;
    for path in PRIVATE_TMP_PATHS {
        if unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                path.as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c"mode=1777,size=67108864,nr_inodes=16384".as_ptr().cast(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(path.as_ptr(), std::ptr::addr_of_mut!(stat)) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        const TMPFS_MAGIC: libc::c_long = 0x0102_1994;
        if stat.f_type as libc::c_long != TMPFS_MAGIC {
            return Err(std::io::Error::from_raw_os_error(libc::ENODEV));
        }
    }
    Ok(())
}

fn set_mount_read_only(path: &CStr, read_only: bool) -> std::io::Result<()> {
    let attributes = MountAttr {
        attr_set: if read_only { MOUNT_ATTR_RDONLY } else { 0 },
        attr_clr: if read_only { 0 } else { MOUNT_ATTR_RDONLY },
        propagation: 0,
        userns_fd: 0,
    };
    if unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            libc::AT_FDCWD,
            path.as_ptr(),
            AT_RECURSIVE,
            std::ptr::addr_of!(attributes),
            std::mem::size_of::<MountAttr>(),
        )
    } != 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[derive(Debug)]
struct PrivateMountNamespace {
    fd: OwnedFd,
    task_path: CString,
    active: bool,
}

impl PrivateMountNamespace {
    fn capture(pid: u32, task_path: &Path) -> Result<Self, String> {
        let path = CString::new(format!("/proc/{pid}/ns/mnt"))
            .map_err(|_| "extension mount namespace path contains NUL".to_string())?;
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let own = open_namespace(c"/proc/self/ns/mnt")?;
        let captured = fstat(fd.as_raw_fd())
            .map_err(|error| format!("inspect extension mount namespace: {error}"))?;
        let broker = fstat(own.as_raw_fd())
            .map_err(|error| format!("inspect broker mount namespace: {error}"))?;
        if captured.st_dev == broker.st_dev && captured.st_ino == broker.st_ino {
            return Err("extension host did not enter a private mount namespace".to_string());
        }
        let task_path = CString::new(task_path.as_os_str().as_bytes())
            .map_err(|_| "extension task mount path contains NUL".to_string())?;
        Ok(Self {
            fd,
            task_path,
            active: true,
        })
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(format!(
                "fork private-mount cleanup helper: {}",
                std::io::Error::last_os_error()
            ));
        }
        if pid == 0 {
            if unsafe { libc::setns(self.fd.as_raw_fd(), libc::CLONE_NEWNS) } != 0 {
                unsafe { libc::_exit(100) };
            }
            for (index, path) in PRIVATE_TMP_PATHS.iter().rev().enumerate() {
                if unsafe { libc::umount2(path.as_ptr(), 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL)
                {
                    unsafe { libc::_exit(101 + index as i32) };
                }
            }
            if unsafe { libc::umount2(self.task_path.as_ptr(), 0) } != 0 {
                unsafe { libc::_exit(110) };
            }
            unsafe { libc::_exit(0) };
        }
        let mut status = 0;
        loop {
            let waited = unsafe { libc::waitpid(pid, std::ptr::addr_of_mut!(status), 0) };
            if waited == pid {
                break;
            }
            if waited < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(format!(
                "wait for private-mount cleanup helper: {}",
                std::io::Error::last_os_error()
            ));
        }
        if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
            return Err(format!(
                "private-mount cleanup helper failed with status {status}"
            ));
        }
        self.active = false;
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn run_test_helper(&self, mount_child: bool) -> Result<(), String> {
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if pid == 0 {
            if unsafe { libc::setns(self.fd.as_raw_fd(), libc::CLONE_NEWNS) } != 0 {
                unsafe { libc::_exit(120) };
            }
            if mount_child {
                if unsafe { libc::mkdir(c"/tmp/cos-nested-mount".as_ptr(), 0o700) } != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                {
                    unsafe { libc::_exit(121) };
                }
                if unsafe {
                    libc::mount(
                        c"tmpfs".as_ptr(),
                        c"/tmp/cos-nested-mount".as_ptr(),
                        c"tmpfs".as_ptr(),
                        libc::MS_NOSUID | libc::MS_NODEV,
                        c"mode=0700,size=4096,nr_inodes=8".as_ptr().cast(),
                    )
                } != 0
                {
                    unsafe { libc::_exit(122) };
                }
            } else if unsafe { libc::umount2(c"/tmp/cos-nested-mount".as_ptr(), 0) } != 0 {
                unsafe { libc::_exit(123) };
            }
            unsafe { libc::_exit(0) };
        }
        let mut status = 0;
        loop {
            let waited = unsafe { libc::waitpid(pid, std::ptr::addr_of_mut!(status), 0) };
            if waited == pid {
                break;
            }
            if waited < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(std::io::Error::last_os_error().to_string());
        }
        if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
            Ok(())
        } else {
            Err(format!(
                "private-mount test helper failed with status {status}"
            ))
        }
    }
}

fn open_namespace(path: &CStr) -> Result<OwnedFd, String> {
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn combine_cleanup_error(
    error: String,
    containment: Result<(), String>,
    paths: Result<(), String>,
) -> String {
    let mut errors = vec![error];
    if let Err(containment) = containment {
        errors.push(format!("containment cleanup failed: {containment}"));
    }
    if let Err(paths) = paths {
        errors.push(format!("task-state cleanup failed: {paths}"));
    }
    errors.join("; ")
}

fn apply_resource_limits() -> std::io::Result<()> {
    set_limit(libc::RLIMIT_CORE, 0)?;
    set_limit(libc::RLIMIT_NOFILE, HOST_NOFILE_LIMIT)?;
    set_limit(libc::RLIMIT_NPROC, HOST_NPROC_LIMIT)?;
    set_limit(libc::RLIMIT_AS, HOST_ADDRESS_SPACE_LIMIT)?;
    set_limit(libc::RLIMIT_FSIZE, HOST_FILE_SIZE_LIMIT)
}

fn process_isolation_filter() -> Result<Vec<libc::sock_filter>, String> {
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xc000_00b7;

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    return Err("extension seccomp filter is unsupported on this architecture".to_string());

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let denied = [
            libc::SYS_ptrace as u32,
            libc::SYS_process_vm_readv as u32,
            libc::SYS_process_vm_writev as u32,
            libc::SYS_kcmp as u32,
            libc::SYS_pidfd_getfd as u32,
        ];
        let mut filter = Vec::with_capacity(5 + denied.len() * 4);
        filter.push(libc::sock_filter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: 4,
        });
        filter.push(libc::sock_filter {
            code: BPF_JMP_JEQ_K,
            jt: 1,
            jf: 0,
            k: AUDIT_ARCH,
        });
        filter.push(libc::sock_filter {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_KILL_PROCESS,
        });
        filter.push(libc::sock_filter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: 0,
        });
        for syscall in denied {
            filter.push(libc::sock_filter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 1,
                k: syscall,
            });
            filter.push(libc::sock_filter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
            });
            #[cfg(target_arch = "x86_64")]
            {
                filter.push(libc::sock_filter {
                    code: BPF_JMP_JEQ_K,
                    jt: 0,
                    jf: 1,
                    k: syscall | 0x4000_0000,
                });
                filter.push(libc::sock_filter {
                    code: BPF_RET_K,
                    jt: 0,
                    jf: 0,
                    k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
                });
            }
        }
        filter.push(libc::sock_filter {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        });
        Ok(filter)
    }
}

fn install_process_isolation_seccomp(filter: &[libc::sock_filter]) -> std::io::Result<()> {
    let len =
        u16::try_from(filter.len()).map_err(|_| crate::agentd::spawn::raw_error(libc::E2BIG))?;
    let program = libc::sock_fprog {
        len,
        filter: filter.as_ptr().cast_mut(),
    };
    if unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            std::ptr::addr_of!(program),
        )
    } != 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
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
