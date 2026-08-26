use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Tasks registered via [`spawn`] survive the foreground future
/// returning and are awaited by [`drain`]. This is the exact
/// pattern `cos agent ask` relies on for the auto-curator.
#[test]
fn drain_awaits_registered_tasks() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    runtime.block_on(async {
        for _ in 0..3 {
            let c = counter.clone();
            spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                c.fetch_add(1, Ordering::SeqCst);
            });
        }
        // Foreground future returns essentially immediately, as in
        // a real `ask` invocation.
        let drained = drain(Duration::from_secs(5)).await;
        assert_eq!(drained, 3, "all 3 background tasks should be drained");
    });
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

/// `drain` on an empty registry is a fast no-op.
#[test]
fn drain_empty_returns_zero() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let n = drain(Duration::from_secs(1)).await;
        assert_eq!(n, 0);
    });
}

/// `drain` respects the overall timeout when a task hangs.
/// Hanging tasks are abandoned (cancelled by the runtime later);
/// drain returns instead of blocking forever.
#[test]
fn drain_times_out_on_hung_task() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        spawn(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let start = std::time::Instant::now();
        let _ = drain(Duration::from_millis(100)).await;
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "drain should return promptly on timeout, got {:?}",
            start.elapsed()
        );
    });
}
