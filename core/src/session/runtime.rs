//! High-level lifecycle helpers on top of the disk layout + lease.
//!
//! [`store`](super::store) gives you "create the directory, append a
//! turn, write meta". [`lease`](super::lease) gives you "hold the
//! flock". This module composes the two so a typical agent runtime
//! can say:
//!
//! ```ignore
//! use cos::session;
//! let s = session::promote_to_durable("整理发票", "cos-agent")?;
//! // …agent loop, calls into apps that update s.sid()…
//! s.finish(session::Status::Done)?;
//! ```
//!
//! ## What "promote_to_durable" means
//!
//! The CLI's bootstrap code (see [`crate::caps::bootstrap`]) mints a
//! short-lived in-process session row for every `cos` invocation. Most
//! invocations are one-shot (`cos perms ls`, `cos fs read …`) and
//! should never spill onto disk — promoting them all would explode
//! `$COS_DATA_DIR/sessions/` into millions of empty directories.
//!
//! `promote_to_durable` is the **opt-in** path an agent runtime calls
//! when it knows the work is going to be long-lived: it allocates a
//! durable directory, takes the lease, updates the `COS_SESSION` env
//! var so any subprocess inherits "I am inside session X", and hands
//! back a [`DurableSession`] RAII guard. Drop without [`finish`] marks
//! the meta `Failed`.
//!
//! ## Pause / resume
//!
//! [`pause`] consumes a [`DurableSession`] and flips status to
//! `Paused` while releasing the lease. [`resume`] takes a sid +
//! runtime label and tries to re-acquire the lease, returning a fresh
//! [`DurableSession`]. Resume only accepts sessions whose status is
//! `Paused` — it is not a hostile takeover; if a different runtime is
//! actively holding the lease we surface `TransitionError::Lease`.
//!
//! ## Cross-runtime handover
//!
//! Because the session lives on disk and the lease is a kernel-level
//! flock, "another agent picks up where I left off" is just:
//!
//! ```ignore
//! // agent A:
//! let s = session::promote_to_durable("…", "cos-agent")?;
//! …work…
//! session::pause(s)?;
//!
//! // agent B in a totally different process / runtime:
//! let s = session::resume(&sid, "langchain-py")?;
//! …continue work…
//! ```
//!
//! Nothing needs to be in-memory at handover time; the JSONL files +
//! `state.json` are the contract.

use std::ffi::OsString;

use super::id::SessionId;
use super::lease::{self, AcquireError, LeaseGuard};
use super::meta::Status;
use super::store::{self, SessionError};

/// Env var that carries "you are inside session X" to subprocesses.
///
/// Mirrored from `caps/bootstrap.rs` and `perms.rs` — the canonical
/// signal for "this `cos` invocation should target an existing
/// session, not mint a new one".
const ENV_COS_SESSION: &str = "COS_SESSION";

/// Serializes every mutation of `COS_SESSION` across the entire
/// process.
///
/// `std::env::set_var` is `unsafe`-ish on Unix: glibc's `setenv`
/// races against any concurrent reader of `environ` (e.g. another
/// thread calling `std::env::var`, `getenv`, or fork()ing). The
/// safest in-tree fix is to funnel **every** read-or-write of
/// `COS_SESSION` through this mutex. Callers that touch the env are
/// the lifecycle helpers in this module; nothing else inside the
/// kernel mutates it.
static ENV_COS_SESSION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_env_lock<F, T>(f: F) -> T
where
    F: FnOnce() -> T,
{
    let _g = ENV_COS_SESSION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f()
}

// ---------------------------------------------------------------------------
// DurableSession — the RAII handle returned by promote / resume
// ---------------------------------------------------------------------------

