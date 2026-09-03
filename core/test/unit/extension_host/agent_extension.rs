use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

enum WaitStep {
    Ready,
    Error(&'static str),
    Pending,
}

struct FakeRootChild {
    signals: Vec<i32>,
    signal_results: VecDeque<Result<(), String>>,
    waits: VecDeque<WaitStep>,
    live_after_wait: Result<bool, String>,
}

impl FakeRootChild {
    fn new(waits: impl IntoIterator<Item = WaitStep>, live_after_wait: bool) -> Self {
        Self {
            signals: Vec::new(),
            signal_results: VecDeque::new(),
            waits: waits.into_iter().collect(),
            live_after_wait: Ok(live_after_wait),
        }
    }
}

impl RootChildControl for FakeRootChild {
    fn signal(&mut self, signal: i32) -> Result<(), String> {
        self.signals.push(signal);
        self.signal_results.pop_front().unwrap_or(Ok(()))
    }

    fn exact_identity_live(&self) -> Result<bool, String> {
        self.live_after_wait.clone()
    }

    fn wait<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        match self.waits.pop_front().unwrap_or(WaitStep::Pending) {
            WaitStep::Ready => Box::pin(async { Ok(()) }),
            WaitStep::Error(error) => Box::pin(async move { Err(error.to_string()) }),
            WaitStep::Pending => Box::pin(std::future::pending()),
        }
    }
}

#[test]
fn instance_nonces_are_high_entropy_shape_and_distinct() {
    let first = random_nonce().unwrap();
    let second = random_nonce().unwrap();
    assert_eq!(first.len(), 64);
    assert_eq!(second.len(), 64);
    assert_ne!(first, second);
    assert!(first
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
}

#[tokio::test]
async fn process_discovery_uses_one_absolute_event_deadline() {
    run_blocking_until(
        super::super::abi::MonotonicDeadlineNs::after(Duration::from_secs(1)).unwrap(),
        || {},
    )
    .await
    .unwrap();
    let started = std::time::Instant::now();
    let deadline =
        super::super::abi::MonotonicDeadlineNs::after(Duration::from_millis(50)).unwrap();
    run_blocking_until(deadline, || {
        std::thread::sleep(Duration::from_millis(10));
    })
    .await
    .unwrap();
    let error = run_blocking_until(deadline, || {
        std::thread::sleep(Duration::from_millis(40));
    })
    .await
    .unwrap_err();
    assert!(error.contains("deadline expired"), "{error}");
    assert!(
        started.elapsed() < Duration::from_millis(90),
        "the post-discovery phase renewed the event deadline"
    );
}

#[tokio::test]
async fn timed_out_discovery_cannot_apply_stale_state_or_retain_a_slot() {
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = slots.clone().acquire_owned().await.unwrap();
    let applied = Arc::new(AtomicBool::new(false));
    let deadline =
        super::super::abi::MonotonicDeadlineNs::after(Duration::from_millis(50)).unwrap();
    let result = run_blocking_until(deadline, || {
        std::thread::sleep(Duration::from_millis(100));
        true
    })
    .await;
    if let Ok(value) = result {
        applied.store(value, Ordering::Release);
    }
    drop(permit);

    assert!(slots.try_acquire().is_ok(), "event slot was not released");
    tokio::time::sleep(Duration::from_millis(75)).await;
    assert!(
        !applied.load(Ordering::Acquire),
        "timed-out discovery mutated stale lifecycle state"
    );
}

#[tokio::test]
async fn root_cleanup_propagates_the_first_wait_error() {
    let mut child = FakeRootChild::new([WaitStep::Error("first wait failed")], false);
    let error = terminate_root_child(
        &mut child,
        Duration::from_millis(5),
        Duration::from_millis(5),
    )
    .await
    .unwrap_err();
    assert_eq!(error, "first wait failed");
    assert_eq!(child.signals, vec![libc::SIGTERM]);
}

#[tokio::test]
async fn root_cleanup_requires_reap_after_sigkill() {
    let mut child = FakeRootChild::new([WaitStep::Pending, WaitStep::Pending], false);
    let error = terminate_root_child(
        &mut child,
        Duration::from_millis(5),
        Duration::from_millis(5),
    )
    .await
    .unwrap_err();
    assert!(error.contains("did not reap after SIGKILL"), "{error}");
    assert_eq!(child.signals, vec![libc::SIGTERM, libc::SIGKILL]);
}

#[tokio::test]
async fn root_cleanup_propagates_sigkill_failure() {
    let mut child = FakeRootChild::new([WaitStep::Pending], false);
    child.signal_results = VecDeque::from([Ok(()), Err("SIGKILL failed".to_string())]);
    let error = terminate_root_child(
        &mut child,
        Duration::from_millis(5),
        Duration::from_millis(5),
    )
    .await
    .unwrap_err();
    assert_eq!(error, "SIGKILL failed");
}

#[tokio::test]
async fn root_cleanup_rejects_a_live_exact_pid_identity_after_wait() {
    let mut child = FakeRootChild::new([WaitStep::Ready], true);
    let error = terminate_root_child(
        &mut child,
        Duration::from_millis(5),
        Duration::from_millis(5),
    )
    .await
    .unwrap_err();
    assert!(error.contains("identity survived reap"), "{error}");
}

#[tokio::test]
async fn root_cleanup_accepts_a_successfully_reaped_process() {
    let mut child = FakeRootChild::new([WaitStep::Ready], false);
    terminate_root_child(
        &mut child,
        Duration::from_millis(5),
        Duration::from_millis(5),
    )
    .await
    .unwrap();
    assert_eq!(child.signals, vec![libc::SIGTERM]);
}

#[test]
fn package_cleanup_failure_cannot_be_reported_as_success() {
    let error = combine_cleanup_results(Ok(()), Err("materialized package survived".to_string()))
        .unwrap_err();
    assert_eq!(error, "materialized package survived");
}
