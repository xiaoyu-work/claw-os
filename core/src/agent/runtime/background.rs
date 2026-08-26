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
/// (any others were **aborted** and dropped from the registry so the
/// runtime can shut down promptly). Safe to call multiple times: a
/// second call after a `drain` simply finds an empty queue.
///
/// On timeout, each still-pending [`JoinHandle`] is `.abort()`-ed
/// before the function returns. Without the abort the runtime's drop
/// would still cancel them, but in long-running setups (multiple
/// `ask` calls back-to-back, or embedded servers that keep the
/// runtime alive after `drain` returns) hung tasks would otherwise
/// leak indefinitely.
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

    // Move handles into an Arc<Mutex<Vec<_>>> so the abort branch can
    // see whatever remains if the join branch loses the race. We
    // pop one handle at a time from the front; whatever stays in
    // the vec on timeout gets aborted.
    let remaining = std::sync::Arc::new(std::sync::Mutex::new(handles));
    let remaining_for_join = remaining.clone();

    let join = async move {
        loop {
            let next = {
                let mut guard = remaining_for_join
                    .lock()
                    .expect("background drain mutex poisoned");
                guard.pop()
            };
            match next {
                Some(h) => {
                    // Ignore individual task errors / panics — curator + indexer
                    // already log their own failures, and we just want to
                    // know everything settled.
                    let _ = h.await;
                }
                None => break,
            }
        }
    };

    tokio::select! {
        _ = join => {
            tracing::trace!("background: drained {total} task(s)");
            total
        }
        _ = tokio::time::sleep(overall_timeout) => {
            // Snatch whatever's still pending and abort it. JoinHandle::abort
            // is non-blocking — the task receives a cancellation
            // signal at its next .await point. We don't .await the
            // handles afterwards: that would re-introduce the same
            // hang we're trying to escape. Dropping the handles
            // doesn't itself cancel a finished task; the abort call
            // is what guarantees nothing leaks if the runtime keeps
            // running after `drain` returns.
            let pending: Vec<JoinHandle<()>> = {
                let mut guard = remaining
                    .lock()
                    .expect("background drain mutex poisoned");
                std::mem::take(&mut *guard)
            };
            let aborted = pending.len();
            for h in &pending {
                h.abort();
            }
            tracing::warn!(
                "background: drain timed out after {:?}; aborted {} pending task(s) \
                 (likely a slow auxiliary-LLM call or an unresponsive embedder)",
                overall_timeout,
                aborted,
            );
            // Tasks that completed before the timeout were popped off
            // by the join branch and counted in `total - aborted`.
            total.saturating_sub(aborted)
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/background.rs"
    ));
}
