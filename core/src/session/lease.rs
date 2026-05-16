//! Cross-process lease for a durable session.
//!
//! At most one process at a time may write to a given session
//! (`turns.jsonl`, `mutations.jsonl`, `caps.json`, `state.json`). This
//! module enforces that with an advisory `flock(LOCK_EX)` on a
//! sentinel file `<session_dir>/lease.lock`. The data file
//! `<session_dir>/lease.json` records *who* currently holds it for
//! display purposes.
//!
//! ## Why a separate sentinel
//!
//! - Readers (`cos agent ls`, future GUI) want to know the current
//!   holder without touching any lock; they just read `lease.json`.
//! - Writers (the lease holder) need to update `lease.json` for
//!   heartbeats; doing that under flock on `lease.json` itself would
//!   force every reader to compete with heartbeats.
//! - `flock(2)` is auto-released by the kernel on process death (any
//!   exit path: clean, panic, segfault, `kill -9`, OOM-kill). That
//!   gives us "lease holder died → next acquire just works" with zero
//!   timeout logic.
//!
//! ## Source-of-truth split
//!
//! - `lease.lock` flock → *am I allowed to write?* (binary)
//! - `lease.json` → *who has the lease right now, when did they last
//!   heartbeat?* (informational)
//!
//! The two can briefly disagree: a crashed process leaves a stale
//! `lease.json` after the flock is released. The next `try_acquire`
//! observes that, overwrites `lease.json` with its own info, and
//! continues. No reaper thread is needed.
//!
//! ## What this module **does** and **does not** do
//!
//! - It does **not** validate caps. Whoever holds the lease is
//!   trusted to call `caps::require` themselves before mutating.
//! - It does **not** spawn a heartbeat thread. The holder is expected
//!   to call `LeaseGuard::heartbeat` from its own event loop. A
//!   `spawn_heartbeat` helper can be layered on later if the agent
//!   runtime wants it.
//! - It does **not** force-evict a healthy holder. There is no
//!   "preempt" — the only way to lose a lease is to drop the guard,
//!   crash, or have the kernel kill you.
//!
//! ## Example
//!
//! ```ignore
//! use cos::session;
//! let sid = session::create("invoice agent").unwrap();
//! let guard = session::try_acquire(&sid).expect("nobody else holds it");
//! session::append_turn(&sid, turn).unwrap(); // safe — we hold the lease
//! guard.heartbeat().unwrap();
//! drop(guard); // lease released, lease.json removed
//! ```

use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use super::id::SessionId;
use super::meta::{now_rfc3339, Lease};
use super::store::{self, SessionError};

/// RAII handle to a held lease. Drop releases the flock (implicitly by
/// closing the fd) and removes `lease.json`. The lease can never
/// outlive the process that acquired it.
///
/// Cheap to move but not `Clone` — a lease has exactly one owner.
#[derive(Debug)]
pub struct LeaseGuard {
    sid: SessionId,
    pid: u32,
    started_at: String,
    // Holding this File alive keeps the flock; Drop closes the fd and
    // the kernel releases the lock.
    _lock: File,
}

impl LeaseGuard {
    /// Session this lease is for.
    pub fn sid(&self) -> &SessionId {
        &self.sid
    }

    /// PID of the process that acquired the lease (always the current
    /// process at acquire time — leases cannot be acquired on behalf
    /// of another process).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Refresh the `heartbeat_at` field in `lease.json`. Call this
    /// from the agent's main loop (every few seconds is plenty;
    /// readers use the timestamp purely for display).
    ///
    /// No-op semantically if it fails — the flock is still held, so
    /// the lease itself is intact; this only updates the display
    /// metadata. The error is returned so callers can log.
    pub fn heartbeat(&self) -> Result<(), SessionError> {
        store::write_lease(
            &self.sid,
            &Lease {
                pid: self.pid,
                started_at: self.started_at.clone(),
                heartbeat_at: now_rfc3339(),
                runtime: None,
            },
        )
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        // Remove the data file first so readers immediately see "no
        // holder", then close the fd to release the flock. Order
        // doesn't matter for correctness (a racing acquire would
        // succeed either way once the flock is freed) but this order
        // gives readers the cleanest signal.
        let _ = store::remove_lease(&self.sid);
        // _lock fd closes here, kernel releases LOCK_EX.
    }
}

