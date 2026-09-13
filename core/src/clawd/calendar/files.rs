use std::ffi::CString;
use std::fs::{self, File};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path};

const FILES: [&str; 4] = [
    "events.db",
    "events.db-wal",
    "events.db-shm",
    "events.db-journal",
];
const AT_RECURSIVE: i32 = 0x8000;
const MOUNT_ATTR_RDONLY: u64 = 1;
const MOUNT_ATTR_NOSUID: u64 = 2;
const MOUNT_ATTR_NODEV: u64 = 4;
const MOUNT_ATTR_NOEXEC: u64 = 8;
const MOUNT_ATTR_NOSYMFOLLOW: u64 = 0x0020_0000;

pub(super) struct Database {
    directory: File,
}

impl Database {
    pub fn open(root: &Path, owner: u32) -> Result<Option<Self>, String> {
        if !root.is_absolute()
            || root
                .components()
                .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
        {
            return Err("Calendar owner data root is not an absolute normalized path".into());
        }
        let _owner = super::super::client_identity::FsIdentityGuard::enter(owner)?;
        let mut directory = File::open("/").map_err(|error| error.to_string())?;
        for component in root.join("apps/calendar/calendar").components() {
            if let Component::Normal(name) = component {
                directory = match super::super::filesystem::open_at(
                    &directory,
                    name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                    0,
                ) {
                    Ok(directory) => directory,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(format!("open Calendar data directory: {error}")),
                };
            }
        }
        if directory
            .metadata()
            .map_err(|error| error.to_string())?
            .uid()
            != owner
        {
            return Err("Calendar data directory belongs to another owner".into());
        }
        for name in FILES {
            let file = match super::super::filesystem::open_at(
                &directory,
                std::ffi::OsStr::new(name),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0,
            ) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && name == FILES[0] => {
                    return Ok(None)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("open Calendar database file: {error}")),
            };
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.uid() != owner || metadata.nlink() != 1 {
                return Err(
                    "Calendar data must be regular, single-link files owned by its user".into(),
                );
            }
        }
        Ok(Some(Self { directory }))
    }
}

pub(super) struct QueryView {
    directory: tempfile::TempDir,
    mount: OwnedFd,
}

impl QueryView {
    pub fn prepare(data: Database) -> Result<Self, String> {
        for path in ["/run", "/run/cos"] {
            let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
            if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
                return Err("Calendar runtime parent is not protected by Root".into());
            }
        }
        let root = Path::new("/run/cos/calendar-queries");
        match fs::DirBuilder::new().mode(0o711).create(root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
        let metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err("Calendar query runtime is not protected by Root".into());
        }
        let directory = tempfile::Builder::new()
            .prefix("query-")
            .tempdir_in(root)
            .map_err(|error| error.to_string())?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
        let mount = unsafe {
            libc::syscall(
                libc::SYS_open_tree,
                data.directory.as_raw_fd(),
                c"".as_ptr(),
                1_u32 | libc::O_CLOEXEC as u32 | libc::AT_EMPTY_PATH as u32 | AT_RECURSIVE as u32,
            )
        };
        if mount < 0 {
            return Err(format!(
                "pin Calendar data mount: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mount = unsafe { OwnedFd::from_raw_fd(mount as RawFd) };
        #[repr(C)]
        struct Attributes {
            set: u64,
            clear: u64,
            propagation: u64,
            userns: u64,
        }
        let attributes = Attributes {
            set: MOUNT_ATTR_RDONLY
                | MOUNT_ATTR_NOSUID
                | MOUNT_ATTR_NODEV
                | MOUNT_ATTR_NOEXEC
                | MOUNT_ATTR_NOSYMFOLLOW,
            clear: 0,
            propagation: 0,
            userns: 0,
        };
        if unsafe {
            libc::syscall(
                libc::SYS_mount_setattr,
                mount.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH | AT_RECURSIVE,
                std::ptr::addr_of!(attributes),
                std::mem::size_of::<Attributes>(),
            )
        } != 0
        {
            return Err(format!(
                "protect Calendar data mount: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self { directory, mount })
    }

    pub fn mount(&self) -> RawFd {
        self.mount.as_raw_fd()
    }

    pub fn directory(&self) -> CString {
        CString::new(self.directory.path().as_os_str().as_bytes()).expect("runtime path")
    }
}

pub(super) fn install(mount: RawFd, directory: &CString) -> std::io::Result<()> {
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0
        || unsafe {
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
    let fd = unsafe {
        libc::open(
            directory.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let target = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(target.as_raw_fd(), &mut metadata) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if metadata.st_uid != 0 {
        return Err(std::io::Error::from_raw_os_error(libc::ESTALE));
    }
    // A live directory keeps SQLite's own WAL creation/checkpoint locking
    // intact. NOSYMFOLLOW prevents a replaced sidecar from escaping this view.
    if unsafe {
        libc::syscall(
            libc::SYS_move_mount,
            mount,
            c"".as_ptr(),
            target.as_raw_fd(),
            c"".as_ptr(),
            0x4 | 0x40,
        )
    } != 0
        || unsafe { libc::chdir(directory.as_ptr()) } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
