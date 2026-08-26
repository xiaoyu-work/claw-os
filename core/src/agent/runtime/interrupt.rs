//! Per-session interrupt signaling.
//!
//! The agent loop ([`super::loop_::ask_with`] / friends) checks for an
//! interrupt at the top of every turn. When one is signaled, the loop
//! returns [`AgentError::Interrupted`](super::loop_::AgentError) — the
//! current in-flight turn (provider call + tool execution) is allowed
//! to drain naturally, but no further turns run.
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
//! The registry is a single `Mutex<HashMap<String, Arc<AtomicBool>>>`.
//! Lock hold time is bounded to a hashmap lookup + clone of an `Arc`,
//! so it's never held across an `await`. Hot-path `check()` reads the
//! `AtomicBool` directly with `Ordering::Relaxed` — no lock.
//!
//! ## Re-registration semantics
//!
//! Re-registering the same `session_id` is allowed: it cancels the
//! previous registration (sets its flag, so the old loop returns
//! [`AgentError::Interrupted`]) and installs the new entry. The old
//! handle's `Drop` no longer wipes the new entry — `RegistryGuard`
//! holds its own `Arc<AtomicBool>` and only removes the map entry
//! when the still-installed flag is pointer-equal to the one being
//! dropped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Process-wide registry of in-flight sessions. Lazily initialized.
fn registry() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
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
    flag: Arc<AtomicBool>,
    /// Counts strong refs of the underlying flag for the registry
    /// cleanup. We use a separate `Arc<()>` because `Arc::strong_count`
    /// on the flag itself races with external signalers that briefly
    /// hold their own clone.
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
    flag: Arc<AtomicBool>,
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = registry().lock() {
            if let Some(current) = map.get(&self.session_id) {
                if Arc::ptr_eq(current, &self.flag) {
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
    /// Cheap — single relaxed atomic load. Safe to call in tight
    /// loops.
    pub fn check(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// Reset the flag to "not interrupted" — e.g. when an interactive
    /// CLI catches Ctrl-C, prompts the user "interrupt? [y/N]", and
    /// the user answers "no". Rare; the common case is to just let
    /// the handle drop and start a fresh session.
    pub fn clear(&self) {
        self.flag.store(false, Ordering::Relaxed);
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
    let flag = Arc::new(AtomicBool::new(false));
    let guard = Arc::new(RegistryGuard {
        session_id: session_id.clone(),
        flag: flag.clone(),
    });
    if let Ok(mut map) = registry().lock() {
        if let Some(old) = map.insert(session_id.clone(), flag.clone()) {
            // Cancel any in-flight session that still observes the old
            // flag — its loop will see `check() == true` and exit with
            // `Interrupted` on its next turn boundary.
            old.store(true, Ordering::Relaxed);
        }
    }
    Handle {
        session_id,
        flag,
        _guard: guard,
    }
}

/// Signal an interrupt for a registered session. Returns `true` if
/// the session was found and signaled, `false` if no live handle
/// matches `session_id`.
pub fn signal(session_id: &str) -> bool {
    let flag = match registry().lock() {
        Ok(map) => map.get(session_id).cloned(),
        Err(_) => None,
    };
    match flag {
        Some(f) => {
            f.store(true, Ordering::Relaxed);
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
