use serde::Serialize;
use std::path::PathBuf;

use super::transport::PeerProcess;

#[derive(Debug, Clone)]
pub struct AuthenticatedExtensionHost {
    pub purpose: crate::extension_host::protocol::HostPurpose,
    pub lease_id: String,
    pub authority_session_id: Option<String>,
    pub host_session_id: Option<String>,
    pub owner_uid: u32,
    pub extension_uid: u32,
    pub capability_generation: String,
    pub host_pid: u32,
    pub host_start_time_ticks: Option<u64>,
}

/// The peer a request came from.
///
/// Built from credentials the kernel attached to the request and confirmed
/// against `/proc`, never from request fields. The broker-owned extension
/// proxy may additionally project its signed task-owner principal while
/// retaining the actual host uid in [`Self::execution_uid`].
#[derive(Debug, Clone, Serialize)]
pub struct ClientIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    /// Host-kernel uid of a process executing for the authenticated principal.
    ///
    /// Present only for the extension proxy, where an isolated uid is mapped
    /// to the task owner's authority after exact lease/session checks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_uid: Option<u32>,
    /// Field 22 of `/proc/<pid>/stat`, read when the peer was verified.
    ///
    /// Kept out of every serialization: it is an internal
    /// disambiguator for pid reuse, and audit records already carry the
    /// pid. Routes that need a start time re-read it themselves at the
    /// moment they bind to it.
    #[serde(skip)]
    pub start_time_ticks: Option<u64>,
    /// Kernel-observed local terminal presence at message admission. This is
    /// never read from request JSON or the peer's environment.
    #[serde(skip)]
    pub attended_local: bool,
    #[serde(skip)]
    pub extension_host: Option<AuthenticatedExtensionHost>,
}

impl ClientIdentity {
    /// The peer behind one authenticated message.
    pub fn from_peer(process: PeerProcess) -> Self {
        let attended_local = process_has_terminal(process.pid, process.start_time_ticks);
        Self {
            pid: Some(process.pid),
            uid: Some(process.uid),
            gid: Some(process.gid),
            execution_uid: None,
            start_time_ticks: Some(process.start_time_ticks),
            attended_local,
            extension_host: None,
        }
    }

    pub fn unknown() -> Self {
        Self {
            pid: None,
            uid: None,
            gid: None,
            execution_uid: None,
            start_time_ticks: None,
            attended_local: false,
            extension_host: None,
        }
    }

    /// Identity already verified by the extension proxy. The host-kernel uid
    /// stays visible while authority is projected as the task owner.
    pub(crate) fn from_verified_delegation(
        pid: u32,
        principal_uid: u32,
        execution_uid: u32,
        gid: u32,
        start_time_ticks: u64,
        extension_host: AuthenticatedExtensionHost,
    ) -> Self {
        Self {
            pid: Some(pid),
            uid: Some(principal_uid),
            gid: Some(gid),
            execution_uid: Some(execution_uid),
            start_time_ticks: Some(start_time_ticks),
            attended_local: false,
            extension_host: Some(extension_host),
        }
    }

