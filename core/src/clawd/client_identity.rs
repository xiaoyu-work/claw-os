use serde::Serialize;
use std::path::PathBuf;
use tokio::net::UnixStream;

#[derive(Debug, Clone, Serialize)]
pub struct ClientIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
}

impl ClientIdentity {
    pub fn from_stream(stream: &UnixStream) -> Self {
        peer_identity(stream).unwrap_or_else(Self::unknown)
    }

    pub fn unknown() -> Self {
        Self {
            pid: None,
            uid: None,
            gid: None,
        }
    }

    /// Resolve this peer's `$HOME` directory from the passwd database
    /// via `getpwuid_r`. Returns `None` when the uid is unknown, when
    /// passwd lookup fails, or on non-Linux platforms (clawd is Linux-
    /// only, so this is harmless).
    ///
    /// clawd uses this to read `<home>/.config/cos/config.json` for
    /// the requesting user instead of its own root-owned config —
    /// without it, `cos agent ask` from a non-root user falls back to
    /// the empty system default and fails "no LLM provider configured".
    pub fn home_dir(&self) -> Option<PathBuf> {
        let uid = self.uid?;
        resolve_home(uid)
    }
}

#[cfg(target_os = "linux")]
fn peer_identity(stream: &UnixStream) -> Option<ClientIdentity> {
    let cred = stream.peer_cred().ok()?;
    Some(ClientIdentity {
        pid: cred.pid().and_then(|pid| u32::try_from(pid).ok()),
        uid: Some(cred.uid()),
        gid: Some(cred.gid()),
    })
}

#[cfg(not(target_os = "linux"))]
fn peer_identity(_stream: &UnixStream) -> Option<ClientIdentity> {
    None
}

#[cfg(unix)]
fn resolve_home(uid: u32) -> Option<PathBuf> {
    use std::ffi::{CStr, OsString};
    use std::os::unix::ffi::OsStringExt;

    // 16 KiB matches `sysconf(_SC_GETPW_R_SIZE_MAX)` on glibc; large
    // enough for any realistic passwd entry. If it ever isn't, we
    // bail rather than retry — getting home wrong is recoverable
    // (falls back to clawd's own config), getting it wrong silently
    // is not.
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
        return None;
    }
    if pwd.pw_dir.is_null() {
        return None;
    }
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
    let bytes = dir.to_bytes().to_vec();
    if bytes.is_empty() {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn resolve_home(_uid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_identity_has_no_uid_or_home() {
        let id = ClientIdentity::unknown();
        assert!(id.uid.is_none());
        assert!(id.home_dir().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_home_for_current_uid_matches_passwd() {
        // The current process's uid must resolve to a real passwd
        // entry on any working unix system. Compare against the
        // `HOME` env var as a sanity check (they should normally
        // agree; if HOME has been overridden we just skip).
        let uid = unsafe { libc::getuid() } as u32;
        let resolved = resolve_home(uid);
        assert!(resolved.is_some(), "getpwuid_r returned None for self uid");
        if let (Some(env_home), Some(pwd_home)) =
            (std::env::var_os("HOME"), resolved.as_ref())
        {
            if env_home != pwd_home.as_os_str() {
                // Possible in containers where HOME is set to /root
                // but passwd points elsewhere — log and move on.
                eprintln!(
                    "note: HOME ({:?}) differs from passwd entry ({:?})",
                    env_home, pwd_home
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_home_for_bogus_uid_returns_none() {
        // uid 4_000_000_001 is well above any realistic system uid.
        assert!(resolve_home(4_000_000_001).is_none());
    }
}
