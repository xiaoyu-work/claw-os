//! Garbage-collect terminal sessions.
//!
//! After a session reaches `Done` or `Failed` its directory is still
//! useful for forensics (Why did the agent do that? Show me the turn
//! log.) but it should not pile up indefinitely. `gc_archive` zips up
//! every terminal session older than the caller-supplied threshold and
//! moves it into `<sessions_root>/.archive/<sid>.zip`, then deletes the
//! original directory.
//!
//! Why zip, not tar.zst: the workspace already depends on `zip` (with
//! the `zstd` feature) for [`crate::engine_pkg`] and
//! [`crate::agent::skills::sync`]. Reusing it avoids pulling `tar` +
//! `zstd` crates for what amounts to a few small JSON / JSONL files
//! per session.
//!
//! ## Atomicity
//!
//! Each session is archived independently. The sequence per session is:
//!
//! 1. Write the zip to `<.archive>/<sid>.zip.tmp` (any pre-existing
//!    tmp from a crashed prior run is overwritten).
//! 2. `rename` to `<.archive>/<sid>.zip` — atomic on POSIX.
//! 3. `remove_dir_all` the original session dir.
//!
//! If step 1 or 2 fails the original is untouched, the next run
//! retries. If step 3 fails the zip is already in `.archive/` and the
//! original dir lingers; the next gc run will see the active dir, find
//! a matching archive, and re-attempt the delete. That self-heals.
//!
//! ## What this does **not** do
//!
//! - It does not enforce a lease (Phase 2). A terminal session has no
//!   lease holder by definition.
//! - It does not delete archived zips. Retention policy beyond the
//!   first archive boundary is left to system tooling (logrotate,
//!   cron, manual cleanup).
//! - It does not read archives back — that's a separate concern that
//!   lands when a UI surface needs `cos agent show <archived-sid>`.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::id::SessionId;
use super::store::{self, SessionError};

const ARCHIVE_DIRNAME: &str = ".archive";

/// Summary of one `gc_archive` run. Stringly-typed errors so callers
/// can log / serialize without dragging in the error enum.
#[derive(Debug, Default)]
pub struct GcStats {
    /// Session ids that were successfully zipped + removed.
    pub archived: Vec<SessionId>,
    /// Sessions whose status is still active (not eligible).
    pub skipped_active: usize,
    /// Sessions whose `ended_at` is missing or younger than the
    /// caller-supplied threshold.
    pub skipped_too_recent: usize,
    /// Per-session errors. We keep going on individual failures —
    /// one corrupt session must not block the rest.
    pub errors: Vec<(SessionId, String)>,
}

/// Root for archived session zips, e.g. `/var/lib/cos/sessions/.archive`.
pub fn archive_root() -> PathBuf {
    store::sessions_root().join(ARCHIVE_DIRNAME)
}

/// Path the archived zip for `sid` would live at, regardless of whether
/// it currently exists.
pub fn archive_path(sid: &SessionId) -> PathBuf {
    archive_root().join(format!("{}.zip", sid.as_str()))
}

/// `true` iff `<.archive>/<sid>.zip` is a regular file on disk.
pub fn is_archived(sid: &SessionId) -> bool {
    archive_path(sid).is_file()
}

/// Archive every terminal session whose `ended_at` is older than
/// `min_age`. Returns a [`GcStats`] summarizing the run.
///
/// `min_age` is the **minimum** time since `ended_at`; pass
/// `Duration::ZERO` to archive every terminal session unconditionally
/// (useful in tests).
///
/// Active sessions are skipped. Sessions whose meta cannot be parsed
/// are skipped silently (matching `store::list`). The archive root is
/// created on demand.
pub fn gc_archive(min_age: Duration) -> Result<GcStats, SessionError> {
    let metas = store::list()?;
    let now = SystemTime::now();
    let mut stats = GcStats::default();

    if metas.is_empty() {
        return Ok(stats);
    }

    let archive_dir = archive_root();
    fs::create_dir_all(&archive_dir)
        .map_err(|e| SessionError::Io { path: archive_dir.clone(), source: e })?;

    for meta in metas {
        let sid = meta.id.clone();

        if meta.status.is_active() {
            stats.skipped_active += 1;
            continue;
        }

        let eligible = match meta.ended_at.as_deref().and_then(parse_rfc3339) {
            Some(ended) => now
                .duration_since(ended)
                .map(|age| age >= min_age)
                .unwrap_or(false),
            None => false,
        };
        if !eligible {
            stats.skipped_too_recent += 1;
            continue;
        }

        match archive_one(&sid) {
            Ok(()) => stats.archived.push(sid),
            Err(e) => stats.errors.push((sid, e.to_string())),
        }
    }

    Ok(stats)
}

