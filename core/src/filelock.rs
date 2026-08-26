/// Atomic file I/O using Linux syscalls: flock(2) + rename(2).
///
/// No third-party crates. Uses libc (already a dependency) for:
///   - `flock(LOCK_SH)` / `flock(LOCK_EX)` — advisory file locking
///   - `rename(2)` via `std::fs::rename` — atomic on same filesystem
///
/// Write pattern (crash-safe):
///   1. Acquire exclusive flock on target file
///   2. Write data to `<target>.tmp` in same directory
///   3. `rename("<target>.tmp", "<target>")` — atomic swap
///   4. Release flock
///
/// If the process crashes between steps 2 and 3, the original file is intact.
/// If it crashes after step 3, the new data is fully written.
///
/// On non-Linux platforms, falls back to std::fs without locking (best-effort).
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Read a file's contents under a shared (read) lock.
/// Returns `Ok(None)` if the file does not exist.
pub fn read_locked(path: &Path) -> Result<Option<String>, String> {
    if !path.is_file() {
        return Ok(None);
    }

    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) };
        if ret != 0 {
            return Err(format!(
                "flock LOCK_SH {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    let data = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }

    drop(file);
    Ok(Some(data))
}

/// Write data atomically under an exclusive lock.
///
/// Uses write-to-tmp + rename(2) for crash safety, with fsync of the
/// temp file before rename and fsync of the parent directory after, so
/// the new bytes are durable on disk after this returns.
///
/// Lock surface: a sibling `<path>.lock` sentinel (same idiom as
/// [`update_locked`]). Locking the data file's own inode was unsafe
/// because the atomic-write path renames a tmp file over the data
/// file; the original inode goes away and any flock held on it
/// becomes useless. A separate sentinel inode is never renamed and
/// remains a stable synchronization point.
///
/// Parent directories are created automatically.
pub fn write_locked(path: &Path, data: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    let lock_path = lock_sentinel_path(path);
    let lock_file = open_private(&lock_path, false, false)
        .map_err(|e| format!("open lock {}: {e}", lock_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(format!(
                "flock LOCK_EX {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    // Write to tmp + fsync, then atomic rename(2), then fsync the
    // parent directory so the directory entry is durable too.
    let tmp_path = path.with_extension("tmp");
    {
        let mut tmp = open_private(&tmp_path, true, false)
            .map_err(|e| format!("open tmp {}: {e}", tmp_path.display()))?;
        tmp.write_all(data.as_bytes())
            .map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
        tmp.sync_all()
            .map_err(|e| format!("fsync {}: {e}", tmp_path.display()))?;
    }
    fs::rename(&tmp_path, path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    crate::storage::set_private_file(path).map_err(|e| format!("chmod {}: {e}", path.display()))?;
    if let Some(parent) = path.parent() {
        let _ = sync_dir(parent);
    }

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
    }

    drop(lock_file);
    Ok(())
}

/// fsync a directory so that recent rename(2) / unlink(2) operations
/// inside it become durable. Best-effort: opening a directory for
/// read+fsync is a Linux/POSIX-ism that works on every Unix we ship
/// to. On non-Unix the call is a no-op.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    let f = File::open(dir)?;
    f.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Append a line to a file under an exclusive lock.
/// Used for append-only logs (audit.jsonl, watch history).
pub fn append_locked(path: &Path, line: &str) -> Result<(), String> {
    with_exclusive_path_lock(path, || {
        let mut file =
            open_private(path, false, true).map_err(|e| format!("open {}: {e}", path.display()))?;
        writeln!(file, "{}", line).map_err(|e| format!("write {}: {e}", path.display()))
    })
}

/// Run `operation` while holding the stable sibling lock for `path`.
///
/// This is the primitive for compound append/rotate operations that must
/// inspect the current file and then mutate it without another writer landing
/// between those steps. The lock is on `<path>.lock`, so renaming `path`
/// while the closure runs does not invalidate the lock.
pub fn with_exclusive_path_lock<T, F>(path: &Path, operation: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    let lock_path = lock_sentinel_path(path);
    let lock_file = open_private(&lock_path, false, false)
        .map_err(|e| format!("open lock {}: {e}", lock_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(format!(
                "flock LOCK_EX {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    let result = operation();

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
    }

    drop(lock_file);
    result
}

/// Error returned by [`update_locked`]. Separates infrastructure
/// failures (lock acquisition, file I/O) from closure-supplied
/// errors so callers can pattern-match on their domain error.
#[derive(Debug)]
pub enum UpdateLockError<E> {
    /// Locking or I/O failure outside the user closure.
    Io(String),
    /// The user closure returned an error.
    Transform(E),
}

impl<E: std::fmt::Display> std::fmt::Display for UpdateLockError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateLockError::Io(s) => f.write_str(s),
            UpdateLockError::Transform(e) => write!(f, "{e}"),
        }
    }
}

impl<E: std::fmt::Display + std::fmt::Debug> std::error::Error for UpdateLockError<E> {}

/// Read-modify-write a file atomically. The closure receives the
/// current contents (or `None` if the file does not exist) and
/// returns the new contents to write.
///
/// Why this exists separately from `read_locked` + `write_locked`:
/// chaining them — as several callers used to — releases the
/// shared lock at the end of the read and re-acquires an exclusive
/// lock at the start of the write. A concurrent writer can land
/// between those two operations, so two RMW callers can both read
/// the same stale value and the last one wins. `update_locked`
/// holds an exclusive lock for the entire read+modify+write so
/// concurrent callers serialize correctly.
///
/// Lock surface: a sibling `<path>.lock` sentinel rather than the
/// data file itself. flock(2) attaches to an inode, but our atomic
/// write replaces the data file's inode (write-tmp + rename), so
/// any flock held on the original data file becomes useless after
/// the first writer renames over it. A separate sentinel inode is
/// never renamed away and stays a valid synchronization point.
pub fn update_locked<F, E>(path: &Path, transform: F) -> Result<(), UpdateLockError<E>>
where
    F: FnOnce(Option<String>) -> std::result::Result<String, E>,
{
    update_locked_with_prepare(path, transform, crate::storage::set_private_file)
}

pub fn update_locked_with_prepare<F, E, P>(
    path: &Path,
    transform: F,
    prepare_tmp: P,
) -> Result<(), UpdateLockError<E>>
where
    F: FnOnce(Option<String>) -> std::result::Result<String, E>,
    P: FnOnce(&Path) -> std::io::Result<()>,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| UpdateLockError::Io(format!("mkdir {}: {e}", parent.display())))?;
    }

    let lock_path = lock_sentinel_path(path);
    let lock_file = open_private(&lock_path, false, false)
        .map_err(|e| UpdateLockError::Io(format!("open lock {}: {e}", lock_path.display())))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(UpdateLockError::Io(format!(
                "flock LOCK_EX {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            )));
        }
    }

    // From this point we hold an exclusive lock on the sentinel.
    let existing: Option<String> = if path.is_file() {
        Some(
            fs::read_to_string(path)
                .map_err(|e| UpdateLockError::Io(format!("read {}: {e}", path.display())))?,
        )
    } else {
        None
    };

    let new_data = transform(existing).map_err(UpdateLockError::Transform)?;

    let tmp_path = path.with_extension("tmp");
    {
        let mut tmp = open_private(&tmp_path, true, false)
            .map_err(|e| UpdateLockError::Io(format!("open tmp {}: {e}", tmp_path.display())))?;
        tmp.write_all(new_data.as_bytes())
            .map_err(|e| UpdateLockError::Io(format!("write {}: {e}", tmp_path.display())))?;
        tmp.sync_all()
            .map_err(|e| UpdateLockError::Io(format!("fsync {}: {e}", tmp_path.display())))?;
    }
    prepare_tmp(&tmp_path)
        .map_err(|e| UpdateLockError::Io(format!("secure {}: {e}", tmp_path.display())))?;
    fs::rename(&tmp_path, path)
        .map_err(|e| UpdateLockError::Io(format!("rename {}: {e}", path.display())))?;
    if let Some(parent) = path.parent() {
        let _ = sync_dir(parent);
    }

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
    }
    drop(lock_file);
    Ok(())
}

/// Compute the path of the lock sentinel for `path`. Appending
/// `.lock` rather than swapping the extension keeps the sentinel
/// alongside the data file even when `path` already has multiple
/// dots (e.g. `state.json` -> `state.json.lock`).
fn lock_sentinel_path(path: &Path) -> std::path::PathBuf {
    let mut s: std::ffi::OsString = path.as_os_str().to_os_string();
    s.push(".lock");
    std::path::PathBuf::from(s)
}

fn open_private(path: &Path, truncate: bool, append: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(truncate)
        .append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    crate::storage::set_private_file(path)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/filelock.rs"
    ));
}
