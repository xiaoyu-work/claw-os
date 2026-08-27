use serde::Serialize;
use std::path::PathBuf;

use super::transport::PeerProcess;

/// The peer a request came from.
///
/// Built from the credentials the kernel attached to the request
/// message and confirmed against `/proc`, never from anything the
/// request said about itself. See [`super::transport::peer`] for the
/// exact Linux semantics.
#[derive(Debug, Clone, Serialize)]
pub struct ClientIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    /// Field 22 of `/proc/<pid>/stat`, read when the peer was verified.
    ///
    /// Kept out of every serialization: it is an internal
    /// disambiguator for pid reuse, and audit records already carry the
    /// pid. Routes that need a start time re-read it themselves at the
    /// moment they bind to it.
    #[serde(skip)]
    pub start_time_ticks: Option<u64>,
}

impl ClientIdentity {
    /// The peer behind one authenticated message.
    pub fn from_peer(process: PeerProcess) -> Self {
        Self {
            pid: Some(process.pid),
            uid: Some(process.uid),
            gid: Some(process.gid),
            start_time_ticks: Some(process.start_time_ticks),
        }
    }

    pub fn unknown() -> Self {
        Self {
            pid: None,
            uid: None,
            gid: None,
            start_time_ticks: None,
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

    pub fn require_uid(&self) -> Result<u32, String> {
        self.uid
            .ok_or_else(|| "clawd peer uid is unavailable".to_string())
    }

    pub fn require_home_dir(&self) -> Result<PathBuf, String> {
        let uid = self.require_uid()?;
        resolve_home(uid).ok_or_else(|| format!("home directory is unavailable for peer uid {uid}"))
    }
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/client_identity.rs"
    ));
}
