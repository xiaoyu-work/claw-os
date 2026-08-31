//! Exclusive host-kernel identities for extension containment domains.

use std::collections::{HashMap, HashSet};
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex};

pub const UID_MIN_ENV: &str = "COS_EXTENSION_UID_MIN";
pub const UID_COUNT_ENV: &str = "COS_EXTENSION_UID_COUNT";
pub const DEFAULT_UID_MIN: u32 = 61_184;
pub const DEFAULT_UID_COUNT: u32 = 64;
const MAX_UID_COUNT: u32 = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionIdentity {
    pub uid: u32,
    pub gid: u32,
    pub username: String,
}

#[derive(Debug)]
pub struct ExtensionIdentityPool {
    identities: Vec<ExtensionIdentity>,
    in_use: Mutex<HashSet<u32>>,
    retained_locks: Mutex<HashMap<u32, std::fs::File>>,
}

impl ExtensionIdentityPool {
    pub fn load(gid: u32) -> Result<Arc<Self>, String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("extension identity allocation requires a root broker".to_string());
        }
        if gid == 0 {
            return Err("extension execution gid must not be root".to_string());
        }
        let first = parse_env_u32(UID_MIN_ENV, DEFAULT_UID_MIN)?;
        let count = parse_env_u32(UID_COUNT_ENV, DEFAULT_UID_COUNT)?;
        if first < 60_001 || count == 0 || count > MAX_UID_COUNT {
            return Err(format!(
                "extension uid range must start above 60000 and contain 1..={MAX_UID_COUNT} identities"
            ));
        }
        let last = first
            .checked_add(count - 1)
            .ok_or_else(|| "extension uid range overflows u32".to_string())?;
        if last == u32::MAX {
            return Err("extension uid range includes the invalid uid value".to_string());
        }

        let mut identities = Vec::with_capacity(count as usize);
        for uid in first..=last {
            if passwd_name(uid)?.is_some() {
                return Err(format!(
                    "reserved extension uid {uid} belongs to a host account; choose an unmapped range"
                ));
            }
            identities.push(ExtensionIdentity {
                uid,
                gid,
                username: format!("cos-extension-{uid}"),
            });
        }
        Ok(Arc::new(Self {
            identities,
            in_use: Mutex::new(HashSet::new()),
            retained_locks: Mutex::new(HashMap::new()),
        }))
    }

    pub fn acquire(self: &Arc<Self>, owner_uid: u32) -> Result<ExtensionIdentityLease, String> {
        let mut in_use = self
            .in_use
            .lock()
            .map_err(|_| "extension identity pool is poisoned".to_string())?;
        for identity in &self.identities {
            if passwd_name(identity.uid)?.is_some() {
                return Err(format!(
                    "reserved extension uid {} became mapped to a host account",
                    identity.uid
                ));
            }
            if identity.uid == owner_uid
                || in_use.contains(&identity.uid)
                || uid_has_process(identity.uid)
            {
                continue;
            }
            let Some(lock) = try_uid_lock(identity.uid)? else {
                continue;
            };
            crate::storage::purge_routed_extension_reader(identity.uid)?;
            in_use.insert(identity.uid);
            return Ok(ExtensionIdentityLease {
                pool: self.clone(),
                identity: identity.clone(),
                lock: Some(lock),
                release_on_drop: true,
            });
        }
        Err("no isolated extension execution uid is available".to_string())
    }

    pub fn len(&self) -> usize {
        self.identities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.identities.is_empty()
    }

    fn release(&self, uid: u32) {
        if let Ok(mut in_use) = self.in_use.lock() {
            in_use.remove(&uid);
        }
    }

    fn retain_lock(&self, uid: u32, lock: std::fs::File) {
        if let Ok(mut retained) = self.retained_locks.lock() {
            retained.insert(uid, lock);
        } else {
            std::mem::forget(lock);
        }
    }
}

#[derive(Debug)]
pub struct ExtensionIdentityLease {
    pool: Arc<ExtensionIdentityPool>,
    identity: ExtensionIdentity,
    lock: Option<std::fs::File>,
    release_on_drop: bool,
}

impl ExtensionIdentityLease {
    pub fn identity(&self) -> &ExtensionIdentity {
        &self.identity
    }

    /// Stop automatic release before a process may have entered this uid.
    ///
    /// The caller may release it again only after cgroup cleanup is verified.
    pub fn retain_until_cleanup(&mut self) {
        self.release_on_drop = false;
    }

    pub fn release(mut self) {
        self.pool.release(self.identity.uid);
        self.lock.take();
        self.release_on_drop = false;
    }
}

impl Drop for ExtensionIdentityLease {
    fn drop(&mut self) {
        if self.release_on_drop {
            self.pool.release(self.identity.uid);
            self.lock.take();
        } else if let Some(lock) = self.lock.take() {
            self.pool.retain_lock(self.identity.uid, lock);
        }
    }
}

fn parse_env_u32(key: &str, default: u32) -> Result<u32, String> {
    match std::env::var(key) {
        Ok(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|error| format!("invalid {key}: {error}")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(format!("read {key}: {error}")),
    }
}

fn passwd_name(uid: u32) -> Result<Option<String>, String> {
    use std::ffi::CStr;

    const BUF_SIZE: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; BUF_SIZE];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 {
        return Err(format!(
            "lookup reserved extension uid {uid}: {}",
            std::io::Error::from_raw_os_error(rc)
        ));
    }
    if result.is_null() {
        return Ok(None);
    }
    if passwd.pw_name.is_null() {
        return Err(format!("passwd entry for uid {uid} has no name"));
    }
    let name = unsafe { CStr::from_ptr(passwd.pw_name) }
        .to_str()
        .map_err(|_| format!("passwd entry for uid {uid} is not UTF-8"))?;
    Ok(Some(name.to_string()))
}

fn uid_has_process(uid: u32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
        {
            continue;
        }

        let status = match std::fs::read_to_string(entry.path().join("status")) {
            Ok(status) => status,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return true,
        };
        if status.lines().find_map(|line| {
            line.strip_prefix("Uid:")
                .and_then(|values| values.split_whitespace().next())
                .and_then(|value| value.parse::<u32>().ok())
        }) == Some(uid)
        {
            return true;
        }
    }
    false
}

fn try_uid_lock(uid: u32) -> Result<Option<std::fs::File>, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let root = std::path::Path::new("/run/cos/extension-uids");
    std::fs::create_dir_all(root)
        .map_err(|error| format!("create extension uid lock directory: {error}"))?;
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("inspect extension uid lock directory: {error}"))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != 0
        || metadata.gid() != 0
    {
        return Err("extension uid lock directory has unsafe identity".to_string());
    }
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("protect extension uid lock directory: {error}"))?;
    let path = root.join(format!("{uid}.lock"));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| format!("open extension uid lock: {error}"))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("protect extension uid lock: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect extension uid lock: {error}"))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err("extension uid lock has unsafe identity".to_string());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(Some(file))
    } else {
        let error = std::io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            Ok(None)
        } else {
            Err(format!("lock extension uid {uid}: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/identity.rs"
    ));
}
