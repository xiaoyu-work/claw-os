//! Process-wide Root admission fencing, never a permission or an owner deny policy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

type Gates = Mutex<HashMap<String, Weak<RwLock<()>>>>;

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
    package_manager_idle()?;
    if super::app_store::delivery_pending(app)? || super::app_install::pending(app)? {
        return Err(format!("App `{app}` has an active or indeterminate package transaction"));
    }
    gate(app)?
        .try_read_owned()
        .map_err(|_| format!("App `{app}` package mutation is pending; retry admission afterwards"))
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
