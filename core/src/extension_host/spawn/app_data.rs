//! Owner/App data stays in the existing partition. Only the private service
//! mount translates ownership to its leased execution identity.

use std::io::Read;

use super::*;

const MOUNT_ATTR_IDMAP: u64 = 0x0010_0000;
const MOUNT_ATTR_NOSUID: u64 = 0x0000_0002;
const MOUNT_ATTR_NODEV: u64 = 0x0000_0004;
const OPEN_TREE_CLONE: u32 = 1;
const MOVE_MOUNT_F_EMPTY_PATH: u32 = 0x0000_0004;
const MOVE_MOUNT_T_EMPTY_PATH: u32 = 0x0000_0040;
const NAMESPACE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_MOUNTINFO_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(super) struct Binding {
    source: PathBuf,
    owner_uid: u32,
    device: u64,
    inode: u64,
}

impl Binding {
    pub fn require_current(&self) -> Result<(), String> {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(self.owner_uid)?;
        let source = open_absolute_dir(&self.source)?;
        let metadata = fstat(source.as_raw_fd()).map_err(|error| error.to_string())?;
        if metadata.st_uid != self.owner_uid
            || metadata.st_dev != self.device
            || metadata.st_ino != self.inode
        {
            return Err("App persistent data directory identity changed".to_string());
        }
        reject_submounts(&self.source)
    }
}

pub(super) struct Prepared {
    pub view_root: PathBuf,
    pub destination: CString,
    pub binding: Binding,
    mount: OwnedFd,
    target_device: u64,
    target_inode: u64,
}

impl Prepared {
    pub fn configure_environment(&self, command: &mut std::process::Command) {
        command.env("COS_DATA_DIR", &self.view_root);
        command.env("COS_USER_DATA_DIR", &self.view_root);
    }

