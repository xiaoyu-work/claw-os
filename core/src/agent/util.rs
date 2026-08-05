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
    // Per-process tmp suffix so two writers in different processes never
    // clobber each other's pending data. Nanos give us within-process
    // uniqueness even when called in a tight loop.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(
        ".{file_name}.{}.{nonce}.tmp",
        std::process::id()
    ));

    {
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp)?;
        f.write_all(data)?;
        // sync_all flushes data + metadata; sync_data would skip mtime
        // and similar but both are sufficient for our durability claim.
        f.sync_all()?;
    }

    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    crate::storage::set_private_file(path)?;

    // fsync the parent directory so the rename itself is durable.
    // Some filesystems (notably ext4 in `data=ordered`) can otherwise
    // lose the rename across an OS crash even though it committed the
    // file's data blocks. Best-effort on platforms that can't open a
    // directory for sync (very rare on unix; nop on Windows).
    sync_dir(parent);

    Ok(())
}

/// Open the directory and fsync it. Errors are swallowed: on platforms
/// that don't support directory fsync (e.g. some FUSE mounts, Windows)
/// we still want the file write to succeed.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
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
    use super::*;

    #[test]
    fn atomic_write_creates_and_replaces_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        atomic_write_with_fsync(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        atomic_write_with_fsync(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
    }

    #[test]
    fn atomic_write_leaves_no_tmp_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        atomic_write_with_fsync(&path, b"x").unwrap();
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                !name.ends_with(".tmp"),
                "no leftover tmp file expected, got {name}"
            );
        }
    }

    #[test]
    fn atomic_write_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("a").join("state.json");
        atomic_write_with_fsync(&path, b"hello").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn char_safe_truncate_ascii_keeps_n_bytes() {
        assert_eq!(char_safe_truncate("hello world", 5), "hello");
        assert_eq!(char_safe_truncate("hello", 100), "hello");
        assert_eq!(char_safe_truncate("", 10), "");
    }

    #[test]
    fn char_safe_truncate_walks_back_off_multibyte() {
        // "héllo" — 'é' is 2 bytes (0xC3 0xA9). Bytes: h(1) é(2) l(1) l(1) o(1) = 6.
        let s = "héllo";
        // Cut at byte 2 — middle of 'é'. Walk back to byte 1.
        assert_eq!(char_safe_truncate(s, 2), "h");
        // Cut at byte 3 — just past 'é'. Already on boundary.
        assert_eq!(char_safe_truncate(s, 3), "hé");
    }

    #[test]
    fn char_safe_truncate_handles_emoji() {
        // "hi 🌍" — emoji is 4 bytes. Total: h(1) i(1) ' '(1) 🌍(4) = 7.
        let s = "hi 🌍";
        assert_eq!(char_safe_truncate(s, 4), "hi "); // mid-emoji byte 4 walks back to 3
        assert_eq!(char_safe_truncate(s, 7), "hi 🌍");
        assert_eq!(char_safe_truncate(s, 100), "hi 🌍");
    }

    #[test]
    fn char_safe_truncate_zero_returns_empty() {
        assert_eq!(char_safe_truncate("anything", 0), "");
    }
}
