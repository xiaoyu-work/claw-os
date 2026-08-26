//! Per-session interrupt signaling.
//!
//! The agent loop ([`super::loop_::ask_with`] / friends) checks for an
//! interrupt at the top of every turn and while awaiting provider or
//! tool work. When one is signaled, the loop returns
//! [`AgentError::Interrupted`](super::loop_::AgentError) and drops the
//! in-flight future at the next cancellation-aware await point.
//!
//! ## Why per-session?
//!
//! In production we routinely run multiple concurrent agent sessions
//! in the same process — gateway adapters spawn one session per chat
//! channel, the IPC `cos agent service` worker runs one session per
//! `submit`. A naive process-wide kill switch would tear them all
//! down at once. Each session registers under its own
//! [`Handle::session_id`]; signaling one is a no-op for the rest.
//!
//! ## Lifecycle
//!
//! 1. The loop calls [`register`] on entry — gets back a [`Handle`].
//! 2. The handle's [`Handle::check`] is called between turns; returns
//!    `true` if an external [`signal`] has been issued.
//! 3. On loop exit (success / failure / interrupt), the handle's
//!    `Drop` removes the session from the registry **only if the
//!    map entry still points at this handle's own flag** — see the
//!    re-register hazard discussion below.
//!
//! External signalers (e.g. a `cos agent interrupt <session-id>`
//! command, a Ctrl-C handler installed by the CLI) call [`signal`]
//! with the target session id. Returns `true` if a session was
//! found, `false` otherwise — useful for distinguishing "session
//! not running" from "interrupt delivered" at the CLI surface.
//!
//! ## Concurrency
//!
//! The registry is a single `Mutex<HashMap<String, Arc<SignalState>>>`.
//! Lock hold time is bounded to a hashmap lookup + clone of an `Arc`,
//! so it's never held across an `await`. Hot-path `check()` reads the
//! atomic flag directly with acquire ordering — no lock. Async
//! waiters use a `Notify` paired with the same sticky flag.
//!
//! ## Re-registration semantics
//!
//! Re-registering the same `session_id` is allowed: it cancels the
//! previous registration (sets its flag, so the old loop returns
//! [`AgentError::Interrupted`]) and installs the new entry. The old
//! handle's `Drop` no longer wipes the new entry — `RegistryGuard`
//! holds its own `Arc<SignalState>` and only removes the map entry
//! when the still-installed state is pointer-equal to the one being
//! dropped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Notify;

#[derive(Debug)]
struct SignalState {
    flag: AtomicBool,
    wake: Notify,
}

impl SignalState {
    fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
            wake: Notify::new(),
        }
    }

    fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }
}

/// Process-wide registry of in-flight sessions. Lazily initialized.
fn registry() -> &'static Mutex<HashMap<String, Arc<SignalState>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<SignalState>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Handle held by the agent loop for the lifetime of a session.
///
/// Cheap to clone (it's an `Arc` under the hood) — multiple worker
/// threads inside one session can all check the same flag. Removes
/// itself from the registry on the **last** clone's drop.
#[derive(Debug, Clone)]
pub struct Handle {
    session_id: String,
    state: Arc<SignalState>,
    /// Counts strong refs of the underlying state for the registry
    /// cleanup. The separate guard avoids relying on `Arc::strong_count`,
    /// which races with external signalers that briefly clone the state.
    _guard: Arc<RegistryGuard>,
}

/// Owned by the live `Handle` chain. On drop, removes the registry
/// entry **only** when it still matches the flag this guard installed
/// — re-registering under the same `session_id` rotates the flag, so
/// a stale guard whose flag has been displaced is a no-op cleanup.
/// Without this check, the late drop of the previous handle would
/// silently wipe the new entry and any pending [`signal`] for the
/// active session would return `false`.
#[derive(Debug)]
struct RegistryGuard {
    session_id: String,
    state: Arc<SignalState>,
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = registry().lock() {
            if let Some(current) = map.get(&self.session_id) {
                if Arc::ptr_eq(current, &self.state) {
                    map.remove(&self.session_id);
                }
                // Otherwise: someone re-registered under this id and
                // installed a fresh flag; leave it alone.
            }
        }
    }
}

impl Handle {
    /// Session id this handle was registered under. Stable across
    /// clones — used to plumb the id through nested calls.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// True once any holder of this session id has been signaled.
    /// Cheap — single atomic load. Safe to call in tight
    /// loops.
    pub fn check(&self) -> bool {
        self.state.flag.load(Ordering::Acquire)
    }

    /// Wait until this session is interrupted.
    ///
    /// The notification is paired with the sticky atomic flag so a
    /// signal delivered immediately before the waiter is registered
    /// cannot be lost.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.state.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.check() {
                return;
            }
            notified.await;
        }
    }

    /// Reset the flag to "not interrupted" — e.g. when an interactive
    /// CLI catches Ctrl-C, prompts the user "interrupt? [y/N]", and
    /// the user answers "no". Rare; the common case is to just let
    /// the handle drop and start a fresh session.
    pub fn clear(&self) {
        self.state.flag.store(false, Ordering::Release);
    }
}

/// Register a new session. Returns the handle the loop holds for the
/// duration of the session. Re-registering the same `session_id`
/// **cancels** the prior registration (sets its flag to `true`, so the
/// old loop sees `check() == true`) and installs the new entry.
///
/// Callers should still pick session ids unique enough to avoid
/// collisions in normal operation; the runtime uses the random session
/// id from `MemoryDb::new_session` for this. The re-register-cancel
/// path is defence in depth against caller bugs and quick test loops.
pub fn register(session_id: impl Into<String>) -> Handle {
    let session_id = session_id.into();
    let state = Arc::new(SignalState::new());
    let guard = Arc::new(RegistryGuard {
        session_id: session_id.clone(),
        state: state.clone(),
    });
    if let Ok(mut map) = registry().lock() {
        if let Some(old) = map.insert(session_id.clone(), state.clone()) {
            // Cancel any in-flight session that still observes the old
            // state — its loop will wake and exit with `Interrupted`.
            old.cancel();
        }
    }
    Handle {
        session_id,
        state,
        _guard: guard,
    }
}

/// Signal an interrupt for a registered session. Returns `true` if
/// the session was found and signaled, `false` if no live handle
/// matches `session_id`.
pub fn signal(session_id: &str) -> bool {
    let state = match registry().lock() {
        Ok(map) => map.get(session_id).cloned(),
        Err(_) => None,
    };
    match state {
        Some(state) => {
            state.cancel();
            true
        }
        None => false,
    }
}

/// Snapshot of currently-registered session ids. Useful for the CLI's
/// `cos agent interrupt --list` surface.
pub fn registered_sessions() -> Vec<String> {
    registry()
        .lock()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Check whether a session id is registered without holding a Handle.
pub fn is_registered(session_id: &str) -> bool {
    registry()
        .lock()
        .map(|m| m.contains_key(session_id))
        .unwrap_or(false)
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/interrupt.rs"
    ));
}
