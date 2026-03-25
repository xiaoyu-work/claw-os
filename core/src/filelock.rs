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
/// Uses write-to-tmp + rename(2) for crash safety.
/// Parent directories are created automatically.
pub fn write_locked(path: &Path, data: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    // Open or create the target file for locking.
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(format!(
                "flock LOCK_EX {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    // Write to tmp, then atomic rename(2).
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, data).map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
    fs::rename(&tmp_path, path).map_err(|e| format!("rename {}: {e}", path.display()))?;

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

/// Append a line to a file under an exclusive lock.
/// Used for append-only logs (audit.jsonl, watch history).
pub fn append_locked(path: &Path, line: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(format!(
                "flock LOCK_EX {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    writeln!(file, "{}", line).map_err(|e| format!("write {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }

    drop(file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();

    fn test_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
        INIT.call_once(|| {
            let _ = fs::create_dir_all(&dir);
        });
        dir
    }

    #[test]
    fn write_and_read() {
        let path = test_dir().join("filelock-wr.json");
        write_locked(&path, r#"{"hello":"world"}"#).unwrap();
        let data = read_locked(&path).unwrap().unwrap();
        assert_eq!(data, r#"{"hello":"world"}"#);
    }

    #[test]
    fn read_nonexistent() {
        let path = test_dir().join("filelock-nonexistent.json");
        assert!(read_locked(&path).unwrap().is_none());
    }

    #[test]
    fn append_creates_and_appends() {
        let path = test_dir().join("filelock-append.jsonl");
        let _ = fs::remove_file(&path);
        append_locked(&path, "line1").unwrap();
        append_locked(&path, "line2").unwrap();
        let data = fs::read_to_string(&path).unwrap();
        assert_eq!(data.lines().count(), 2);
    }

    #[test]
    fn write_atomic_no_leftover_tmp() {
        let path = test_dir().join("filelock-atomic.json");
        write_locked(&path, "first").unwrap();
        write_locked(&path, "second").unwrap();
        let data = read_locked(&path).unwrap().unwrap();
        assert_eq!(data, "second");
        assert!(!path.with_extension("tmp").exists());
    }
}