/// Why a try_acquire failed.
#[derive(Debug, thiserror::Error)]
pub enum AcquireError {
    /// The session directory does not exist.
    #[error("session not found: {0}")]
    NotFound(String),

    /// Another process holds the flock. `held_by` reports what
    /// `lease.json` currently says — may be slightly stale (the holder
    /// may have just released and a heartbeat hasn't landed yet) but
    /// good enough for an error message.
    #[error("lease held by pid {}", .held_by.pid)]
    Held { held_by: Lease },

    /// Underlying IO error (could not open `lease.lock`, could not
    /// write `lease.json`, etc).
    #[error("io: {0}")]
    Io(String),
}

/// Non-blocking lease acquisition.
///
/// Returns `Ok(LeaseGuard)` if the lease was free and is now held by
/// this process. Returns `Err(AcquireError::Held)` if another process
/// holds it. Returns `Err(AcquireError::NotFound)` if the session
/// directory does not exist.
///
/// Never blocks. If you want to wait for the current holder to
/// release, poll with backoff — there is no `acquire_blocking` because
/// long waits inside the kernel make crash recovery harder to reason
/// about.
pub fn try_acquire(sid: &SessionId) -> Result<LeaseGuard, AcquireError> {
    let dir = store::session_dir(sid);
    if !dir.exists() {
        return Err(AcquireError::NotFound(sid.as_str().to_string()));
    }

    let lock_path = store::lease_lock_path(sid);
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| AcquireError::Io(format!("open {}: {e}", lock_path.display())))?;

    // SAFETY: lock_file is a valid open fd, libc::flock is async-signal-safe.
    #[cfg(unix)]
    {
        let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                // Someone else holds it. Try to report who.
                let held = store::read_lease(sid)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| Lease {
                        pid: 0,
                        started_at: String::new(),
                        heartbeat_at: String::new(),
                        runtime: None,
                    });
                return Err(AcquireError::Held { held_by: held });
            }
            return Err(AcquireError::Io(format!(
                "flock {}: {err}",
                lock_path.display()
            )));
        }
    }

    // Non-Unix platforms (Windows): we don't have a portable cross-
    // process advisory lock primitive available in `std`. Refuse to
    // acquire — better to fail closed than pretend we have mutual
    // exclusion we can't enforce. Windows support would route this
    // through `LockFileEx` on the open `HANDLE`; until that exists,
    // multi-process sessions on Windows are unsupported.
    #[cfg(not(unix))]
    {
        return Err(AcquireError::Io(format!(
            "session leases require flock(2); not implemented on this platform ({})",
            std::env::consts::OS
        )));
    }

    // We hold the flock. Stamp lease.json with our identity. If a
    // previous holder crashed, this overwrites their stale entry.
    let pid = std::process::id();
    let now = now_rfc3339();
    let lease = Lease {
        pid,
        started_at: now.clone(),
        heartbeat_at: now.clone(),
        runtime: None,
    };
    store::write_lease(sid, &lease)
        .map_err(|e| AcquireError::Io(format!("write lease.json: {e}")))?;

    Ok(LeaseGuard {
        sid: sid.clone(),
        pid,
        started_at: now,
        _lock: lock_file,
    })
}

/// Read the current `lease.json` for a session. `Ok(None)` means no
/// process has ever acquired the lease, or the previous holder cleanly
/// released. Note: this reads only the **informational** file; if you
/// need to know whether the lease is actually available right now,
/// call [`try_acquire`] and inspect the error.
pub fn current(sid: &SessionId) -> Result<Option<Lease>, SessionError> {
    store::read_lease(sid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_error_held_reports_pid_in_display() {
        let err = AcquireError::Held {
            held_by: Lease {
                pid: 4242,
                started_at: "2024-01-01T00:00:00Z".into(),
                heartbeat_at: "2024-01-01T00:00:30Z".into(),
                runtime: None,
            },
        };
        let s = format!("{err}");
        assert!(s.contains("4242"), "display: {s}");
    }

    #[test]
    fn acquire_error_not_found_includes_sid() {
        let err = AcquireError::NotFound("ses_x_y".into());
        assert!(format!("{err}").contains("ses_x_y"));
    }
}
