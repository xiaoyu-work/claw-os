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
    atomic_write_with_fsync_and_prepare(path, data, |_| Ok(()))
}

pub(crate) fn atomic_write_with_fsync_and_prepare<F>(
    path: &Path,
    data: &[u8],
    prepare: F,
) -> io::Result<()>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent dir"))?;
    ensure_durable_dir(parent, false)?;

    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_string_lossy()
        .into_owned();
    // An unpredictable suffix plus create_new prevents concurrent writers
    // from ever sharing or truncating the same staging inode.
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let tmp = parent.join(format!(".{file_name}.{}.{nonce}.tmp", std::process::id()));

    let staged = (|| {
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
        f.sync_all()
    })();
    if let Err(error) = staged {
        match fs::remove_file(&tmp) {
            Ok(()) => sync_dir(parent)?,
            Err(cleanup) if cleanup.kind() != io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    cleanup.kind(),
                    format!("staging failed: {error}; temporary cleanup failed: {cleanup}"),
                ));
            }
            Err(_) => {}
        }
        return Err(error);
    }

    if let Err(error) = prepare(&tmp) {
        match fs::remove_file(&tmp) {
            Ok(()) => sync_dir(parent)?,
            Err(cleanup) if cleanup.kind() != io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    cleanup.kind(),
                    format!("prepare failed: {error}; temporary cleanup failed: {cleanup}"),
                ));
            }
            Err(_) => {}
        }
        return Err(error);
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
    let visible_result = (|| {
        persistence_barrier("after_rename")?;
        crate::storage::set_private_file(path)?;
        sync_dir(parent)
    })();
    visible_result.map_err(visible_persistence_error)
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

/// Create a private directory hierarchy and durably commit every new dirent.
///
/// The nearest pre-existing ancestor is fsynced before any mutation to prove
/// the filesystem supports directory durability. Each child is then created
/// one component at a time, protected, fsynced, and followed by an fsync of
/// its parent. Returning success therefore means a reboot cannot erase only
/// part of the hierarchy.
pub(crate) fn ensure_durable_private_dir(path: &Path) -> io::Result<()> {
    ensure_durable_dir(path, true)
}

fn ensure_durable_dir(path: &Path, private: bool) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut ancestor = path;
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "durable directory ancestor is not a real directory: {}",
                            ancestor.display()
                        ),
                    ));
                }
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "directory path has no existing ancestor",
                            )
                        })?
                        .to_os_string(),
                );
                ancestor = ancestor.parent().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory path has no existing ancestor",
                    )
                })?;
            }
            Err(error) => return Err(error),
        }
    }

    sync_dir(ancestor)?;
    let mut current = ancestor.to_path_buf();
    for component in missing.iter().rev() {
        current.push(component);
        persistence_barrier("dir_create")?;
        match fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&current)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "directory creation raced with unsafe path: {}",
                            current.display()
                        ),
                    ));
                }
            }
            Err(error) => return Err(error),
        }
        if private {
            set_private_dir_mode(&current)?;
        }
        persistence_barrier("after_dir_create")?;
        sync_dir(&current)?;
        let parent = current.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "created directory has no parent",
            )
        })?;
        sync_dir(parent)?;
    }
    if private {
        set_private_dir_mode(path)?;
    }
    sync_dir(path)
}

fn set_private_dir_mode(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
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

fn visible_persistence_error(error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "durability is indeterminate after a visible filesystem mutation; do not retry: {error}"
        ),
    )
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
