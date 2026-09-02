//! Per-launch runtime directory.
//!
//! Every sandboxed worker gets one private directory on the host that
//! holds only the endpoints the policy grants it: the narrow broker
//! socket and, when egress is approved, the egress-broker socket. The
//! directory is created `0700` and owned by the account the worker
//! runs as, then removed when the launch ends.
//!
//! It is deliberately *not* under `/run/cos`: that tree belongs to the
//! root broker, and a worker must never be able to enumerate it.

use std::path::{Path, PathBuf};

/// Root for per-launch worker directories.
///
/// `XDG_RUNTIME_DIR` when the launcher has one (tmpfs, per-user,
/// cleaned by the session), otherwise a `worker/` subtree of the
/// launcher's own data directory.
pub fn root() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return path.join("cos-worker");
        }
    }
    crate::paths::data_dir().join("worker")
}

/// A private launch directory, removed on drop.
#[derive(Debug)]
pub struct LaunchDir {
    path: PathBuf,
}

impl LaunchDir {
    /// Create `root()/<id>` with `0700` permissions owned by
    /// `owner_uid` (when the launcher is privileged enough to set it).
    pub fn create(id: &str, owner: Option<(u32, u32)>) -> Result<Self, String> {
        let root = root();
        std::fs::create_dir_all(&root)
            .map_err(|error| format!("create worker runtime root: {error}"))?;
        harden(&root, owner)?;
        let path = root.join(id);
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
        std::fs::create_dir(&path)
            .map_err(|error| format!("create worker runtime dir: {error}"))?;
        harden(&path, owner)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for LaunchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
fn harden(path: &Path, owner: Option<(u32, u32)>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("restrict worker runtime dir: {error}"))?;
    if let Some((uid, gid)) = owner {
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| "worker runtime path is not representable".to_string())?;
            if unsafe { libc::chown(c_path.as_ptr(), uid, gid) } != 0 {
                return Err(format!(
                    "assign worker runtime dir to uid {uid}: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn harden(_path: &Path, _owner: Option<(u32, u32)>) -> Result<(), String> {
    Err("worker runtime directories require Unix".to_string())
}

/// Short, collision-resistant launch identifier. Used for the runtime
/// directory and the cgroup name; never derived from worker input.
pub fn launch_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}
