//! Package-reserved host-kernel identities for extension containment domains.

use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const GROUP_NAME: &str = "cos-extension";
pub const GROUP_GID: u32 = 60_999;
pub const IDENTITY_PREFIX: &str = "cos-ext-";
pub const FIRST_UID: u32 = 61_000;
pub const IDENTITY_COUNT: u32 = 64;
pub const TASK_IDENTITY_COUNT: u32 = 56;
pub const SERVICE_IDENTITY_COUNT: u32 = IDENTITY_COUNT - TASK_IDENTITY_COUNT;
pub const IDENTITY_HOME: &str = "/nonexistent";
pub const IDENTITY_SHELL: &str = "/usr/sbin/nologin";
pub const SYSTEMD_DYNAMIC_UID_MIN: u32 = 61_184;
pub const SYSTEMD_DYNAMIC_UID_MAX: u32 = 65_519;
pub const RESERVATION_MANIFEST: &str = "/var/lib/cos/extension-identities.reserved";
const QUARANTINE_DIR: &str = "/var/lib/cos/extension-quarantine";

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
    validate_on_acquire: bool,
    execution_gid: u32,
    quarantine_dir: Option<PathBuf>,
}

impl ExtensionIdentityPool {
    pub fn load(gid: u32) -> Result<Arc<Self>, String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("extension identity allocation requires a root broker".to_string());
        }
        validate_runtime_reservation(gid)?;
        let pool = Self::from_identities(
            expected_identities(gid),
            true,
            Some(PathBuf::from(QUARANTINE_DIR)),
        );
        pool.recover_quarantined()?;
        Ok(pool)
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn for_test(gid: u32) -> Arc<Self> {
        Self::from_identities(expected_identities(gid), false, None)
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn for_test_with_quarantine(
        gid: u32,
        quarantine_dir: PathBuf,
    ) -> Result<Arc<Self>, String> {
        let pool = Self::from_identities(expected_identities(gid), false, Some(quarantine_dir));
        pool.recover_quarantined()?;
        Ok(pool)
    }

    fn from_identities(
        identities: Vec<ExtensionIdentity>,
        validate_on_acquire: bool,
        quarantine_dir: Option<PathBuf>,
    ) -> Arc<Self> {
        let execution_gid = identities
            .first()
            .map(|identity| identity.gid)
            .unwrap_or(GROUP_GID);
        Arc::new(Self {
            identities,
            in_use: Mutex::new(HashSet::new()),
            retained_locks: Mutex::new(HashMap::new()),
            validate_on_acquire,
            execution_gid,
            quarantine_dir,
        })
    }

    pub fn acquire(
        self: &Arc<Self>,
        owner_uid: u32,
        purpose: super::protocol::HostPurpose,
    ) -> Result<ExtensionIdentityLease, String> {
        if self.validate_on_acquire {
            validate_runtime_reservation(self.execution_gid)?;
        }
        let mut in_use = self
            .in_use
            .lock()
            .map_err(|_| "extension identity pool is poisoned".to_string())?;
        for (index, identity) in self.identities.iter().enumerate() {
            if !identity_supports_purpose(index, purpose) {
                continue;
            }
            if identity.uid == owner_uid
                || in_use.contains(&identity.uid)
                || uid_has_process(identity.uid)
                || uid_runtime_exists(identity.uid)
            {
                continue;
            }
            let Some(lock) = try_uid_lock(identity.uid)? else {
                continue;
            };
            if self.quarantine_dir.is_some() {
                if let Err(error) = self.recover_identity(identity.uid) {
                    tracing::error!(
                        extension_uid = identity.uid,
                        error = %error,
                        "extension identity remains quarantined"
                    );
                    in_use.insert(identity.uid);
                    self.retain_lock(identity.uid, lock);
                    continue;
                }
            }
            crate::storage::purge_routed_extension_reader(identity.uid)?;
            in_use.insert(identity.uid);
            return Ok(ExtensionIdentityLease {
                pool: self.clone(),
                identity: identity.clone(),
                lock: Some(lock),
                release_on_drop: true,
                cleanup_record: None,
            });
        }
        Err(format!(
            "no isolated {} extension identity is available",
            match purpose {
                super::protocol::HostPurpose::Task => "task",
                super::protocol::HostPurpose::AppService => "App service",
            }
        ))
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

    fn recover_quarantined(&self) -> Result<(), String> {
        let Some(directory) = self.quarantine_dir.as_deref() else {
            return Ok(());
        };
        ensure_quarantine_dir(directory)?;
        for identity in &self.identities {
            if !marker_path(directory, identity.uid).exists() {
                continue;
            }
            let Some(lock) = try_uid_lock(identity.uid)? else {
                continue;
            };
            if let Err(error) = self.recover_identity(identity.uid) {
                tracing::error!(
                    extension_uid = identity.uid,
                    error = %error,
                    "startup could not recover quarantined extension identity"
                );
                self.in_use
                    .lock()
                    .map_err(|_| "extension identity pool is poisoned".to_string())?
                    .insert(identity.uid);
                self.retain_lock(identity.uid, lock);
            }
        }
        Ok(())
    }

    fn recover_identity(&self, uid: u32) -> Result<(), String> {
        let Some(directory) = self.quarantine_dir.as_deref() else {
            return Ok(());
        };
        let Some(record) = read_cleanup_record(directory, uid)? else {
            return Ok(());
        };
        if uid_has_process(uid) {
            return Err(format!(
                "extension uid {uid} still owns a process after containment recovery"
            ));
        }
        if uid_runtime_exists(uid) {
            return Err(format!(
                "/run/user/{uid} still exists for quarantined extension identity"
            ));
        }
        if let Some(task_name) = record.task_name {
            crate::extension_host::spawn::HostPaths::recover(record.owner_uid, &task_name)?;
        }
        crate::storage::purge_routed_extension_reader(uid)?;
        if uid_has_process(uid) {
            return Err(format!(
                "extension uid {uid} acquired a process during quarantine recovery"
            ));
        }
        remove_cleanup_record(directory, uid)
    }
}

#[derive(Debug)]
pub struct ExtensionIdentityLease {
    pool: Arc<ExtensionIdentityPool>,
    identity: ExtensionIdentity,
    lock: Option<std::fs::File>,
    release_on_drop: bool,
    cleanup_record: Option<CleanupRecord>,
}

fn identity_supports_purpose(index: usize, purpose: super::protocol::HostPurpose) -> bool {
    let service_identity = index >= TASK_IDENTITY_COUNT as usize;
    service_identity == (purpose == super::protocol::HostPurpose::AppService)
}

impl ExtensionIdentityLease {
    pub fn identity(&self) -> &ExtensionIdentity {
        &self.identity
    }

    pub fn begin_task(&mut self, owner_uid: u32) -> Result<(), String> {
        if self.cleanup_record.is_some() {
            return Err("extension identity already has an active cleanup record".to_string());
        }
        let record = CleanupRecord {
            uid: self.identity.uid,
            owner_uid,
            task_name: None,
        };
        if let Some(directory) = self.pool.quarantine_dir.as_deref() {
            write_cleanup_record(directory, &record)?;
        }
        self.release_on_drop = false;
        self.cleanup_record = Some(record);
        Ok(())
    }

    pub fn record_task(&mut self, owner_uid: u32, task_name: &str) -> Result<(), String> {
        validate_task_name(task_name)?;
        let mut record = self
            .cleanup_record
            .clone()
            .ok_or_else(|| "extension identity cleanup record was not started".to_string())?;
        if record.owner_uid != owner_uid {
            return Err("extension identity cleanup owner changed".to_string());
        }
        record.task_name = Some(task_name.to_string());
        if let Some(directory) = self.pool.quarantine_dir.as_deref() {
            write_cleanup_record(directory, &record)?;
        }
        self.cleanup_record = Some(record);
        Ok(())
    }

    pub fn release(mut self) -> Result<(), String> {
        if uid_has_process(self.identity.uid) {
            self.release_on_drop = false;
            return Err(format!(
                "extension uid {} still owns a process after cleanup",
                self.identity.uid
            ));
        }
        if uid_runtime_exists(self.identity.uid) {
            self.release_on_drop = false;
            return Err(format!(
                "/run/user/{} still exists after extension cleanup",
                self.identity.uid
            ));
        }
        if self.cleanup_record.is_some() {
            if let Some(directory) = self.pool.quarantine_dir.as_deref() {
                if let Err(error) = remove_cleanup_record(directory, self.identity.uid) {
                    if let Some(lock) = self.lock.take() {
                        self.pool.retain_lock(self.identity.uid, lock);
                    }
                    return Err(error);
                }
            }
            self.cleanup_record = None;
        }
        self.pool.release(self.identity.uid);
        self.lock.take();
        self.release_on_drop = false;
        Ok(())
    }
}

impl Drop for ExtensionIdentityLease {
    fn drop(&mut self) {
        if self.release_on_drop {
            self.pool.release(self.identity.uid);
            self.lock.take();
        } else if let Some(lock) = self.lock.take() {
            tracing::error!(
                extension_uid = self.identity.uid,
                "extension identity lease dropped before verified cleanup; retaining quarantine"
            );
            self.pool.retain_lock(self.identity.uid, lock);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CleanupRecord {
    uid: u32,
    owner_uid: u32,
    task_name: Option<String>,
}

fn validate_task_name(task_name: &str) -> Result<(), String> {
    if task_name.len() != 32
        || !task_name
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("extension cleanup task name is invalid".to_string());
    }
    Ok(())
}

fn marker_path(directory: &Path, uid: u32) -> PathBuf {
    directory.join(format!("{uid}.state"))
}

fn ensure_quarantine_dir(directory: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    std::fs::create_dir_all(directory)
        .map_err(|error| format!("create extension quarantine directory: {error}"))?;
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(directory)
        .map_err(|error| format!("pin extension quarantine directory: {error}"))?;
    let metadata = handle
        .metadata()
        .map_err(|error| format!("inspect extension quarantine directory: {error}"))?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.gid() != 0 {
        return Err("extension quarantine directory has unsafe identity".to_string());
    }
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("protect extension quarantine directory: {error}"))?;
    let metadata = handle
        .metadata()
        .map_err(|error| format!("verify extension quarantine directory: {error}"))?;
    if metadata.mode() & 0o7777 != 0o700 {
        return Err("extension quarantine directory has unsafe mode".to_string());
    }
    Ok(())
}

fn cleanup_record_text(record: &CleanupRecord) -> String {
    let mut text = format!("version=1\nuid={}\n", record.uid);
    text.push_str(&format!("owner_uid={}\n", record.owner_uid));
    text.push_str(&format!(
        "task_name={}\n",
        record.task_name.as_deref().unwrap_or("-")
    ));
    text
}

fn parse_cleanup_record(content: &str, expected_uid: u32) -> Result<CleanupRecord, String> {
    let lines = content.lines().collect::<Vec<_>>();
    if lines.len() != 4
        || lines[0] != "version=1"
        || !lines[1].starts_with("uid=")
        || !lines[2].starts_with("owner_uid=")
        || !lines[3].starts_with("task_name=")
    {
        return Err("extension cleanup record has an invalid shape".to_string());
    }
    let uid = lines[1][4..]
        .parse::<u32>()
        .map_err(|_| "extension cleanup record has an invalid uid".to_string())?;
    if uid != expected_uid {
        return Err("extension cleanup record uid does not match its filename".to_string());
    }
    let owner = &lines[2]["owner_uid=".len()..];
    let task_name = &lines[3]["task_name=".len()..];
    let owner_uid = owner
        .parse::<u32>()
        .map_err(|_| "extension cleanup record has an invalid owner uid".to_string())?;
    let task_name = match task_name {
        "-" => None,
        _ => {
            validate_task_name(task_name)?;
            Some(task_name.to_string())
        }
    };
    Ok(CleanupRecord {
        uid,
        owner_uid,
        task_name,
    })
}

fn write_cleanup_record(directory: &Path, record: &CleanupRecord) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;

    ensure_quarantine_dir(directory)?;
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let temporary = directory.join(format!(".{}.{}.new", record.uid, nonce));
    let final_path = marker_path(directory, record.uid);
    let write = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|error| format!("create extension cleanup record: {error}"))?;
        file.write_all(cleanup_record_text(record).as_bytes())
            .map_err(|error| format!("write extension cleanup record: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync extension cleanup record: {error}"))?;
        std::fs::rename(&temporary, &final_path)
            .map_err(|error| format!("publish extension cleanup record: {error}"))?;
        sync_directory(directory)
    })();
    if write.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write
}

