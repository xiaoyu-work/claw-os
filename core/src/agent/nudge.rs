//! Periodic "nudge" reminders surfaced to the agent.
//!
//! The runtime checks [`NudgeStore::due`] each turn (or on
//! schedule) and prepends any due reminders into the system
//! prompt, so the agent is gently reminded of follow-ups,
//! deferred tasks, or user-supplied prompts at the right
//! cadence.
//!
//! Storage is JSON on-disk (one file in the agent state
//! directory). Library-only; the runtime decides how to surface
//! due nudges.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Nudge {
    pub id: String,
    pub message: String,
    /// Unix epoch seconds when this nudge becomes due.
    pub due_at_epoch_s: u64,
    /// Optional repeat interval in seconds. When `None`, the nudge
    /// is one-shot and self-deletes after firing once.
    pub repeat_secs: Option<u64>,
    /// Free-form tag for routing (e.g. "calendar", "follow-up",
    /// "user-prompt"). Helps the runtime decide where to surface.
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub last_fired_epoch_s: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct NudgeFile {
    nudges: BTreeMap<String, Nudge>,
}

pub struct NudgeStore {
    path: PathBuf,
}

impl NudgeStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> NudgeFile {
        match fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => NudgeFile::default(),
        }
    }

    fn save(&self, file: &NudgeFile) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(file)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Crash-safe: per-process tmp + fsync(tmp) + rename + fsync(parent).
        // Replaces a previous `fs::write(tmp) + fs::rename` which skipped
        // both fsyncs and could surface a torn/empty nudges file on
        // recovery, dropping every queued reminder.
        crate::agent::util::atomic_write_with_fsync(&self.path, &json)
    }

    /// Acquire an exclusive advisory lock for the duration of a
    /// read-modify-write cycle. The lock attaches to a sibling
    /// `.lock` sentinel — locking the data file directly would be
    /// useless because `save_atomic` swaps its inode on each write.
    /// On non-unix we return Ok with no lock (best-effort).
    fn lock_rmw(&self) -> io::Result<RmwLock> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
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
            .open(&lock_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(RmwLock { file: f })
    }

    /// Add a nudge. Returns the assigned id (caller may have
    /// supplied one via `nudge.id`; if blank, a UUID is assigned).
    pub fn add(&self, mut nudge: Nudge) -> io::Result<String> {
        if nudge.id.is_empty() {
            nudge.id = Uuid::new_v4().simple().to_string();
        }
        let _lock = self.lock_rmw()?;
        let mut file = self.load();
        let id = nudge.id.clone();
        file.nudges.insert(id.clone(), nudge);
        self.save(&file)?;
        Ok(id)
    }

    pub fn remove(&self, id: &str) -> io::Result<bool> {
        let _lock = self.lock_rmw()?;
        let mut file = self.load();
        let removed = file.nudges.remove(id).is_some();
        if removed {
            self.save(&file)?;
        }
        Ok(removed)
    }

    pub fn list(&self) -> Vec<Nudge> {
        let file = self.load();
        file.nudges.into_values().collect()
    }

    /// Return all nudges that are due as of `now_epoch_s`. Does
    /// not mutate state — call [`fire`] after surfacing them so
    /// repeat counters advance and one-shots self-delete.
    pub fn due(&self, now_epoch_s: u64) -> Vec<Nudge> {
        self.load()
            .nudges
            .into_values()
            .filter(|n| n.due_at_epoch_s <= now_epoch_s)
            .collect()
    }

    /// Mark `id` as fired at `now_epoch_s`.
    ///
    /// * One-shot (no repeat) → deleted.
    /// * Repeating → due_at advances to `max(now, due) + repeat`,
    ///   `last_fired_epoch_s` set.
    ///
    /// Returns true if the nudge existed and was updated/removed.
    pub fn fire(&self, id: &str, now_epoch_s: u64) -> io::Result<bool> {
        let _lock = self.lock_rmw()?;
        let mut file = self.load();
        let Some(nudge) = file.nudges.get_mut(id) else {
            return Ok(false);
        };
        match nudge.repeat_secs {
            None => {
                file.nudges.remove(id);
            }
            Some(repeat) => {
                let base = nudge.due_at_epoch_s.max(now_epoch_s);
                nudge.due_at_epoch_s = base.saturating_add(repeat);
                nudge.last_fired_epoch_s = Some(now_epoch_s);
            }
        }
        self.save(&file)?;
        Ok(true)
    }
}

/// RAII guard for the exclusive flock taken by `lock_rmw`. Releases
/// on drop via `flock(LOCK_UN)`; closing the fd would do the same but
/// being explicit makes the lock lifetime obvious in profiles.
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