    pub fn process_uid(&self) -> Option<u32> {
        self.execution_uid.or(self.uid)
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

#[cfg(target_os = "linux")]
fn process_has_terminal(pid: u32, start_time_ticks: u64) -> bool {
    if crate::proc::read_start_time_ticks_pub(pid) != Some(start_time_ticks) {
        return false;
    }
    let attended = (0..=2).any(|fd| {
        std::fs::read_link(format!("/proc/{pid}/fd/{fd}"))
            .ok()
            .and_then(|path| path.to_str().map(str::to_string))
            .is_some_and(|path| {
                path == "/dev/tty"
                    || path == "/dev/console"
                    || path.starts_with("/dev/pts/")
                    || path.starts_with("/dev/tty")
            })
    });
    attended && crate::proc::read_start_time_ticks_pub(pid) == Some(start_time_ticks)
}

#[cfg(not(target_os = "linux"))]
fn process_has_terminal(_pid: u32, _start_time_ticks: u64) -> bool {
    false
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

/// Thread-local filesystem credentials. Never hold across an await or move to
/// another thread. Broker-owned session stores must be accessed outside it.
#[cfg(target_os = "linux")]
pub(crate) struct FsIdentityGuard {
    previous_uid: libc::c_int,
    previous_gid: libc::c_int,
    previous_groups: Option<Vec<libc::gid_t>>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(target_os = "linux")]
impl FsIdentityGuard {
    pub(crate) fn enter(uid: u32) -> Result<Self, String> {
        let (gid, groups) = owner_groups(uid)?;
        Self::enter_groups(uid, gid, &groups)
    }

    fn enter_groups(uid: u32, gid: u32, groups: &[libc::gid_t]) -> Result<Self, String> {
        let previous_groups = {
            let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
            if count < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let mut previous = vec![0; count as usize];
            if unsafe { libc::getgroups(count, previous.as_mut_ptr()) } < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            // libc::setgroups synchronizes all threads; this raw syscall only
            // changes the synchronous provider thread, like setfsuid itself.
            let mut current = previous.clone();
            current.sort_unstable();
            let mut wanted = groups.to_vec();
            wanted.sort_unstable();
            if current == wanted {
                None
            } else {
                if unsafe { libc::syscall(libc::SYS_setgroups, groups.len(), groups.as_ptr()) } != 0
                {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                Some(previous)
            }
        };
        let guard = Self {
            previous_gid: unsafe { libc::setfsgid(gid) },
            previous_uid: unsafe { libc::setfsuid(uid) },
            previous_groups,
            _thread: std::marker::PhantomData,
        };
        if unsafe { libc::setfsuid(!0) } != uid as libc::c_int
            || unsafe { libc::setfsgid(!0) } != gid as libc::c_int
        {
            return Err(format!("failed to enter filesystem identity {uid}:{gid}"));
        }
        Ok(guard)
    }
}

#[cfg(target_os = "linux")]
impl Drop for FsIdentityGuard {
    fn drop(&mut self) {
        unsafe {
            libc::setfsuid(self.previous_uid as libc::uid_t);
            libc::setfsgid(self.previous_gid as libc::gid_t);
            if libc::setfsuid(!0) != self.previous_uid || libc::setfsgid(!0) != self.previous_gid {
                std::process::abort();
            }
            if let Some(groups) = &self.previous_groups {
                // Continuing a daemon thread under unknown credentials is unsafe.
                if libc::syscall(libc::SYS_setgroups, groups.len(), groups.as_ptr()) != 0 {
                    std::process::abort();
                }
            }
        }
    }
}

/// Resolve account credentials, not the isolated process's execution GID.
/// NSS failures and unknown owners fail closed; no ambient-group fallback.
#[cfg(target_os = "linux")]
pub(crate) fn owner_groups(uid: u32) -> Result<(u32, Vec<libc::gid_t>), String> {
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() || pwd.pw_name.is_null() || pwd.pw_uid != uid {
        return Err(format!("filesystem owner account {uid} is unavailable"));
    }
    let mut count = 0;
    unsafe { libc::getgrouplist(pwd.pw_name, pwd.pw_gid, std::ptr::null_mut(), &mut count) };
    if !(1..=65_536).contains(&count) {
        return Err(format!("filesystem owner groups for {uid} are unavailable"));
    }
    let mut groups = vec![0; count as usize];
    if unsafe { libc::getgrouplist(pwd.pw_name, pwd.pw_gid, groups.as_mut_ptr(), &mut count) } < 0 {
        return Err(format!(
            "filesystem owner groups for {uid} changed during lookup"
        ));
    }
    groups.truncate(count as usize);
    groups.sort_unstable();
    groups.dedup();
    Ok((pwd.pw_gid, groups))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/client_identity.rs"
    ));
}
