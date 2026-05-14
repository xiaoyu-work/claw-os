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

    let prev_session_env = std::env::var_os(ENV_COS_SESSION);
    std::env::set_var(ENV_COS_SESSION, sid.as_str());

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

    // Flip to Paused while we still hold the lease. If this fails the
    // lease is still released by Drop below — caller can retry.
    store::update_meta(&sid, |m| {
        if m.status.is_active() && m.status != Status::Paused {
            m.status = Status::Paused;
        }
    })?;

    handle.lease = None;
    restore_env(handle.prev_session_env.take());
    handle.finished = true;
    Ok(())
}

/// Resume a previously paused session: re-acquire the lease, flip
/// `status` back to `Running`, set `COS_SESSION`, return a fresh
/// [`DurableSession`].
///
/// Refuses sessions whose status is anything other than `Paused`
/// (`Pending` should go through [`promote_to_durable`], terminal
/// sessions are read-only). To take over from a runtime that crashed
/// mid-Running you'll currently see `InvalidStatus { actual: Running }`
/// — the recovery path will be filed as a Phase 6 follow-up.
pub fn resume(
    sid: &SessionId,
    runtime: impl Into<String>,
) -> Result<DurableSession, TransitionError> {
    if !store::session_dir(sid).exists() {
        return Err(TransitionError::NotFound(sid.as_str().to_string()));
    }

    let meta = store::get_meta(sid)?;
    if meta.status != Status::Paused {
        return Err(TransitionError::InvalidStatus {
            actual: meta.status,
        });
    }

    let lease = lease::try_acquire(sid)?;

    store::update_meta(sid, |m| {
        m.status = Status::Running;
    })?;

    let prev_session_env = std::env::var_os(ENV_COS_SESSION);
    std::env::set_var(ENV_COS_SESSION, sid.as_str());

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
    match prev {
        Some(v) => std::env::set_var(ENV_COS_SESSION, v),
        None => std::env::remove_var(ENV_COS_SESSION),
    }
}