/// Zip a single session directory into `.archive/<sid>.zip` and remove
/// the original. Idempotent at the dir-level: if a zip is already
/// present and the dir is gone, this is a no-op style success path,
/// but we still treat the missing dir as an error so callers can
/// notice — `gc_archive` filters that path out via the meta walk.
fn archive_one(sid: &SessionId) -> Result<(), SessionError> {
    let dir = store::session_dir(sid);
    if !dir.exists() {
        return Err(SessionError::NotFound(sid.as_str().to_string()));
    }

    let final_path = archive_path(sid);
    let tmp_path = final_path.with_extension("zip.tmp");

    if tmp_path.exists() {
        let _ = fs::remove_file(&tmp_path);
    }

    write_zip(&dir, &tmp_path)?;

    fs::rename(&tmp_path, &final_path).map_err(|e| SessionError::Io {
        path: final_path.clone(),
        source: e,
    })?;

    fs::remove_dir_all(&dir).map_err(|e| SessionError::Io { path: dir, source: e })?;
    Ok(())
}

/// Build a zip of `src_dir` at `dst`. Entries are stored with paths
/// relative to `src_dir` (forward slashes, per the zip spec).
///
/// Compression: zstd if available (the crate feature is enabled in
/// `core/Cargo.toml`), else deflate. We could specialize per-file but
/// session payloads are tiny — a uniform method keeps the code small.
fn write_zip(src_dir: &Path, dst: &Path) -> Result<(), SessionError> {
    let file = File::create(dst).map_err(|e| SessionError::Io {
        path: dst.to_path_buf(),
        source: e,
    })?;
    let mut zw = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Zstd);

    walk_into_zip(src_dir, src_dir, &mut zw, opts)?;

    zw.finish().map_err(|e| SessionError::Io {
        path: dst.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    })?;
    Ok(())
}

fn walk_into_zip(
    root: &Path,
    cur: &Path,
    zw: &mut ZipWriter<File>,
    opts: SimpleFileOptions,
) -> Result<(), SessionError> {
    let entries = fs::read_dir(cur).map_err(|e| SessionError::Io {
        path: cur.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| SessionError::Io {
            path: cur.to_path_buf(),
            source: e,
        })?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .expect("walk entries are descendants of root")
            .to_string_lossy()
            .replace('\\', "/");

        let ft = entry.file_type().map_err(|e| SessionError::Io {
            path: path.clone(),
            source: e,
        })?;

        if ft.is_dir() {
            zw.add_directory(format!("{}/", rel), opts)
                .map_err(|e| SessionError::Io {
                    path: path.clone(),
                    source: std::io::Error::other(e.to_string()),
                })?;
            walk_into_zip(root, &path, zw, opts)?;
        } else if ft.is_file() {
            zw.start_file(&rel, opts).map_err(|e| SessionError::Io {
                path: path.clone(),
                source: std::io::Error::other(e.to_string()),
            })?;
            let mut buf = Vec::new();
            File::open(&path)
                .and_then(|mut f| f.read_to_end(&mut buf))
                .map_err(|e| SessionError::Io {
                    path: path.clone(),
                    source: e,
                })?;
            zw.write_all(&buf).map_err(|e| SessionError::Io {
                path: path.clone(),
                source: e,
            })?;
        }
        // Symlinks inside a session dir are not expected; skip silently
        // to avoid following one out of the session sandbox.
    }
    Ok(())
}

fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let dt = DateTime::parse_from_rfc3339(s).ok()?;
    let secs = dt.timestamp();
    if secs < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/gc.rs"
    ));
}