fn read_cleanup_record(directory: &Path, uid: u32) -> Result<Option<CleanupRecord>, String> {
    let path = marker_path(directory, uid);
    let Some(content) = read_root_policy_file(&path, Some(0o600), true)? else {
        return Ok(None);
    };
    parse_cleanup_record(&content, uid).map(Some)
}

fn remove_cleanup_record(directory: &Path, uid: u32) -> Result<(), String> {
    let path = marker_path(directory, uid);
    if read_root_policy_file(&path, Some(0o600), true)?.is_none() {
        return Ok(());
    }
    std::fs::remove_file(&path)
        .map_err(|error| format!("remove extension cleanup record: {error}"))?;
    sync_directory(directory)
}

fn sync_directory(directory: &Path) -> Result<(), String> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    let directory = std::ffi::CString::new(directory.as_os_str().as_bytes())
        .map_err(|_| "extension quarantine directory contains NUL".to_string())?;
    let fd = unsafe {
        libc::open(
            directory.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(format!(
            "open extension quarantine directory for sync: {}",
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.sync_all()
        .map_err(|error| format!("sync extension quarantine directory: {error}"))
}

fn validate_fixed_range() -> Result<(), String> {
    let last = FIRST_UID
        .checked_add(IDENTITY_COUNT - 1)
        .ok_or_else(|| "extension identity range overflows".to_string())?;
    if GROUP_GID <= 60_000
        || GROUP_GID >= FIRST_UID
        || IDENTITY_COUNT != 64
        || FIRST_UID <= 60_000
        || last >= SYSTEMD_DYNAMIC_UID_MIN
        || FIRST_UID <= SYSTEMD_DYNAMIC_UID_MAX && last >= SYSTEMD_DYNAMIC_UID_MIN
    {
        return Err(
            "extension identity range overlaps login or systemd DynamicUser space".to_string(),
        );
    }
    Ok(())
}

fn validate_runtime_reservation(gid: u32) -> Result<(), String> {
    validate_fixed_range()?;
    validate_execution_gid(gid)?;
    validate_group(gid)?;
    validate_subid_file(Path::new("/etc/subuid"), gid)?;
    validate_subid_file(Path::new("/etc/subgid"), gid)?;
    validate_reservation_manifest(Path::new(RESERVATION_MANIFEST), gid)?;
    for identity in expected_identities(gid) {
        validate_account(&identity)?;
    }
    Ok(())
}

fn validate_execution_gid(gid: u32) -> Result<(), String> {
    let last_uid = FIRST_UID + IDENTITY_COUNT - 1;
    if gid == 0 || gid >= SYSTEMD_DYNAMIC_UID_MIN || (FIRST_UID..=last_uid).contains(&gid) {
        return Err(format!(
            "extension execution gid {gid} overlaps root, reserved UIDs, or DynamicUser space"
        ));
    }
    Ok(())
}

fn expected_identities(gid: u32) -> Vec<ExtensionIdentity> {
    (0..IDENTITY_COUNT)
        .map(|index| ExtensionIdentity {
            uid: FIRST_UID + index,
            gid,
            username: format!("{IDENTITY_PREFIX}{index:02}"),
        })
        .collect()
}

fn validate_group(expected_gid: u32) -> Result<(), String> {
    let group = group_by_name(GROUP_NAME)?
        .ok_or_else(|| format!("package-created group `{GROUP_NAME}` is missing"))?;
    if group.name != GROUP_NAME
        || group.password != "x"
        || group.gid != expected_gid
        || group.gid == 0
        || group.gid >= SYSTEMD_DYNAMIC_UID_MIN
    {
        return Err(format!(
            "group `{GROUP_NAME}` has gid {}, expected {expected_gid} outside DynamicUser space",
            group.gid
        ));
    }
    if !group.members.is_empty() {
        return Err(format!(
            "group `{GROUP_NAME}` has unexpected supplementary members"
        ));
    }
    let reverse = group_by_gid(expected_gid)?
        .ok_or_else(|| format!("gid {expected_gid} has no NSS group record"))?;
    if reverse != group {
        return Err(format!(
            "gid {expected_gid} does not resolve to the exact `{GROUP_NAME}` record"
        ));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct GroupRecord {
    name: String,
    password: String,
    gid: u32,
    members: Vec<String>,
}

fn group_by_name(name: &str) -> Result<Option<GroupRecord>, String> {
    let name = std::ffi::CString::new(name).map_err(|_| "group name contains NUL".to_string())?;
    group_lookup(|group, buffer, result| unsafe {
        libc::getgrnam_r(
            name.as_ptr(),
            group,
            buffer.as_mut_ptr(),
            buffer.len(),
            result,
        )
    })
}

fn group_by_gid(gid: u32) -> Result<Option<GroupRecord>, String> {
    group_lookup(|group, buffer, result| unsafe {
        libc::getgrgid_r(gid, group, buffer.as_mut_ptr(), buffer.len(), result)
    })
}

fn group_lookup(
    lookup: impl FnOnce(&mut libc::group, &mut Vec<libc::c_char>, &mut *mut libc::group) -> libc::c_int,
) -> Result<Option<GroupRecord>, String> {
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut group: libc::group = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::group = std::ptr::null_mut();
    let rc = lookup(&mut group, &mut buffer, &mut result);
    if rc != 0 {
        return Err(format!(
            "lookup extension group: {}",
            std::io::Error::from_raw_os_error(rc)
        ));
    }
    if result.is_null() {
        return Ok(None);
    }
    let name = c_string(group.gr_name, "group name")?;
    let mut members = Vec::new();
    if !group.gr_mem.is_null() {
        let mut cursor = group.gr_mem;
        while unsafe { !(*cursor).is_null() } {
            members.push(c_string(unsafe { *cursor }, "group member")?);
            cursor = unsafe { cursor.add(1) };
        }
    }
    Ok(Some(GroupRecord {
        name,
        password: c_string(group.gr_passwd, "group password")?,
        gid: group.gr_gid,
        members,
    }))
}

fn validate_account(expected: &ExtensionIdentity) -> Result<(), String> {
    let account = account_by_name(&expected.username)?.ok_or_else(|| {
        format!(
            "package-created extension account `{}` is missing",
            expected.username
        )
    })?;
    let index = expected
        .uid
        .checked_sub(FIRST_UID)
        .ok_or_else(|| "extension account uid is outside the reserved range".to_string())?;
    if account.name != expected.username
        || account.password != "x"
        || account.uid != expected.uid
        || account.gid != expected.gid
        || account.gecos != format!("Claw OS extension slot {index}")
        || account.home != IDENTITY_HOME
        || account.shell != IDENTITY_SHELL
    {
        return Err(format!(
            "extension account `{}` does not match uid/gid/home/shell policy",
            expected.username
        ));
    }
    let reverse = account_by_uid(expected.uid)?
        .ok_or_else(|| format!("uid {} has no NSS passwd record", expected.uid))?;
    if reverse != account {
        return Err(format!(
            "uid {} does not resolve to the exact `{}` record",
            expected.uid, expected.username
        ));
    }
    if !shadow_password(&expected.username)?.is_some_and(|password| {
        !password.is_empty()
            && matches!(password.as_bytes()[0], b'!' | b'*')
            && password.bytes().all(|byte| matches!(byte, b'!' | b'*'))
    }) {
        return Err(format!(
            "extension account `{}` is not password-locked",
            expected.username
        ));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct AccountRecord {
    name: String,
    password: String,
    uid: u32,
    gid: u32,
    gecos: String,
    home: String,
    shell: String,
}

fn account_by_name(name: &str) -> Result<Option<AccountRecord>, String> {
    let name = std::ffi::CString::new(name).map_err(|_| "account name contains NUL".to_string())?;
    account_lookup(|passwd, buffer, result| unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            result,
        )
    })
}

fn account_by_uid(uid: u32) -> Result<Option<AccountRecord>, String> {
    account_lookup(|passwd, buffer, result| unsafe {
        libc::getpwuid_r(uid, passwd, buffer.as_mut_ptr(), buffer.len(), result)
    })
}

fn account_lookup(
    lookup: impl FnOnce(
        &mut libc::passwd,
        &mut Vec<libc::c_char>,
        &mut *mut libc::passwd,
    ) -> libc::c_int,
) -> Result<Option<AccountRecord>, String> {
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = lookup(&mut passwd, &mut buffer, &mut result);
    if rc != 0 {
        return Err(format!(
            "lookup extension account: {}",
            std::io::Error::from_raw_os_error(rc)
        ));
    }
    if result.is_null() {
        return Ok(None);
    }
    Ok(Some(AccountRecord {
        name: c_string(passwd.pw_name, "account name")?,
        password: c_string(passwd.pw_passwd, "account password")?,
        uid: passwd.pw_uid,
        gid: passwd.pw_gid,
        gecos: c_string(passwd.pw_gecos, "account comment")?,
        home: c_string(passwd.pw_dir, "account home")?,
        shell: c_string(passwd.pw_shell, "account shell")?,
    }))
}

fn shadow_password(name: &str) -> Result<Option<String>, String> {
    let name =
        std::ffi::CString::new(name).map_err(|_| "shadow account name contains NUL".to_string())?;
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut shadow: libc::spwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::spwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getspnam_r(
            name.as_ptr(),
            &mut shadow,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 {
        return Err(format!(
            "lookup extension account shadow record: {}",
            std::io::Error::from_raw_os_error(rc)
        ));
    }
    if result.is_null() {
        return Ok(None);
    }
    Ok(Some(c_string(shadow.sp_pwdp, "shadow password")?))
}

fn c_string(pointer: *const libc::c_char, field: &str) -> Result<String, String> {
    if pointer.is_null() {
        return Err(format!("NSS {field} is missing"));
    }
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map(str::to_string)
        .map_err(|_| format!("NSS {field} is not UTF-8"))
}

fn validate_reservation_manifest(path: &Path, gid: u32) -> Result<(), String> {
    let actual = read_root_policy_file(path, Some(0o600), false)?
        .ok_or_else(|| "extension identity reservation manifest is missing".to_string())?;
    let expected = reservation_manifest(gid);
    if actual != expected {
        return Err(
            "extension identity reservation manifest does not match NSS policy".to_string(),
        );
    }
    Ok(())
}

fn reservation_manifest(gid: u32) -> String {
    let mut manifest = format!("version=1\ngroup={GROUP_NAME}:{gid}\n");
    for identity in expected_identities(gid) {
        manifest.push_str(&format!(
            "identity={}:{}:{}:{IDENTITY_HOME}:{IDENTITY_SHELL}\n",
            identity.username, identity.uid, identity.gid
        ));
    }
    manifest
}

fn validate_subid_file(path: &Path, gid: u32) -> Result<(), String> {
    let Some(content) = read_root_policy_file(path, None, true)? else {
        return Ok(());
    };
    validate_subid_content(path, &content, gid)
}

fn validate_subid_content(path: &Path, content: &str, gid: u32) -> Result<(), String> {
    let is_subgid = path.file_name().and_then(|name| name.to_str()) == Some("subgid");
    let reserved_end = FIRST_UID + IDENTITY_COUNT - 1;
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(format!(
                "{} line {} is not a valid subordinate-id record",
                path.display(),
                index + 1
            ));
        }
        if fields[0].is_empty() {
            return Err(format!(
                "{} line {} has an empty owner",
                path.display(),
                index + 1
            ));
        }
        let start = fields[1]
            .parse::<u32>()
            .map_err(|_| format!("{} line {} has invalid start", path.display(), index + 1))?;
        let count = fields[2]
            .parse::<u32>()
            .map_err(|_| format!("{} line {} has invalid count", path.display(), index + 1))?;
        if count == 0 {
            return Err(format!(
                "{} line {} has zero count",
                path.display(),
                index + 1
            ));
        }
        let end = start.checked_add(count - 1).ok_or_else(|| {
            format!(
                "{} line {} overflows subordinate-id space",
                path.display(),
                index + 1
            )
        })?;
        let overlaps_uids = start <= reserved_end && end >= FIRST_UID;
        let overlaps_gid = is_subgid && start <= gid && end >= gid;
        if overlaps_uids || overlaps_gid {
            return Err(format!(
                "{} line {} overlaps package-reserved extension identities",
                path.display(),
                index + 1
            ));
        }
    }
    Ok(())
}

fn read_root_policy_file(
    path: &Path,
    exact_mode: Option<u32>,
    allow_missing: bool,
) -> Result<Option<String>, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(format!("open {}: {error}", path.display())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.nlink() != 1
        || exact_mode.is_some_and(|expected| mode != expected)
        || exact_mode.is_none() && mode & 0o022 != 0
    {
        return Err(format!("{} has unsafe ownership or mode", path.display()));
    }
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(Some(content))
}

fn uid_has_process(uid: u32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return true,
        };
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
        let Some(ids) = status.lines().find_map(|line| line.strip_prefix("Uid:")) else {
            return true;
        };
        let mut parsed = 0usize;
        let found = ids.split_whitespace().any(|value| {
            parsed += 1;
            value.parse::<u32>().map_or(true, |value| value == uid)
        });
        if found || parsed != 4 {
            return true;
        }
    }
    false
}

fn uid_runtime_exists(uid: u32) -> bool {
    match std::fs::symlink_metadata(format!("/run/user/{uid}")) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

fn try_uid_lock(uid: u32) -> Result<Option<std::fs::File>, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let root = Path::new("/run/cos/extension-uids");
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
