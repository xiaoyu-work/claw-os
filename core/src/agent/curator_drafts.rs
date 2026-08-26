//! Persistent store for curator-proposed skill drafts.
//!
//! `cos agent curator propose` produces an in-memory [`SkillDraft`]
//! snapshot per call. Without persistence those drafts vanish the
//! moment the process exits; that's fine for one-shot exploration
//! but gives the agent no way to revisit, accept, or reject them.
//!
//! This module wraps the drafts in a tiny single-file JSON store
//! (`data_dir/agent/curator-drafts.json`) with atomic rewrite
//! semantics. Every mutation rewrites the entire file via a
//! tmp+rename dance, so partial writes can never leave the store
//! corrupted.
//!
//! API surface mirrors a queue:
//!
//!   * [`DraftStore::open_default`] / [`DraftStore::open_at`] —
//!     load (creating an empty file if necessary)
//!   * [`DraftStore::list`] / [`DraftStore::get`] — read-only
//!     accessors over the in-memory mirror
//!   * [`DraftStore::add`] — append a new `Proposed` record from a
//!     fresh curator outcome
//!   * [`DraftStore::set_status`] — transition to
//!     [`DraftStatus::Accepted`] or [`DraftStatus::Rejected`]
//!   * [`DraftStore::delete`] — drop a record entirely
//!
//! Records carry a uuid v4 id (so callers can refer to them across
//! invocations), a created-at timestamp (ms since unix epoch), the
//! source `session_id`, the [`SkillDraft`] body, and an optional
//! free-form `note` set when the user accepts/rejects.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agent::curator::SkillDraft;

/// Status of a stored draft. Transitions are one-way: a Proposed
/// draft becomes Accepted or Rejected; Accepted/Rejected don't
/// transition back. Use `delete` if a draft was wrongly accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DraftStatus {
    Proposed,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftRecord {
    pub id: String,
    pub session_id: String,
    pub created_ts_ms: u64,
    pub status: DraftStatus,
    pub draft: SkillDraft,
    /// Optional free-form note attached at accept/reject time.
    pub note: Option<String>,
}

/// On-disk schema. Versioned so future incompatible changes can
/// surface a clean error rather than silently mis-parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct DraftFile {
    schema: u32,
    drafts: Vec<DraftRecord>,
}

impl Default for DraftFile {
    fn default() -> Self {
        Self {
            schema: 1,
            drafts: Vec::new(),
        }
    }
}

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
pub struct DraftStore {
    path: PathBuf,
    file: DraftFile,
}

impl DraftStore {
    /// Open or create the store at the cos-default location
    /// (`data_dir/agent/curator-drafts.json`). A non-existent file
    /// produces an empty store; a malformed file errors.
    pub fn open_default() -> Result<Self, String> {
        Self::open_at(crate::paths::agent_curator_drafts_path())
    }

    pub fn open_at(path: PathBuf) -> Result<Self, String> {
        let file = if path.exists() {
            let bytes = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
            if bytes.is_empty() {
                DraftFile::default()
            } else {
                let parsed: DraftFile = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("parse {}: {e}", path.display()))?;
                if parsed.schema != SCHEMA_VERSION {
                    return Err(format!(
                        "{} has schema v{}, expected v{}",
                        path.display(),
                        parsed.schema,
                        SCHEMA_VERSION
                    ));
                }
                parsed
            }
        } else {
            DraftFile::default()
        };
        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn list(&self) -> &[DraftRecord] {
        &self.file.drafts
    }

    pub fn get(&self, id: &str) -> Option<&DraftRecord> {
        self.file.drafts.iter().find(|r| r.id == id)
    }

    /// Append a new `Proposed` draft. Returns the assigned id.
    /// The new record is the *last* element of `list()` after the
    /// call.
    pub fn add(&mut self, session_id: String, draft: SkillDraft) -> Result<String, String> {
        let _lock = self.lock_rmw()?;
        // Re-read after taking the lock so concurrent writers can't
        // silently undo each other's additions (open_default + add
        // from two processes / threads otherwise saw the same baseline
        // snapshot and last writer won).
        self.reload_locked()?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let rec = DraftRecord {
            id: id.clone(),
            session_id,
            created_ts_ms: now_ms(),
            status: DraftStatus::Proposed,
            draft,
            note: None,
        };
        self.file.drafts.push(rec);
        self.save_atomic()?;
        Ok(id)
    }

