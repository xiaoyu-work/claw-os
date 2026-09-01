//! Shared helpers for `agent/` top-level modules.
//!
//! Two categories:
//!
//! 1. **Crash-safe persistence.** [`atomic_write_with_fsync`] performs the
//!    canonical `tmp + sync + rename + fsync(parent)` sequence so a power
//!    loss between write and rename can never expose a torn file. The agent
//!    sub-modules previously rolled their own `tmp + rename` helpers; those
//!    skipped `sync_data` and were vulnerable to recovery surprises. Routing
//!    every state-file writer through this helper makes that class of bug
//!    a one-line fix per call site.
//!
//! 2. **UTF-8-safe truncation.** [`char_safe_truncate`] returns the longest
//!    prefix of `s` not exceeding `n_bytes` that still ends on a char
//!    boundary, so callers can byte-bound a string for display without
//!    risking the `byte index N is not a char boundary` panic.

use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;

/// Write `data` to `path` atomically and synchronously.
///
/// Steps:
///   1. Create the parent directory if missing.
///   2. Open a fresh per-process tmp file (`<basename>.<pid>.<nonce>.tmp`)
///      in the parent directory. Per-process names mean two concurrent
///      writers never clobber each other's tmp.
///   3. `write_all` then `sync_all` (data + metadata) the tmp file.
///   4. `rename` tmp → `path` (atomic on a single filesystem).
///   5. `fsync` the parent directory so the dirent flip is durable.
///
/// Power-loss recovery: at any point you see either the previous file
/// contents (rename hadn't taken effect) or the new contents (rename
/// succeeded + parent dir fsync committed the dirent).
pub(crate) fn atomic_write_with_fsync(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent dir"))?;
    fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_string_lossy()
        .into_owned();
    // An unpredictable suffix plus create_new prevents concurrent writers
    // from ever sharing or truncating the same staging inode.
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let tmp = parent.join(format!(".{file_name}.{}.{nonce}.tmp", std::process::id()));

    {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        persistence_barrier("file_open")?;
        let mut f = options.open(&tmp)?;
        persistence_barrier("file_write")?;
        f.write_all(data)?;
        // sync_all flushes data + metadata; sync_data would skip mtime
        // and similar but both are sufficient for our durability claim.
        persistence_barrier("file_fsync")?;
        f.sync_all()?;
    }

    persistence_barrier("rename")?;
    if let Err(e) = fs::rename(&tmp, path) {
        match fs::remove_file(&tmp) {
            Ok(()) => sync_dir(parent)?,
            Err(cleanup) if cleanup.kind() != io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    cleanup.kind(),
                    format!("rename failed: {e}; temporary cleanup failed: {cleanup}"),
                ));
            }
            Err(_) => {}
        }
        return Err(e);
    }
    persistence_barrier("after_rename")?;
    crate::storage::set_private_file(path)?;

    sync_dir(parent)
}

/// Open and fsync a directory. A filesystem that cannot durably commit
/// directory entries is not safe for queue state transitions.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        persistence_barrier("dir_open")?;
        let d = fs::File::open(dir)?;
        persistence_barrier("dir_fsync")?;
        d.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "durable directory fsync is unsupported on this platform",
        ))
    }
}

pub(crate) fn persistence_barrier(point: &str) -> io::Result<()> {
    #[cfg(debug_assertions)]
    if std::env::var("COS_TEST_PERSISTENCE_FAILPOINT").as_deref() == Ok(point) {
        return Err(io::Error::other(format!("persistence failpoint: {point}")));
    }
    let _ = point;
    Ok(())
}

/// Return the largest prefix of `s` whose **byte length** is `<= n_bytes`
/// and which ends on a UTF-8 char boundary. If `n_bytes` already falls on
/// a boundary the prefix is exactly `n_bytes` bytes; otherwise the index
/// is walked down until the next valid boundary.
///
/// This is the safe replacement for `&s[..n_bytes]` when `n_bytes` was
/// derived from a byte budget (`s.len()`-based math) but `s` may contain
/// multi-byte code points.
pub(crate) fn char_safe_truncate(s: &str, n_bytes: usize) -> &str {
    if n_bytes >= s.len() {
        return s;
    }
    let mut end = n_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/util.rs"
    ));
}
