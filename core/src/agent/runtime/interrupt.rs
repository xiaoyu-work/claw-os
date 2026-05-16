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
    use super::*;

    /// Make every test use a unique session id so concurrent test
    /// threads don't collide in the global registry.
    fn unique_id(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
    }

    #[test]
    fn signal_returns_false_for_unknown_session() {
        assert!(!signal(&unique_id("never-registered")));
    }

    #[test]
    fn check_starts_false() {
        let h = register(unique_id("starts-false"));
        assert!(!h.check());
    }

    #[test]
    fn signal_flips_check_true() {
        let id = unique_id("flip");
        let h = register(&id);
        assert!(!h.check());
        assert!(signal(&id));
        assert!(h.check());
    }

    #[test]
    fn drop_removes_from_registry() {
        let id = unique_id("drop");
        {
            let _h = register(&id);
            assert!(is_registered(&id));
        }
        assert!(!is_registered(&id));
        // Signaling a dropped session is a benign no-op.
        assert!(!signal(&id));
    }

    #[test]
    fn clone_keeps_session_alive_until_last_drop() {
        let id = unique_id("clone-arc");
        let h = register(&id);
        let h2 = h.clone();
        assert!(is_registered(&id));
        drop(h);
        // Still registered — h2 holds the guard.
        assert!(is_registered(&id));
        assert!(signal(&id));
        assert!(h2.check());
        drop(h2);
        assert!(!is_registered(&id));
    }

    #[test]
    fn clone_shares_signal_state() {
        let id = unique_id("clone-shared");
        let h = register(&id);
        let h2 = h.clone();
        signal(&id);
        assert!(h.check());
        assert!(h2.check());
    }

    #[test]
    fn clear_resets_flag() {
        let id = unique_id("clear");
        let h = register(&id);
        signal(&id);
        assert!(h.check());
        h.clear();
        assert!(!h.check());
    }

    #[test]
    fn re_register_cancels_old() {
        let id = unique_id("re-register");
        let h1 = register(&id);
        assert!(!h1.check());

        // Re-register under the same id. The new handle starts fresh.
        let h2 = register(&id);
        assert!(!h2.check());
        // The old handle is now signaled — its loop will exit on the
        // next turn check (`AgentError::Interrupted`). This is the
        // re-register-cancels-old behaviour.
        assert!(
            h1.check(),
            "re-registering under the same id must cancel the prior handle"
        );

        // The new handle is the live one — signal hits its flag.
        signal(&id);
        assert!(h2.check());

        // When the old handle eventually drops, the registry entry for
        // the new handle must survive (Drop ptr-equality check).
        drop(h1);
        assert!(
            is_registered(&id),
            "old handle Drop must not wipe the active registration"
        );
        assert!(signal(&id), "the new handle is still reachable via signal");
    }

    /// Companion to `re_register_cancels_old`: even after the old
    /// handle is dropped (which previously called an unconditional
    /// `map.remove`), the new handle remains queryable both by
    /// [`is_registered`] and by [`signal`].
    #[test]
    fn old_handle_drop_does_not_evict_new_entry() {
        let id = unique_id("ptr-eq-drop");
        let h1 = register(&id);
        let h2 = register(&id);
        // Drop the displaced old handle first.
        drop(h1);
        // The new handle's flag must still be reachable.
        assert!(is_registered(&id));
        assert!(signal(&id));
        assert!(h2.check());
    }

    #[test]
    fn registered_sessions_lists_active_ids() {
        let id_a = unique_id("list-a");
        let id_b = unique_id("list-b");
        let _a = register(&id_a);
        let _b = register(&id_b);
        let listed = registered_sessions();
        assert!(listed.contains(&id_a));
        assert!(listed.contains(&id_b));
    }

    #[test]
    fn handle_session_id_is_stable() {
        let id = unique_id("stable");
        let h = register(&id);
        assert_eq!(h.session_id(), id);
        let h2 = h.clone();
        assert_eq!(h2.session_id(), id);
    }

    #[test]
    fn signal_is_idempotent() {
        let id = unique_id("idempotent");
        let h = register(&id);
        assert!(signal(&id));
        assert!(signal(&id));
        assert!(h.check());
    }

    /// Concurrency smoke: spawn many threads that signal the same id
    /// while a watcher loops on `check()`. Watcher must converge to
    /// `true` and stay there.
    #[test]
    fn concurrent_signal_and_check_converge() {
        use std::sync::Barrier;
        use std::thread;

        let id = unique_id("concurrent");
        let h = register(&id);

        const N_SIGNALERS: usize = 8;
        let barrier = Arc::new(Barrier::new(N_SIGNALERS + 1));
        let mut handles = Vec::with_capacity(N_SIGNALERS);
        for _ in 0..N_SIGNALERS {
            let b = barrier.clone();
            let id_c = id.clone();
            handles.push(thread::spawn(move || {
                b.wait();
                for _ in 0..100 {
                    signal(&id_c);
                }
            }));
        }
        barrier.wait();
        for j in handles {
            j.join().unwrap();
        }
        // All signalers done — the flag must be set.
        assert!(h.check());
    }
}
