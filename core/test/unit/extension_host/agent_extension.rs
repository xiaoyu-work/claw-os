use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
