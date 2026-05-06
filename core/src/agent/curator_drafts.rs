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
use std::io::Write;
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
            let bytes = fs::read(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            if bytes.is_empty() {
                DraftFile::default()
            } else {
                let parsed: DraftFile = serde_json::from_slice(&bytes).map_err(|e| {
                    format!("parse {}: {e}", path.display())
                })?;
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
        let rec = self
            .file
            .drafts
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no draft with id {id}"))?;
        rec.draft.title = trimmed.to_string();
        self.save_atomic()
    }

    /// Atomic write: serialise to `<path>.tmp`, fsync, rename to
    /// `<path>`. Worst-case crash leaves either the old contents
    /// (if rename hadn't happened yet) or the new contents (rename
    /// succeeded) — never a torn file.
    fn save_atomic(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let json = serde_json::to_vec_pretty(&self.file)
            .map_err(|e| format!("serialise: {e}"))?;
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp)
                .map_err(|e| format!("create {}: {e}", tmp.display()))?;
            f.write_all(&json)
                .map_err(|e| format!("write {}: {e}", tmp.display()))?;
            f.sync_all().ok();
        }
        fs::rename(&tmp, &self.path)
            .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), self.path.display()))?;
        Ok(())
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
    use super::*;
    use crate::agent::curator::SkillConfidence;

    fn sample_draft(suggested_id: &str) -> SkillDraft {
        SkillDraft {
            suggested_id: suggested_id.to_string(),
            title: "demo".into(),
            description: "test draft".into(),
            allowed_tools: vec!["echo".into(), "now".into()],
            turns_used: 4,
            confidence: SkillConfidence::Medium,
        }
    }

    fn tmp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cos-curator-drafts-{label}-{}.json",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn open_at_nonexistent_returns_empty_store() {
        let p = tmp_path("nonexistent");
        let store = DraftStore::open_at(p.clone()).expect("open");
        assert!(store.list().is_empty());
        // open should NOT create the file until the first write.
        assert!(!p.exists());
    }

    #[test]
    fn add_persists_and_reload_roundtrips() {
        let p = tmp_path("roundtrip");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let id = store
            .add("sess-1".into(), sample_draft("first"))
            .expect("add");
        assert_eq!(store.list().len(), 1);

        let reopened = DraftStore::open_at(p.clone()).unwrap();
        assert_eq!(reopened.list().len(), 1);
        let got = reopened.get(&id).expect("present");
        assert_eq!(got.session_id, "sess-1");
        assert_eq!(got.draft.suggested_id, "first");
        assert_eq!(got.status, DraftStatus::Proposed);
        assert!(got.note.is_none());

        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn add_assigns_unique_ids() {
        let p = tmp_path("unique-ids");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let a = store.add("s".into(), sample_draft("a")).unwrap();
        let b = store.add("s".into(), sample_draft("b")).unwrap();
        let c = store.add("s".into(), sample_draft("c")).unwrap();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn set_status_transitions_and_persists() {
        let p = tmp_path("status");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let id = store.add("s".into(), sample_draft("x")).unwrap();
        store
            .set_status(&id, DraftStatus::Accepted, Some("looks good".into()))
            .expect("ok");
        let reopened = DraftStore::open_at(p.clone()).unwrap();
        let r = reopened.get(&id).unwrap();
        assert_eq!(r.status, DraftStatus::Accepted);
        assert_eq!(r.note.as_deref(), Some("looks good"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn set_status_unknown_id_errors() {
        let p = tmp_path("unknown-status");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let err = store
            .set_status("nope", DraftStatus::Accepted, None)
            .unwrap_err();
        assert!(err.contains("no draft"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn delete_removes_and_persists() {
        let p = tmp_path("delete");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let id = store.add("s".into(), sample_draft("d")).unwrap();
        store.delete(&id).expect("ok");
        assert!(store.list().is_empty());
        let reopened = DraftStore::open_at(p.clone()).unwrap();
        assert!(reopened.list().is_empty());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn delete_unknown_id_errors() {
        let p = tmp_path("unknown-delete");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let err = store.delete("nope").unwrap_err();
        assert!(err.contains("no draft"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn empty_file_is_treated_as_empty_store() {
        let p = tmp_path("empty");
        std::fs::write(&p, b"").unwrap();
        let store = DraftStore::open_at(p.clone()).expect("open");
        assert!(store.list().is_empty());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn malformed_file_returns_error() {
        let p = tmp_path("malformed");
        std::fs::write(&p, b"{this is not json").unwrap();
        let err = DraftStore::open_at(p.clone()).unwrap_err();
        assert!(err.contains("parse"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn schema_mismatch_returns_error() {
        let p = tmp_path("schema-bad");
        std::fs::write(&p, br#"{"schema":99,"drafts":[]}"#).unwrap();
        let err = DraftStore::open_at(p.clone()).unwrap_err();
        assert!(err.contains("schema"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn note_is_optional_and_skipped_when_none_passed() {
        let p = tmp_path("note-none");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let id = store.add("s".into(), sample_draft("n")).unwrap();
        store
            .set_status(&id, DraftStatus::Accepted, Some("first".into()))
            .unwrap();
        // Second transition with note=None should keep the prior note.
        store
            .set_status(&id, DraftStatus::Rejected, None)
            .unwrap();
        let r = store.get(&id).unwrap();
        assert_eq!(r.status, DraftStatus::Rejected);
        assert_eq!(r.note.as_deref(), Some("first"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn save_atomic_uses_tmp_file_suffix() {
        let p = tmp_path("atomic");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        store.add("s".into(), sample_draft("a")).unwrap();
        // tmp file must NOT linger after a successful save.
        let tmp = p.with_extension("json.tmp");
        assert!(!tmp.exists(), "tmp file should be renamed away");
        assert!(p.exists());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn set_title_updates_embedded_skill_draft_title() {
        let p = tmp_path("retitle-ok");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let id = store.add("s".into(), sample_draft("a")).unwrap();
        store.set_title(&id, "Brand-New Title").unwrap();
        let r = store.get(&id).unwrap();
        assert_eq!(r.draft.title, "Brand-New Title");
        // Reload from disk to confirm it persisted.
        let store2 = DraftStore::open_at(p.clone()).unwrap();
        assert_eq!(store2.get(&id).unwrap().draft.title, "Brand-New Title");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn set_title_trims_whitespace() {
        let p = tmp_path("retitle-trim");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let id = store.add("s".into(), sample_draft("b")).unwrap();
        store.set_title(&id, "   Padded Title   ").unwrap();
        assert_eq!(store.get(&id).unwrap().draft.title, "Padded Title");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn set_title_rejects_empty_or_whitespace() {
        let p = tmp_path("retitle-empty");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let id = store.add("s".into(), sample_draft("c")).unwrap();
        let err = store.set_title(&id, "").unwrap_err();
        assert!(err.contains("must not be empty"));
        let err = store.set_title(&id, "   \t\n").unwrap_err();
        assert!(err.contains("must not be empty"));
        // Original title preserved.
        assert_eq!(store.get(&id).unwrap().draft.title, "demo");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn set_title_unknown_id_errors() {
        let p = tmp_path("retitle-unknown");
        let mut store = DraftStore::open_at(p.clone()).unwrap();
        let err = store.set_title("does-not-exist", "anything").unwrap_err();
        assert!(err.contains("no draft with id"));
        std::fs::remove_file(&p).ok();
    }
}
