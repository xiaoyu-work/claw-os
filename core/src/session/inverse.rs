//! Inverse blob store — content-addressed-ish storage for the bytes a
//! mutation needs to undo itself.
//!
//! When a session records `Mutation::FsWrite { path, prev_blob:
//! Some(id) }`, the actual previous bytes live in
//! `<session_dir>/files/inverse/<blob_id>.bin`. This module owns that
//! directory: writers append blobs, the rollback engine reads them
//! back, GC drops them when the session is archived.
//!
//! ## Why not put the bytes inline in `mutations.jsonl`
//!
//! - JSONL is grep-friendly only when individual lines stay small.
//! - Encoding raw bytes as base64 inside JSON inflates them ~33% AND
//!   forces every reader to decode every line just to skim the log.
//! - Sidecar files let us mmap or stream the blob during rollback
//!   instead of holding a giant `Vec<u8>` in memory.
//!
//! ## Blob IDs
//!
//! `blob_id` is a UUIDv4 hex (no dashes, lowercase). We considered
//! content-addressing via SHA-256 to dedup repeated writes of the
//! same bytes, but that bought us little (rollback is per-session and
//! the bytes are usually unique anyway) at the cost of needing
//! reference counting for safe blob GC. Pre-1.0 we ship the simple
//! version; dedup can land later as a transparent optimization.
//!
//! ## Layout
//!
//! ```text
//! <session_dir>/
//!   files/
//!     inverse/
//!       0a1b2c…ef.bin    # raw bytes, no header, no JSON wrapper
//! ```

use std::fs;
use std::path::PathBuf;

use uuid::Uuid;

use super::id::SessionId;
use super::store::{session_dir, SessionError};

/// Directory holding all inverse blobs for a session. Created lazily
/// on first write.
pub fn inverse_root(sid: &SessionId) -> PathBuf {
    session_dir(sid).join("files").join("inverse")
}

/// Resolve a `blob_id` to its file path. Does not check existence.
pub fn blob_path(sid: &SessionId, blob_id: &str) -> PathBuf {
    inverse_root(sid).join(format!("{blob_id}.bin"))
}

/// Generate a fresh blob id. UUIDv4 hex (32 chars, lowercase, no
/// dashes) — short enough to grep, long enough to never collide.
pub fn new_blob_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// Write `bytes` as a new inverse blob and return its id. Creates
/// `files/inverse/` on demand. Errors propagate as
/// [`SessionError::Io`] so callers can mix this with the rest of the
/// store API without an extra error type.
pub fn write_blob(sid: &SessionId, bytes: &[u8]) -> Result<String, SessionError> {
    let dir = inverse_root(sid);
    fs::create_dir_all(&dir).map_err(|e| SessionError::io(dir.clone(), e))?;

    // Try a few times in the astronomically unlikely case of a UUID
    // collision (existing files we'd otherwise clobber).
    for _ in 0..4 {
        let id = new_blob_id();
        let path = dir.join(format!("{id}.bin"));
        if path.exists() {
            continue;
        }
        // Atomic-ish: write to .tmp then rename. We don't need
        // filelock here because the filename is unique per blob.
        let tmp = dir.join(format!("{id}.bin.tmp"));
        fs::write(&tmp, bytes).map_err(|e| SessionError::io(tmp.clone(), e))?;
        fs::rename(&tmp, &path).map_err(|e| SessionError::io(path.clone(), e))?;
        return Ok(id);
    }
    Err(SessionError::Lock(
        "uuid collision four times in a row — the universe is broken".into(),
    ))
}

/// Read an inverse blob's bytes back. Returns
/// [`SessionError::NotFound`] if the blob was never written or was
/// already GC'd.
pub fn read_blob(sid: &SessionId, blob_id: &str) -> Result<Vec<u8>, SessionError> {
    let path = blob_path(sid, blob_id);
    if !path.exists() {
        return Err(SessionError::NotFound(format!(
            "inverse blob {blob_id} for {sid}"
        )));
    }
    fs::read(&path).map_err(|e| SessionError::io(path.clone(), e))
}

/// Delete a blob. Used by GC and (in tests) by direct cleanup. Silent
/// no-op if the blob does not exist.
pub fn delete_blob(sid: &SessionId, blob_id: &str) -> Result<(), SessionError> {
    let path = blob_path(sid, blob_id);
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(&path).map_err(|e| SessionError::io(path.clone(), e))
}