/// Helper: current unix epoch seconds. Returns 0 if the system
/// clock reports a time before UNIX_EPOCH (shouldn't happen).
pub fn now_epoch_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cos-nudge-{label}-{}.json",
            Uuid::new_v4().simple()
        ))
    }

    fn n(message: &str, due: u64, repeat: Option<u64>) -> Nudge {
        Nudge {
            id: String::new(),
            message: message.to_string(),
            due_at_epoch_s: due,
            repeat_secs: repeat,
            tag: None,
            last_fired_epoch_s: None,
        }
    }

    #[test]
    fn add_assigns_uuid_if_blank() {
        let p = tmp("uuid");
        let store = NudgeStore::new(&p);
        let id = store.add(n("hello", 100, None)).unwrap();
        assert!(!id.is_empty());
        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn add_keeps_supplied_id() {
        let p = tmp("supplied-id");
        let store = NudgeStore::new(&p);
        let nudge = Nudge {
            id: "my-nudge".to_string(),
            message: "x".to_string(),
            due_at_epoch_s: 0,
            repeat_secs: None,
            tag: None,
            last_fired_epoch_s: None,
        };
        let id = store.add(nudge).unwrap();
        assert_eq!(id, "my-nudge");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn due_filters_by_time() {
        let p = tmp("due");
        let store = NudgeStore::new(&p);
        store.add(n("past", 100, None)).unwrap();
        store.add(n("future", 9_999_999_999, None)).unwrap();
        let now = 200u64;
        let due = store.due(now);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].message, "past");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn fire_one_shot_deletes() {
        let p = tmp("oneshot");
        let store = NudgeStore::new(&p);
        let id = store.add(n("x", 100, None)).unwrap();
        let ok = store.fire(&id, 200).unwrap();
        assert!(ok);
        assert!(store.list().is_empty());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn fire_repeating_advances_due() {
        let p = tmp("repeat");
        let store = NudgeStore::new(&p);
        let id = store.add(n("ping", 100, Some(60))).unwrap();
        store.fire(&id, 200).unwrap();
        let listed = store.list();
        assert_eq!(listed.len(), 1);
        // Base = max(100, 200) = 200; new due = 260.
        assert_eq!(listed[0].due_at_epoch_s, 260);
        assert_eq!(listed[0].last_fired_epoch_s, Some(200));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn fire_unknown_returns_false() {
        let p = tmp("unknown");
        let store = NudgeStore::new(&p);
        let ok = store.fire("nope", 100).unwrap();
        assert!(!ok);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn remove_returns_true_only_if_existed() {
        let p = tmp("remove");
        let store = NudgeStore::new(&p);
        let id = store.add(n("x", 100, None)).unwrap();
        assert!(store.remove(&id).unwrap());
        assert!(!store.remove(&id).unwrap());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn list_empty_when_file_missing() {
        let p = tmp("missing");
        let store = NudgeStore::new(&p);
        assert!(store.list().is_empty());
    }

    #[test]
    fn save_atomic_via_tmp_rename() {
        let p = tmp("atomic");
        let store = NudgeStore::new(&p);
        store.add(n("x", 100, None)).unwrap();
        // No per-process tmp file should linger after a successful save.
        // The shared atomic_write helper uses a hidden `.<name>.<pid>...tmp`
        // sibling and renames it into place. Restrict the scan to this
        // test's stem so we don't false-positive on `.tmp` files left
        // by other concurrently-running tests sharing `/tmp`.
        let stem = p.file_name().unwrap().to_string_lossy().into_owned();
        if let Some(parent) = p.parent() {
            for entry in fs::read_dir(parent).unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.contains(&stem) {
                    continue;
                }
                assert!(
                    !name.ends_with(".tmp"),
                    "no leftover tmp file expected for {stem}, got {name}"
                );
            }
        }
        assert!(p.exists());
        let _ = fs::remove_file(&p);
        let lock = p.with_file_name(format!(
            "{}.lock",
            p.file_name().unwrap().to_string_lossy()
        ));
        let _ = fs::remove_file(&lock);
    }

    #[test]
    fn now_epoch_s_is_recent() {
        let n = now_epoch_s();
        // After 2025-01-01.
        assert!(n > 1_735_689_600);
    }

    #[test]
    fn repeat_with_due_in_future_uses_due_as_base() {
        let p = tmp("repeat-future");
        let store = NudgeStore::new(&p);
        let id = store.add(n("ping", 1000, Some(60))).unwrap();
        // Fire when current time is BEFORE the due time.
        store.fire(&id, 500).unwrap();
        let listed = store.list();
        // base = max(1000, 500) = 1000; new due = 1060.
        assert_eq!(listed[0].due_at_epoch_s, 1060);
        fs::remove_file(&p).ok();
    }
}
