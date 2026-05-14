//! Process-wide registry of "fire-and-forget" agent background tasks.
//!
//! The auto-curator and the semantic indexer both `tokio::spawn` LLM /
//! DB work after a turn produces a final answer. In interactive
//! (`cos agent chat`) mode this is harmless — the tokio runtime stays
//! alive across turns and the spawned tasks eventually finish on
//! their own. In one-shot (`cos agent ask`) mode the CLI builds a
//! current-thread runtime, `block_on`s the ask future, and then drops
//! the runtime. Dropping a tokio runtime **cancels every spawned
//! task immediately** (only `spawn_blocking` work is given a chance
//! by `shutdown_timeout`). So the curator gets killed mid-LLM-call,
//! `MEMORY.md` never updates, and the semantic index loses the last
//! turn's messages.
//!
//! This module fixes that. Background tasks register themselves with
//! [`spawn`], and the CLI one-shot entry points call [`drain`] (with
//! a timeout) inside `block_on` before the runtime is dropped.
//! Interactive paths simply never call `drain`; the registry then
//! holds dangling `JoinHandle`s which are abandoned at exit — same
//! behaviour as today.
//!
//! The registry is intentionally global (process-wide) rather than
//! threaded through every API. Adding `&BackgroundRegistry` to
//! `ask`, `ask_with_stream`, `AutoCurator::spawn_curate`,
//! `SemanticIndexer::spawn_index`, and every test helper would be a
//! sprawling churn for a one-bug fix. A `LazyLock<Mutex<Vec<_>>>` is
//! one allocation, no cross-cutting type changes.

use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;

static PENDING: LazyLock<Mutex<Vec<JoinHandle<()>>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Spawn `fut` on the current tokio runtime and register its handle so a
/// later [`drain`] call can await it. Behaves like `tokio::spawn(fut)`
/// from the caller's perspective — drop semantics, cancellation,
/// panics — except the handle is also tracked.
///
/// Opportunistically prunes any already-finished handles from the
/// registry on every call so an interactive `cos agent chat` session
/// doesn't accumulate stale entries across turns. (`JoinHandle`'s
/// `Drop` is silent — it doesn't itself abort a finished task, so
/// this is purely a memory hygiene step.)
pub fn spawn<F>(fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let handle = tokio::spawn(fut);
    let mut pending = PENDING.lock().expect("background pending mutex poisoned");
    pending.retain(|h| !h.is_finished());
    pending.push(handle);
}

/// Await all currently-registered background tasks (in arbitrary order)
/// with an overall timeout. Returns the count of tasks that completed
/// (any others were left pending and will be cancelled when the
/// runtime drops). Safe to call multiple times: a second call after
/// a `drain` simply finds an empty queue.
///
/// Must be called from inside a tokio runtime context.
pub async fn drain(overall_timeout: Duration) -> usize {
    let handles: Vec<JoinHandle<()>> = std::mem::take(
        &mut *PENDING.lock().expect("background pending mutex poisoned"),
    );
    if handles.is_empty() {
        return 0;
    }
    let total = handles.len();
    // Race a global timeout against awaiting every handle. We use
    // `tokio::select!` rather than `tokio::time::timeout(join_all)`
    // so we can count partial progress for the trace log.
    let join = async {
        for h in handles {
            // Ignore individual task errors / panics — curator + indexer
            // already log their own failures, and we just want to
            // know everything settled.
            let _ = h.await;
        }
    };
    tokio::select! {
        _ = join => {
            tracing::trace!("background: drained {total} task(s)");
            total
        }
        _ = tokio::time::sleep(overall_timeout) => {
            tracing::warn!(
                "background: drain timed out after {:?}; some tasks were cancelled \
                 (likely a slow auxiliary-LLM call or an unresponsive embedder)",
                overall_timeout,
            );
            // Unknown how many actually finished; report 0 as a
            // conservative lower bound.
            0
        }
    }
}

#[cfg(test)]
mod tests {
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
}