    pub fn install(&self) -> std::io::Result<()> {
        // Reopen in the child's new mount namespace: a pre-fork target fd
        // belongs to the parent's mount tree, even though the inode is shared.
        let how = OpenHow {
            flags: (libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
            mode: 0,
            resolve: RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
        };
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                libc::AT_FDCWD,
                self.destination.as_ptr(),
                std::ptr::addr_of!(how),
                std::mem::size_of::<OpenHow>(),
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let target = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        let metadata = fstat(target.as_raw_fd())?;
        if metadata.st_uid != 0
            || metadata.st_dev != self.target_device
            || metadata.st_ino != self.target_inode
        {
            return Err(std::io::Error::from_raw_os_error(libc::ESTALE));
        }
        if unsafe {
            libc::syscall(
                libc::SYS_move_mount,
                self.mount.as_raw_fd(),
                c"".as_ptr(),
                target.as_raw_fd(),
                c"".as_ptr(),
                MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

pub(super) fn prepare(
    owner: &WorkerIdentity,
    extension: &ExtensionIdentity,
    launch: &HostLaunchSpec,
    paths: &HostPaths,
) -> Result<Option<Prepared>, String> {
    let Some(app) = service_app(launch)? else {
        return Ok(None);
    };
    if unsafe { libc::geteuid() } != 0 || owner.uid == 0 || owner.gid == 0 {
        return Err("App persistent data binding requires Root and a non-root owner".to_string());
    }
    let context = crate::paths::RoutedPathContext::for_owner(owner.uid, owner.home.clone());
    let root = context.clone().scope_sync(crate::paths::user_data_dir);
    let source_path = context.scope_sync(|| {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(owner.uid)?;
        for path in [root.join("apps"), root.join("apps").join(app)] {
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && metadata.uid() == owner.uid => {}
                Ok(_) => {
                    return Err("App data root has a foreign or non-directory entry".to_string())
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("inspect App data root: {error}")),
            }
        }
        let source = crate::worker::derive::app_partition(&root, app)?;
        let parent = source.parent().ok_or("App data partition has no parent")?;
        let directory = open_absolute_dir(parent)?;
        let metadata = fstat(directory.as_raw_fd()).map_err(|error| error.to_string())?;
        if metadata.st_uid != owner.uid {
            return Err("App data parent belongs to another owner".to_string());
        }
        if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0 {
            return Err(format!(
                "protect owner App data: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(source)
    })?;
    let source = {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(owner.uid)?;
        open_absolute_dir(&source_path)?
    };
    let metadata = fstat(source.as_raw_fd()).map_err(|error| error.to_string())?;
    if metadata.st_uid != owner.uid || metadata.st_mode & 0o077 != 0 {
        return Err("App persistent data is not owner-private".to_string());
    }
    reject_submounts(&source_path)?;
    let namespace = user_namespace(owner.uid, extension.uid, owner.gid, extension.gid)?;
    let mount_fd = unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            source.as_raw_fd(),
            c"".as_ptr(),
            OPEN_TREE_CLONE | libc::O_CLOEXEC as u32 | libc::AT_EMPTY_PATH as u32,
        )
    };
    if mount_fd < 0 {
        return Err(format!(
            "clone App data mount: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mount = unsafe { OwnedFd::from_raw_fd(mount_fd as RawFd) };
    let attributes = MountAttr {
        attr_set: MOUNT_ATTR_IDMAP | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV,
        attr_clr: 0,
        propagation: 0,
        userns_fd: namespace.as_raw_fd() as u64,
    };
    if unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            mount.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            std::ptr::addr_of!(attributes),
            std::mem::size_of::<MountAttr>(),
        )
    } != 0
    {
        return Err(format!(
            "App data filesystem must support idmapped mounts: {}",
            std::io::Error::last_os_error()
        ));
    }
    let view = ensure_root_child_dir(paths.task_dir_fd(), c"app-data", 0o711)?;
    let apps = ensure_root_child_dir(view.as_raw_fd(), c"apps", 0o711)?;
    let app_name = CString::new(app).map_err(|_| "App data identity contains NUL")?;
    let target = ensure_root_child_dir(apps.as_raw_fd(), &app_name, 0o000)?;
    let target_metadata = fstat(target.as_raw_fd()).map_err(|error| error.to_string())?;
    let view_root = paths.dir.join("app-data");
    let destination = CString::new(view_root.join("apps").join(app).as_os_str().as_bytes())
        .map_err(|_| "App data view contains NUL")?;
    Ok(Some(Prepared {
        view_root,
        destination,
        binding: Binding {
            source: source_path,
            owner_uid: owner.uid,
            device: metadata.st_dev,
            inode: metadata.st_ino,
        },
        mount,
        target_device: target_metadata.st_dev,
        target_inode: target_metadata.st_ino,
    }))
}

fn service_app(launch: &HostLaunchSpec) -> Result<Option<&str>, String> {
    if launch.purpose != protocol::HostPurpose::AppService {
        return Ok(None);
    }
    let app = launch
        .app_id
        .as_deref()
        .ok_or("App service has no App identity")?;
    crate::worker::derive::validate_app_id(app)?;
    let package = launch
        .package
        .as_ref()
        .ok_or("App service has no package binding")?;
    if package.kind != crate::provenance::PackageKind::App || package.id != app {
        return Err("App service data identity differs from its package".to_string());
    }
    Ok(Some(app))
}

fn reject_submounts(source: &Path) -> Result<(), String> {
    let mut bytes = Vec::new();
    std::fs::File::open("/proc/self/mountinfo")
        .and_then(|file| file.take(MAX_MOUNTINFO_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("read App data mount topology: {error}"))?;
    check_mountinfo(source, &bytes)
}

fn check_mountinfo(source: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_MOUNTINFO_BYTES {
        return Err("App data mount topology is empty or exceeds its size limit".to_string());
    }
    let mut prefix = Vec::new();
    for byte in source.as_os_str().as_bytes() {
        match byte {
            b' ' => prefix.extend_from_slice(b"\\040"),
            b'\t' => prefix.extend_from_slice(b"\\011"),
            b'\n' => prefix.extend_from_slice(b"\\012"),
            b'\\' => prefix.extend_from_slice(b"\\134"),
            byte => prefix.push(*byte),
        }
    }
    prefix.push(b'/');
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let target = line
            .split(|byte| *byte == b' ')
            .nth(4)
            .ok_or("App data mount topology is malformed")?;
        if target.starts_with(&prefix) {
            return Err(
                "App private data cannot contain additional mounted filesystems".to_string(),
            );
        }
    }
    Ok(())
}

fn user_namespace(
    owner_uid: u32,
    execution_uid: u32,
    owner_gid: u32,
    execution_gid: u32,
) -> Result<OwnedFd, String> {
    let mut pipe = [-1; 2];
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(format!(
            "create App data namespace pipe: {}",
            std::io::Error::last_os_error()
        ));
    }
    let reader = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    let parent = unsafe { libc::getpid() };
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(format!(
            "create App data namespace: {}",
            std::io::Error::last_os_error()
        ));
    }
    if pid == 0 {
        unsafe {
            libc::close(reader.as_raw_fd());
            let error = if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                *libc::__errno_location()
            } else if libc::getppid() != parent {
                libc::EPIPE
            } else if libc::unshare(libc::CLONE_NEWUSER) != 0 {
                *libc::__errno_location()
            } else {
                0
            };
            if libc::write(
                writer.as_raw_fd(),
                std::ptr::addr_of!(error).cast(),
                std::mem::size_of::<i32>(),
            ) != 4
                || error != 0
            {
                libc::_exit(1);
            }
            loop {
                libc::pause();
            }
        }
    }
    drop(writer);
    let mut helper = NamespaceHelper { pid: Some(pid) };
    let result = (|| {
        let deadline = Instant::now() + NAMESPACE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("App data user namespace setup timed out".to_string());
            }
            let mut ready = libc::pollfd {
                fd: reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let status =
                unsafe { libc::poll(&mut ready, 1, remaining.as_millis().clamp(1, 3000) as i32) };
            if status < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("wait for App data namespace: {error}"));
            }
            if status > 0 {
                break;
            }
        }
        let mut error = [0_u8; 4];
        std::fs::File::from(reader)
            .read_exact(&mut error)
            .map_err(|error| format!("read App data namespace setup: {error}"))?;
        let error = i32::from_ne_bytes(error);
        if error != 0 {
            return Err(format!(
                "App data user namespace unavailable: {}",
                std::io::Error::from_raw_os_error(error)
            ));
        }
        let root = PathBuf::from(format!("/proc/{pid}"));
        std::fs::write(root.join("setgroups"), b"deny\n")
            .map_err(|error| format!("restrict App data namespace groups: {error}"))?;
        // Mount idmaps map the on-disk ID to its visible execution ID.
        std::fs::write(
            root.join("uid_map"),
            format!("{owner_uid} {execution_uid} 1\n"),
        )
        .map_err(|error| format!("map App data owner: {error}"))?;
        std::fs::write(
            root.join("gid_map"),
            format!("{owner_gid} {execution_gid} 1\n"),
        )
        .map_err(|error| format!("map App data group: {error}"))?;
        let path =
            CString::new(format!("/proc/{pid}/ns/user")).map_err(|_| "invalid namespace path")?;
        open_namespace(&path)
    })();
    let stopped = helper.stop();
    match (result, stopped) {
        (Ok(namespace), Ok(())) => Ok(namespace),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
    }
}

struct NamespaceHelper {
    pid: Option<libc::pid_t>,
}

impl NamespaceHelper {
    fn stop(&mut self) -> Result<(), String> {
        let Some(pid) = self.pid else {
            return Ok(());
        };
        if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
            return Err(format!(
                "stop App data namespace helper: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut status = 0;
        loop {
            let waited = unsafe { libc::waitpid(pid, std::ptr::addr_of_mut!(status), 0) };
            if waited == pid {
                self.pid = None;
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if waited < 0 && error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("reap App data namespace helper: {error}"));
        }
    }
}

impl Drop for NamespaceHelper {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::error!(%error, "App data namespace helper cleanup failed");
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/spawn/app_data.rs"
    ));
}