    /// Mutate the status (and optionally the note) of an existing
    /// record. Returns `Err` when the id is unknown — callers should
    /// surface that to the user rather than silently no-op.
    pub fn set_status(
        &mut self,
        id: &str,
        status: DraftStatus,
        note: Option<String>,
    ) -> Result<(), String> {
        let _lock = self.lock_rmw()?;
        self.reload_locked()?;
        let rec = self
            .file
            .drafts
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no draft with id {id}"))?;
        rec.status = status;
        if note.is_some() {
            rec.note = note;
        }
        self.save_atomic()
    }

    /// Drop a record entirely. `Err` when the id is unknown.
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let _lock = self.lock_rmw()?;
        self.reload_locked()?;
        let before = self.file.drafts.len();
        self.file.drafts.retain(|r| r.id != id);
        if self.file.drafts.len() == before {
            return Err(format!("no draft with id {id}"));
        }
        self.save_atomic()
    }

    /// Replace the embedded [`SkillDraft::title`] of a stored
    /// record. Empty / whitespace-only titles are rejected — the
    /// curator pipeline guarantees a non-empty title at propose
    /// time, so callers should preserve that invariant when
    /// retitling. `Err` when the id is unknown.
    pub fn set_title(&mut self, id: &str, title: &str) -> Result<(), String> {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return Err("title must not be empty".into());
        }
        let _lock = self.lock_rmw()?;
        self.reload_locked()?;
        let rec = self
            .file
            .drafts
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no draft with id {id}"))?;
        rec.draft.title = trimmed.to_string();
        self.save_atomic()
    }

    /// Re-read the on-disk state into `self.file`. Must only be
    /// called while holding the rmw lock. Lets us avoid the
    /// classic open-mutate-save TOCTOU where a peer's update is
    /// silently overwritten because we acted on a stale baseline.
    fn reload_locked(&mut self) -> Result<(), String> {
        if !self.path.exists() {
            self.file = DraftFile::default();
            return Ok(());
        }
        let bytes =
            fs::read(&self.path).map_err(|e| format!("read {}: {e}", self.path.display()))?;
        if bytes.is_empty() {
            self.file = DraftFile::default();
            return Ok(());
        }
        let parsed: DraftFile = serde_json::from_slice(&bytes)
            .map_err(|e| format!("parse {}: {e}", self.path.display()))?;
        if parsed.schema != SCHEMA_VERSION {
            return Err(format!(
                "{} has schema v{}, expected v{}",
                self.path.display(),
                parsed.schema,
                SCHEMA_VERSION
            ));
        }
        self.file = parsed;
        Ok(())
    }

    /// Acquire an exclusive advisory flock on a sibling sentinel for
    /// the duration of a read-modify-write cycle. Save_atomic
    /// replaces the data file's inode, so locking the data file
    /// itself would be useless across rename — the sentinel inode is
    /// stable and serves as a valid sync point.
    fn lock_rmw(&self) -> Result<RmwLock, String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let lock_path = {
            let mut s: std::ffi::OsString = self.path.as_os_str().to_os_string();
            s.push(".lock");
            PathBuf::from(s)
        };
        let f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| format!("open lock {}: {e}", lock_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(format!(
                    "flock LOCK_EX {}: {}",
                    lock_path.display(),
                    io::Error::last_os_error()
                ));
            }
        }
        Ok(RmwLock { file: f })
    }

    /// Atomic write: serialise via the shared `atomic_write_with_fsync`
    /// helper (tmp + sync_all + rename + parent dir fsync). Worst-case
    /// crash leaves either the old contents (rename hadn't happened
    /// yet) or the new contents (rename + parent fsync succeeded) —
    /// never a torn or zero-byte file.
    fn save_atomic(&self) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(&self.file).map_err(|e| format!("serialise: {e}"))?;
        crate::agent::util::atomic_write_with_fsync(&self.path, &json)
            .map_err(|e| format!("write {}: {e}", self.path.display()))?;
        Ok(())
    }
}

/// RAII guard for the exclusive flock taken by [`DraftStore::lock_rmw`].
struct RmwLock {
    file: fs::File,
}

impl Drop for RmwLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/curator_drafts.rs"
    ));
}
