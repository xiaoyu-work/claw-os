use std::fs;
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub fn set_private_umask() {
    #[cfg(unix)]
    unsafe {
        libc::umask(0o077);
    }
}

pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    set_dir_mode(path)
}

pub fn set_private_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub fn harden_clawd_state() -> io::Result<()> {
    let data = crate::paths::data_dir();
    let logs = crate::paths::log_dir();

    validate_clawd_root(&data)?;
    validate_clawd_root(&logs)?;
    ensure_private_dir(&data)?;
    ensure_private_dir(&logs)?;

    for root in [
        data.join("agent"),
        data.join("users"),
        data.join("approvals"),
        data.join("sessions"),
        data.join("proc"),
        data.join("clawd"),
    ] {
        if root.exists() {
            harden_private_tree(&root)?;
        }
    }
    harden_private_tree(&logs)
}

pub fn ensure_routed_caps_dir(path: &Path, uid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        let expected = Path::new("/run/cos/caps").join(uid.to_string());
        if path != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "unexpected routed capability path {}; expected {}",
                    path.display(),
                    expected.display()
                ),
            ));
        }
        let euid = unsafe { libc::geteuid() as u32 };
        if euid != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("cannot prepare routed caps storage as uid {euid}"),
            ));
        }
        let gid = primary_gid(uid)?;
        let runtime_root = Path::new("/run/cos");
        reject_symlink(runtime_root)?;
        fs::create_dir_all(runtime_root)?;
        fs::set_permissions(runtime_root, fs::Permissions::from_mode(0o751))?;
        let caps_root = Path::new("/run/cos/caps");
        reject_symlink(caps_root)?;
        fs::create_dir_all(caps_root)?;
        fs::set_permissions(caps_root, fs::Permissions::from_mode(0o711))?;
        reject_symlink(path)?;
        fs::create_dir_all(path)?;
        chown(path, 0, gid)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o750))?;
        let proc_dir = path.join("proc");
        reject_symlink(&proc_dir)?;
        fs::create_dir_all(&proc_dir)?;
        chown(&proc_dir, 0, gid)?;
        fs::set_permissions(&proc_dir, fs::Permissions::from_mode(0o750))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = uid;
        ensure_private_dir(path)
    }
}

pub fn set_group_readable_file(path: &Path, uid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        let euid = unsafe { libc::geteuid() as u32 };
        if euid != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("cannot secure routed caps registry as uid {euid}"),
            ));
        }
        let gid = primary_gid(uid)?;
        chown(path, 0, gid)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o640))
    }
    #[cfg(not(unix))]
    {
        let _ = uid;
        set_private_file(path)
    }
}

fn harden_private_tree(root: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing symlink inside private state: {}", root.display()),
        ));
    }
    if metadata.is_file() {
        return set_private_file(root);
    }
    if !metadata.is_dir() {
        return Ok(());
    }

    set_dir_mode(root)?;
    for entry in fs::read_dir(root)? {
        harden_private_tree(&entry?.path())?;
    }
    Ok(())
}

fn set_dir_mode(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn validate_clawd_root(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || matches!(
            path.to_str(),
            Some(
                "/"
                    | "/tmp"
                    | "/var"
                    | "/var/tmp"
                    | "/var/lib"
                    | "/var/log"
                    | "/run"
            )
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsafe daemon storage root {}", path.display()),
        ));
    }
    let mut existing = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "daemon root has no parent")
    })?;
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "daemon root has no existing ancestor",
            )
        })?;
    }
    let canonical_parent = existing.canonicalize()?;
    if canonical_parent != existing {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "daemon storage ancestor contains a symlink: {}",
                existing.display()
            ),
        ));
    }
    reject_symlink(path)?;
    if let Ok(metadata) = fs::metadata(path) {
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("daemon storage root is not a directory: {}", path.display()),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() as u32 } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("daemon storage root is not owned by clawd: {}", path.display()),
                ));
            }
        }
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing symlinked storage path {}", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn primary_gid(uid: u32) -> io::Result<u32> {
    const BUF_SIZE: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; BUF_SIZE];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid as libc::uid_t,
            &mut pwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("passwd entry not found for uid {uid}"),
        ));
    }
    Ok(pwd.pw_gid as u32)
}

#[cfg(unix)]
fn chown(path: &Path, uid: u32, gid: u32) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let rc = unsafe {
        libc::chown(
            path.as_ptr(),
            uid as libc::uid_t,
            gid as libc::gid_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
