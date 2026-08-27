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
    // Traversable, never listable. The per-owner agent state each
    // `claw-agentd` worker runs against lives at `<data>/users/<uid>`
    // and is owned by that account, so both the daemon root and the
    // `users/` level must let a non-root worker walk *through* them.
    // Every other subtree below stays `0700 root`.
    ensure_traversable_dir(&data)?;
    ensure_private_dir(&logs)?;

    let users = data.join("users");
    if users.exists() {
        harden_owner_partitioned_tree(&users)?;
    }
    for root in [
        data.join("agent"),
        data.join("approvals"),
        data.join("sessions"),
        data.join("proc"),
        data.join("clawd"),
        // The session event journal: chains, head anchors, the writer
        // lease and the MAC keys. Nothing below this is ever readable
        // or writable by a worker, an App or an MCP server.
        data.join("journal"),
    ] {
        if root.exists() {
            harden_private_tree(&root)?;
        }
    }
    harden_private_tree(&logs)
}

/// Prepare the per-owner agent state root an unprivileged worker writes
/// through: conversation memory, notes, todos and AI budget counters.
///
/// The leaf is owned by the account the task belongs to and stays
/// `0700`, so no other user can read it and `clawd` (root) can still
/// audit it. `<data>` and `<data>/users` are `0711`: a worker can walk
/// to its own directory but cannot enumerate the daemon's state or
/// discover other accounts' partitions by listing.
pub fn ensure_owner_agent_state_dir(uid: u32, gid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        if uid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to prepare owner agent state for uid 0",
            ));
        }
        let euid = unsafe { libc::geteuid() as u32 };
        if euid != 0 && euid != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("cannot prepare owner agent state for uid {uid} as uid {euid}"),
            ));
        }
        let data = crate::paths::data_dir();
        validate_clawd_root(&data)?;
        ensure_traversable_dir(&data)?;
        let users = data.join("users");
        reject_symlink(&users)?;
        fs::create_dir_all(&users)?;
        if euid == 0 {
            chown(&users, 0, 0)?;
        }
        fs::set_permissions(&users, fs::Permissions::from_mode(0o711))?;

        let owner_root = crate::paths::clawd_user_state_dir(uid);
        reject_symlink(&owner_root)?;
        fs::create_dir_all(&owner_root)?;
        if euid == 0 {
            chown(&owner_root, uid, gid)?;
        }
        fs::set_permissions(&owner_root, fs::Permissions::from_mode(0o700))?;

        for child in ["agent", "logs"] {
            let path = owner_root.join(child);
            reject_symlink(&path)?;
            fs::create_dir_all(&path)?;
            if euid == 0 {
                chown(&path, uid, gid)?;
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = gid;
        ensure_private_dir(&crate::paths::clawd_user_state_dir(uid))
    }
}

/// `users/` itself is daemon-owned and traversable; each `users/<uid>`
/// below it belongs to that account and is left alone, so hardening on
/// start-up cannot take a running worker's state away from it.
fn harden_owner_partitioned_tree(root: &Path) -> io::Result<()> {
    reject_symlink(root)?;
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return set_private_file(root);
    }
    #[cfg(unix)]
    fs::set_permissions(root, fs::Permissions::from_mode(0o711))?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let child = fs::symlink_metadata(&path)?;
        if child.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing symlink inside private state: {}", path.display()),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // A partition still owned by root predates the split (or
            // belongs to no account); keep it locked down.
            if child.is_dir() && child.uid() == 0 {
                harden_private_tree(&path)?;
            }
        }
        #[cfg(not(unix))]
        harden_private_tree(&path)?;
    }
    Ok(())
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

/// `0711`: walkable by any account, listable by none. Used for the
/// daemon state root and the `users/` level so an unprivileged
/// `claw-agentd` worker can reach its own owner partition without being
/// able to enumerate the daemon's state.
fn ensure_traversable_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o711))
    }
    #[cfg(not(unix))]
    Ok(())
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
