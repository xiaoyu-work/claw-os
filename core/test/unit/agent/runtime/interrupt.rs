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

#[tokio::test]
async fn signal_wakes_async_cancellation_waiter() {
    let id = unique_id("async-wake");
    let h = register(&id);
    let waiter = h.clone();
    let waiting = tokio::spawn(async move {
        waiter.cancelled().await;
    });

    assert!(signal(&id));
    tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .expect("cancellation waiter did not wake")
        .unwrap();
}

#[tokio::test]
async fn cancelled_returns_when_signal_precedes_wait() {
    let id = unique_id("async-sticky");
    let h = register(&id);
    assert!(signal(&id));
    tokio::time::timeout(std::time::Duration::from_secs(1), h.cancelled())
        .await
        .expect("pre-signalled cancellation was lost");
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
