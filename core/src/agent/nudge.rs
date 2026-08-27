//! Periodic "nudge" reminders surfaced to the agent.
//!
//! The runtime checks [`NudgeStore::due`] each turn (or on schedule) and adds
//! due reminders to request-local user context. Nudges therefore remain timely
//! without invalidating the session's frozen canonical system prompt.
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/nudge.rs"
    ));
}
