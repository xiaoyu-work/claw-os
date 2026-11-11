//! Process-wide Root admission fencing, never a permission or an owner deny policy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

type Gates = Mutex<HashMap<String, Weak<RwLock<()>>>>;

fn admissions() -> Arc<RwLock<()>> {
    static ADMISSIONS: OnceLock<Arc<RwLock<()>>> = OnceLock::new();
    ADMISSIONS.get_or_init(|| Arc::new(RwLock::new(()))).clone()
}

fn gate(app: &str) -> Result<Arc<RwLock<()>>, String> {
    static GATES: OnceLock<Gates> = OnceLock::new();
    let mut gates = GATES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "Root App mutation admission is unavailable")?;
    gates.retain(|_, gate| gate.strong_count() != 0);
    if let Some(gate) = gates.get(app).and_then(Weak::upgrade) {
        return Ok(gate);
    }
    let gate = Arc::new(RwLock::new(()));
    gates.insert(app.to_string(), Arc::downgrade(&gate));
    Ok(gate)
}

pub(crate) fn admit(app: &str) -> Result<OwnedRwLockReadGuard<()>, String> {
    let _global = admissions().try_read_owned()
        .map_err(|_| "A Root package transaction is draining App admissions")?;
    package_manager_idle()?;
    #[cfg(target_os = "linux")]
    if super::package_transactions::pending(app)? {
        return Err("A retained Root package effect has an unconfirmed outcome".into());
    }
    if super::app_store::delivery_pending(app)? || super::app_install::pending(app)? {
        return Err(format!("App `{app}` has an active or indeterminate package transaction"));
    }
    gate(app)?
        .try_read_owned()
        .map_err(|_| format!("App `{app}` package mutation is pending; retry admission afterwards"))
}

pub(crate) async fn fence_all() -> Result<OwnedRwLockWriteGuard<()>, String> {
    tokio::time::timeout(std::time::Duration::from_secs(10), admissions().write_owned())
        .await.map_err(|_| "Root global App admission has not drained".into())
}

fn package_manager_idle() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        if unsafe { libc::geteuid() } != 0 {
            return Ok(());
        }
        for path in ["/var/lib/dpkg/lock-frontend", "/var/lib/dpkg/lock"] {
            let file = match std::fs::OpenOptions::new().read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("OS package admission lock is unavailable: {error}")),
            };
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.uid() != 0 || metadata.nlink() != 1
                || metadata.mode() & 0o022 != 0
            {
                return Err("OS package admission lock is not protected".into());
            }
            let lock = libc::flock {
                l_type: libc::F_RDLCK as _, l_whence: libc::SEEK_SET as _,
                l_start: 0, l_len: 0, l_pid: 0,
            };
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) } != 0 {
                return Err("An OS package transaction is active; App admission waits for its checked outcome".into());
            }
        }
    }
    Ok(())
}

pub(crate) async fn fence(app: &str) -> Result<OwnedRwLockWriteGuard<()>, String> {
    tokio::time::timeout(std::time::Duration::from_secs(10), gate(app)?.write_owned())
        .await
        .map_err(|_| format!("App `{app}` admission has not drained; package mutation remains pending"))
}

type OwnerGates = Mutex<HashMap<(u32, String), Weak<RwLock<()>>>>;

fn owner_gate(app: &str, owner: u32) -> Result<Arc<RwLock<()>>, String> {
    static GATES: OnceLock<OwnerGates> = OnceLock::new();
    let mut gates = GATES.get_or_init(Default::default).lock()
        .map_err(|_| "Root owner/App admission is unavailable")?;
    gates.retain(|_, gate| gate.strong_count() != 0);
    let key = (owner, app.to_string());
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) { return Ok(gate); }
    let gate = Arc::new(RwLock::new(()));
    gates.insert(key, Arc::downgrade(&gate));
    Ok(gate)
}

pub(crate) struct OwnerAdmission {
    _package: OwnedRwLockReadGuard<()>,
    _owner: OwnedRwLockReadGuard<()>,
}

pub(crate) fn admit_for_owner(app: &str, owner: u32) -> Result<OwnerAdmission, String> {
    let package = admit(app)?;
    if super::trust_withdrawal::pending(owner, app)? {
        return Err("Root retirement remains pending for this owner/App".into());
    }
    let owner = owner_gate(app, owner)?.try_read_owned()
        .map_err(|_| "Root owner/App retirement is active")?;
    Ok(OwnerAdmission { _package: package, _owner: owner })
}

pub(crate) struct OwnerFence {
    _package: OwnedRwLockReadGuard<()>,
    _owner: OwnedRwLockWriteGuard<()>,
}

pub(crate) async fn fence_owner(app: &str, owner: u32) -> Result<OwnerFence, String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let package = tokio::time::timeout_at(deadline, gate(app)?.read_owned()).await
        .map_err(|_| "Shared App publication is still active; owner retirement remains pending")?;
    let owner = tokio::time::timeout_at(deadline, owner_gate(app, owner)?.write_owned()).await
        .map_err(|_| "Owner/App admissions have not drained; retirement remains pending")?;
    Ok(OwnerFence { _package: package, _owner: owner })
}
