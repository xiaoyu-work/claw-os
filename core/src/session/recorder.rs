//! Typed mutation recorders.
//!
//! These are the helpers gated apps call when they're about to touch
//! disk on behalf of an attached session. Each helper:
//!
//! 1. Snapshots whatever currently exists at the target (file bytes,
//!    "did the path exist?", credential value, …).
//! 2. Writes the snapshot into [`super::inverse`] and gets a
//!    `blob_id` back.
//! 3. Records the matching [`Mutation`] variant via
//!    [`super::store::record_mutation`].
//!
//! The actual mutation (writing the new bytes, deleting the file,
//! whatever) is the caller's job — these helpers are pure
//! "remember enough to undo me later". Splitting the two means the
//! recorder can't accidentally mask a write failure with a snapshot
//! failure.
//!
//! ## Why typed helpers and not "snapshot any path you like"
//!
//! The Python `claw-os-sdk/python/src/claw_os_sdk/snapshot.py` module already does the
//! "copy whatever you point at" pattern via `<COS_DATA_DIR>/trash/`.
//! That works for a CLI undo, but we want the durable session log to
//! be self-describing: a future GUI showing "this session deleted 3
//! files and renamed 1" should be able to render that from
//! `mutations.jsonl` alone, with no Python snapshot directory required
//! to be reachable. Hence: each verb gets its own typed variant.
//!
//! Phase 3 covers `fs.write` / `fs.delete` / `fs.rename`. Credential
//! variants exist in the [`Mutation`] enum but recorders for them
//! land alongside Phase 4's credential namespace work.

use std::fs;
use std::path::Path;

use super::id::SessionId;
use super::inverse;
use super::mutation::{Mutation, MutationRecord};
use super::store::{self, SessionError};

/// Record a pending `fs.write` to `path`. Reads the file's current
/// bytes (if any) into an inverse blob, then writes a
/// [`Mutation::FsWrite`] record. Caller writes the new contents to
/// `path` after this returns.
///
/// Returns the seq the mutation got in `mutations.jsonl`. Useful for
/// logging or for callers that want to correlate a turn with a
/// mutation.
///
/// `path` is stored verbatim in the log — pass an absolute path so a
/// future rollback in a different cwd still resolves correctly.
pub fn record_fs_write(sid: &SessionId, path: &Path) -> Result<u64, SessionError> {
    let path_str = path.to_string_lossy().into_owned();

    let prev_blob = if path.exists() && !path.is_dir() {
        let bytes = fs::read(path).map_err(|e| SessionError::io(path.to_path_buf(), e))?;
        Some(inverse::write_blob(sid, &bytes)?)
    } else {
        None
    };

    let rec = MutationRecord::new(Mutation::FsWrite {
        path: path_str,
        prev_blob,
    });
    store::record_mutation(sid, rec)
}

/// Record a pending `fs.delete` of `path`. Snapshots the bytes
/// unconditionally — a delete with nothing to restore is a bug
/// upstream (the gated verb should have rejected it). Returns the
/// mutation seq.
///
/// Directories are not yet supported (would need recursive snapshot);
/// returns [`SessionError::NotFound`] for now to surface the
/// mismatch loudly rather than silently lose the subtree.
pub fn record_fs_delete(sid: &SessionId, path: &Path) -> Result<u64, SessionError> {
    let path_str = path.to_string_lossy().into_owned();

    if !path.exists() {
        return Err(SessionError::NotFound(format!(
            "fs.delete recorder: {} does not exist",
            path.display()
        )));
    }
    if path.is_dir() {
        return Err(SessionError::Lock(format!(
            "fs.delete recorder: refusing to snapshot directory {} (use a directory-aware variant)",
            path.display()
        )));
    }

    let bytes = fs::read(path).map_err(|e| SessionError::io(path.to_path_buf(), e))?;
    let blob_id = inverse::write_blob(sid, &bytes)?;

    let rec = MutationRecord::new(Mutation::FsDelete {
        path: path_str,
        blob_id,
    });
    store::record_mutation(sid, rec)
}

/// Record a pending `fs.rename` from `from` to `to`. No bytes are
/// snapshotted — the inverse is just the opposite rename, replayed by
/// [`super::rollback`].
///
/// Records the mutation even if `from` does not exist; the rollback
/// engine will surface the mismatch at replay time. We do this
/// because the gated verb may be about to fail on its own; we'd
/// rather over-record than miss a record.
pub fn record_fs_rename(sid: &SessionId, from: &Path, to: &Path) -> Result<u64, SessionError> {
    let rec = MutationRecord::new(Mutation::FsRename {
        from: from.to_string_lossy().into_owned(),
        to: to.to_string_lossy().into_owned(),
    });
    store::record_mutation(sid, rec)
}