/// RAII handle to a promoted session.
///
/// While alive: this process holds the [`LeaseGuard`] and `COS_SESSION`
/// is set to the session's id. On clean shutdown, call
/// [`finish`](Self::finish) (or [`pause`]). Dropping without either
/// marks the meta `Failed` so the user can see "this agent died".
///
/// Cheap to move (the underlying lease is a single fd). Not `Clone` —
/// at most one handle exists per session per process.
#[must_use = "drop without finish() or pause() marks the session Failed"]
pub struct DurableSession {
    sid: SessionId,
    runtime: String,
    /// Wrapped in `Option` so [`finish`](Self::finish) and [`pause`]
    /// can take it without confusing Drop (Drop sees `None` and skips
    /// the fail-marking branch).
    lease: Option<LeaseGuard>,
    /// What `COS_SESSION` was set to before promote/resume. Drop
    /// restores this so a parent agent's session env is not clobbered.
    prev_session_env: Option<OsString>,
    finished: bool,
}

impl std::fmt::Debug for DurableSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableSession")
            .field("sid", &self.sid)
            .field("runtime", &self.runtime)
            .field("has_lease", &self.lease.is_some())
            .field("finished", &self.finished)
            .finish()
    }
}

impl DurableSession {
    /// Session id this handle is bound to.
    pub fn sid(&self) -> &SessionId {
        &self.sid
    }

    /// Runtime label this handle was created with (`"cos-agent"`,
    /// `"langchain-py"`, …). Recorded on the meta as
    /// `creator_runtime` and on heartbeats as the lease `runtime`.
    pub fn runtime(&self) -> &str {
        &self.runtime
    }

    /// Forward-only heartbeat. Updates `lease.json::heartbeat_at`.
    /// Failure is non-fatal — the flock is still held, only the
    /// display metadata is stale.
    pub fn heartbeat(&self) -> Result<(), SessionError> {
        match &self.lease {
            Some(g) => g.heartbeat(),
            None => Err(SessionError::NotFound(format!(
                "{}: durable session has no lease (already finished/paused)",
                self.sid
            ))),
        }
    }

    /// Mark the session terminal with `status` (must be `Done` or
    /// `Failed`), release the lease, and restore `COS_SESSION`. After
    /// this returns, dropping the handle is a no-op.
    pub fn finish(mut self, status: Status) -> Result<(), SessionError> {
        debug_assert!(
            !status.is_active(),
            "DurableSession::finish requires a terminal status"
        );
        store::end(&self.sid, status)?;
        // Drop the lease BEFORE marking finished, so a panic between
        // end() and the env restore still surfaces as Failed via Drop.
        self.lease = None;
        restore_env(self.prev_session_env.take());
        self.finished = true;
        Ok(())
    }
}

impl Drop for DurableSession {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // The handle was dropped without finish() or pause(): treat as
        // crash. Mark the session Failed so the user can see why no
        // agent is making progress, then release lease + env.
        let _ = store::end(&self.sid, Status::Failed);
        self.lease = None;
        restore_env(self.prev_session_env.take());
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why [`promote_to_durable`] failed. Promote allocates a fresh sid so
/// it can never collide with an existing holder; the only realistic
/// failures are IO + lease acquisition on a brand-new directory.
#[derive(Debug, thiserror::Error)]
pub enum PromoteError {
    #[error("session: {0}")]
    Session(#[from] SessionError),

    #[error("lease: {0}")]
    Lease(#[from] AcquireError),
}

/// Why [`pause`] / [`resume`] failed.
#[derive(Debug, thiserror::Error)]
pub enum TransitionError {
    /// The sid does not exist on disk.
    #[error("session not found: {0}")]
    NotFound(String),

    /// The session's current `status` does not allow this transition.
    /// For [`resume`] this means anything other than `Paused`.
    #[error("invalid status for transition: {actual:?}")]
    InvalidStatus { actual: Status },

    /// Lease acquisition failed (typically: another runtime currently
    /// holds the lease).
    #[error("lease: {0}")]
    Lease(#[from] AcquireError),

    /// Underlying IO / serde failure.
    #[error("session: {0}")]
    Session(#[from] SessionError),
}

// ---------------------------------------------------------------------------
// promote_to_durable
// ---------------------------------------------------------------------------

/// Allocate a fresh durable session and immediately attach this
/// process to it.
///
/// Steps:
///
/// 1. `store::create(purpose)` — mint sid + write `meta.json` (status
///    `Pending`).
/// 2. `update_meta` — stamp `creator_runtime` and flip status to
///    `Running`.
/// 3. `lease::try_acquire` — take the flock. Cannot fail with `Held`
///    because the directory is brand new; a `Held` here would indicate
///    a hash collision and we surface it.
/// 4. Save the prior `COS_SESSION`, set it to the new sid so any
///    subprocess inherits "you are inside this session".
///
/// Returns a [`DurableSession`] guard. Dropping without
/// [`DurableSession::finish`] or [`pause`] marks the session `Failed`.
pub fn promote_to_durable(
    purpose: impl Into<String>,
    runtime: impl Into<String>,
) -> Result<DurableSession, PromoteError> {
    let runtime = runtime.into();
    let sid = store::create(purpose)?;

    // Stamp creator_runtime + flip to Running. We do this BEFORE
    // taking the lease so a subsequent crash leaves the meta in a
    // self-explanatory state (status=Running, no lease holder ⇒ "the
    // last runner died").
    let runtime_for_meta = runtime.clone();
    store::update_meta(&sid, |m| {
        m.creator_runtime = Some(runtime_for_meta);
        m.status = Status::Running;
    })?;

    let lease = lease::try_acquire(&sid)?;

    let prev_session_env = with_env_lock(|| {
        let prev = std::env::var_os(ENV_COS_SESSION);
        std::env::set_var(ENV_COS_SESSION, sid.as_str());
        prev
    });

    Ok(DurableSession {
        sid,
        runtime,
        lease: Some(lease),
        prev_session_env,
        finished: false,
    })
}

// ---------------------------------------------------------------------------
// pause / resume
// ---------------------------------------------------------------------------

/// Pause an attached session: flip `status` to `Paused`, drop the
/// lease, and restore `COS_SESSION`. After this returns, the session
/// directory is intact and a future [`resume`] (in the same process or
/// a totally different runtime) can pick it up.
pub fn pause(mut handle: DurableSession) -> Result<(), TransitionError> {
    let sid = handle.sid.clone();

    // Refuse to "pause" anything other than a Running session. Pause
    // is a transition from {Running} → {Paused}; firing it on a
    // session that is already Paused, or that is terminal, is a
    // caller bug we want to surface loudly. We check *before*
    // mutating the meta so a misuse doesn't leave the meta in a
    // wedged state.
    let meta = store::get_meta(&sid)?;
    if meta.status != Status::Running {
        return Err(TransitionError::InvalidStatus {
            actual: meta.status,
        });
    }

    store::update_meta(&sid, |m| {
        m.status = Status::Paused;
    })?;

    handle.lease = None;
    restore_env(handle.prev_session_env.take());
    handle.finished = true;
    Ok(())
}

/// Resume a previously paused — *or crashed-but-orphaned* — session:
/// re-acquire the lease, flip `status` back to `Running`, set
/// `COS_SESSION`, return a fresh [`DurableSession`].
///
/// Accepts two starting states:
///
/// 1. `Paused` — the previous holder cleanly handed off via
///    [`pause`]. The lease is free; we grab it and flip Running.
/// 2. `Running` but with no live lease holder — the previous holder
///    crashed (panic, segfault, kill -9) before pause could run. The
///    on-disk meta still says Running because nobody got to update
///    it. We *can* prove the previous process is gone because
///    `lease::try_acquire` returns `Held` while any other process
///    holds the flock; if it succeeds, the kernel has already
///    released the prior holder's lock, which only happens at
///    process exit. In that case we re-stamp `meta.status` first so
///    audit logs reflect the recovery.
///
/// All other statuses (`Pending`, terminal) are rejected.
pub fn resume(
    sid: &SessionId,
    runtime: impl Into<String>,
) -> Result<DurableSession, TransitionError> {
    if !store::session_dir(sid).exists() {
        return Err(TransitionError::NotFound(sid.as_str().to_string()));
    }

    let meta = store::get_meta(sid)?;
    match meta.status {
        Status::Paused | Status::Running => {}
        other => {
            return Err(TransitionError::InvalidStatus { actual: other });
        }
    }

    // `try_acquire` is the proof of life. If the previous holder is
    // still up, this fails with AcquireError::Held and we bail
    // without disturbing the meta — the caller learns "another
    // runtime owns this session".
    let lease = lease::try_acquire(sid)?;

    // We got the lease. If status was Running, the previous holder
    // is provably dead (or it would still hold the flock). Re-stamp
    // status so the audit log records the recovery point even
    // though the value doesn't change after we flip it back to
    // Running below.
    store::update_meta(sid, |m| {
        m.status = Status::Running;
    })?;

    let prev_session_env = with_env_lock(|| {
        let prev = std::env::var_os(ENV_COS_SESSION);
        std::env::set_var(ENV_COS_SESSION, sid.as_str());
        prev
    });

    Ok(DurableSession {
        sid: sid.clone(),
        runtime: runtime.into(),
        lease: Some(lease),
        prev_session_env,
        finished: false,
    })
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn restore_env(prev: Option<OsString>) {
    with_env_lock(|| match prev {
        Some(v) => std::env::set_var(ENV_COS_SESSION, v),
        None => std::env::remove_var(ENV_COS_SESSION),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit fix (session/runtime.rs HIGH "resume from crashed"):
    /// a session whose on-disk `meta.status` is still `Running`
    /// because the previous holder crashed before it could flip to
    /// `Paused` must be resumable — otherwise the session is
    /// permanently wedged. The proof of crash is that
    /// `lease::try_acquire` succeeds: the kernel only releases the
    /// flock when the holding process exits.
    #[test]
    fn resume_from_crashed_state() {
        let _lock = crate::test_env::lock_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let prev_data = std::env::var_os("COS_DATA_DIR");
        std::env::set_var("COS_DATA_DIR", dir.path());

        // Create a session and flip it to Running directly — we do
        // NOT acquire a lease, mimicking a process that died after
        // updating meta but before holding the flock long enough for
        // any orderly handoff.
        let sid = store::create("test").expect("create session");
        store::update_meta(&sid, |m| {
            m.status = Status::Running;
        })
        .expect("flip to Running");

        // Sanity: no current process holds the flock.
        assert_eq!(
            store::get_meta(&sid).unwrap().status,
            Status::Running,
            "precondition: meta should report Running"
        );

        // Resume from a "crashed Running" state. Audit fix says this
        // must succeed (not return InvalidStatus) and return a
        // handle that re-stamps the lease.
        let handle = resume(&sid, "runtime-test").expect(
            "resume must accept Status::Running when the prior lease holder is gone",
        );

        // The returned handle owns a fresh lease so a subsequent
        // resume from a competing process would now see `Held`.
        assert!(handle.lease.is_some(), "resume should re-acquire lease");

        // And the meta should still be Running (resume re-stamps).
        assert_eq!(
            store::get_meta(&sid).unwrap().status,
            Status::Running,
            "post-resume status should remain Running"
        );

        // Cleanly drop the handle without finish(); that's a
        // separate concern — what matters is that resume() didn't
        // refuse the crashed-Running state.
        drop(handle);

        match prev_data {
            Some(v) => std::env::set_var("COS_DATA_DIR", v),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }

    /// Resume from a normal `Paused` state still works (regression
    /// guard so the audit fix's "also accept Running" doesn't
    /// inadvertently break the canonical happy path).
    #[test]
    fn resume_from_paused_state() {
        let _lock = crate::test_env::lock_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let prev_data = std::env::var_os("COS_DATA_DIR");
        std::env::set_var("COS_DATA_DIR", dir.path());

        let sid = store::create("test").expect("create session");
        store::update_meta(&sid, |m| {
            m.status = Status::Paused;
        })
        .expect("flip to Paused");

        let handle = resume(&sid, "runtime-test").expect("resume from Paused");
        assert!(handle.lease.is_some());
        drop(handle);

        match prev_data {
            Some(v) => std::env::set_var("COS_DATA_DIR", v),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}
