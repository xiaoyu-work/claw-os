use super::*;
use std::sync::atomic::AtomicU32;

#[tokio::test(start_paused = true)]
async fn token_bucket_starts_full() {
    let b = TokenBucket::new(10, 1.0);
    assert_eq!(b.available().await, 10.0);
}

#[tokio::test(start_paused = true)]
async fn try_acquire_succeeds_when_tokens_available() {
    let b = TokenBucket::new(5, 1.0);
    assert!(b.try_acquire(3).await);
    assert_eq!(b.available().await, 2.0);
}

#[tokio::test(start_paused = true)]
async fn try_acquire_fails_when_insufficient() {
    let b = TokenBucket::new(5, 1.0);
    assert!(b.try_acquire(5).await);
    assert!(!b.try_acquire(1).await);
}

#[tokio::test(start_paused = true)]
async fn refills_over_time() {
    let b = TokenBucket::new(5, 10.0); // 10 tok/s
    b.try_acquire(5).await;
    assert_eq!(b.available().await, 0.0);
    tokio::time::advance(Duration::from_millis(500)).await;
    // 0.5s × 10 tok/s = 5 tokens accrued.
    let avail = b.available().await;
    assert!((avail - 5.0).abs() < 0.01, "expected ~5, got {avail}");
}

#[tokio::test(start_paused = true)]
async fn refill_caps_at_capacity() {
    let b = TokenBucket::new(5, 1.0);
    b.try_acquire(2).await;
    // Wait long enough to overfill.
    tokio::time::advance(Duration::from_secs(100)).await;
    assert_eq!(b.available().await, 5.0);
}

#[tokio::test(start_paused = true)]
async fn acquire_waits_then_returns() {
    let b = TokenBucket::new(5, 10.0);
    b.try_acquire(5).await;
    let h = tokio::spawn({
        let b = b.clone();
        async move { b.acquire(3).await }
    });
    // Future should still be pending right after spawn.
    tokio::time::advance(Duration::from_millis(100)).await;
    // Need 0.3s to refill 3 tokens at 10 tok/s; advance more.
    tokio::time::advance(Duration::from_millis(300)).await;
    h.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cloning_shares_state() {
    let b = TokenBucket::new(5, 1.0);
    let b2 = b.clone();
    b.try_acquire(3).await;
    // b2 sees the consumed state.
    assert_eq!(b2.available().await, 2.0);
}

#[test]
fn retry_policy_default_is_standard() {
    assert_eq!(RetryPolicy::default(), RetryPolicy::standard());
}

#[test]
fn retry_policy_delay_grows_then_clamps() {
    let p = RetryPolicy {
        max_attempts: 10,
        base_ms: 100,
        max_ms: 1000,
        jitter: false,
    };
    assert_eq!(p.delay_for(1), Duration::from_millis(100));
    assert_eq!(p.delay_for(2), Duration::from_millis(200));
    assert_eq!(p.delay_for(3), Duration::from_millis(400));
    assert_eq!(p.delay_for(4), Duration::from_millis(800));
    // Clamped at max_ms.
    assert_eq!(p.delay_for(5), Duration::from_millis(1000));
    assert_eq!(p.delay_for(20), Duration::from_millis(1000));
}

#[test]
fn retry_policy_jitter_within_bounds() {
    let p = RetryPolicy {
        max_attempts: 5,
        base_ms: 1000,
        max_ms: 1000,
        jitter: true,
    };
    for _ in 0..50 {
        let d = p.delay_for(1);
        // `delay_for` rounds `clamped * factor`, and `jitter_factor` is
        // exclusive of 1.5, so the largest representable delay rounds up
        // to exactly `max_ms * 1.5` — the upper bound is inclusive.
        assert!(
            d >= Duration::from_millis(500) && d <= Duration::from_millis(1500),
            "out of range: {d:?}"
        );
    }
}

#[test]
fn retry_policy_attempt_zero_is_zero() {
    let p = RetryPolicy::standard();
    assert_eq!(p.delay_for(0), Duration::ZERO);
}

#[test]
fn is_transient_classification() {
    assert!(is_transient(&LlmError::RateLimited { retry_after_ms: 0 }));
    assert!(is_transient(&LlmError::Provider {
        status: 500,
        message: "x".into()
    }));
    assert!(is_transient(&LlmError::Provider {
        status: 503,
        message: "x".into()
    }));
    assert!(!is_transient(&LlmError::Provider {
        status: 400,
        message: "x".into()
    }));
    assert!(!is_transient(&LlmError::Provider {
        status: 401,
        message: "x".into()
    }));
    assert!(!is_transient(&LlmError::Auth));
    assert!(!is_transient(&LlmError::InvalidRequest("x".into())));
    assert!(!is_transient(&LlmError::NotConfigured("x".into())));
    assert!(!is_transient(&LlmError::Parse("x".into())));
}

#[tokio::test(start_paused = true)]
async fn retry_succeeds_on_first_attempt() {
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let result: Result<u32> = retry_with_backoff(RetryPolicy::standard(), move || {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(7)
        }
    })
    .await;
    assert_eq!(result.unwrap(), 7);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn retry_eventually_succeeds() {
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let result: Result<u32> = retry_with_backoff(
        RetryPolicy {
            max_attempts: 3,
            base_ms: 10,
            max_ms: 100,
            jitter: false,
        },
        move || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                if n < 3 {
                    Err(LlmError::RateLimited { retry_after_ms: 10 })
                } else {
                    Ok(42)
                }
            }
        },
    )
    .await;
    assert_eq!(result.unwrap(), 42);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn retry_gives_up_after_max_attempts() {
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let result: Result<u32> = retry_with_backoff(
        RetryPolicy {
            max_attempts: 3,
            base_ms: 10,
            max_ms: 100,
            jitter: false,
        },
        move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(LlmError::RateLimited { retry_after_ms: 0 })
            }
        },
    )
    .await;
    assert!(matches!(result, Err(LlmError::RateLimited { .. })));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn retry_does_not_retry_non_transient_errors() {
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let result: Result<u32> = retry_with_backoff(RetryPolicy::standard(), move || {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            Err(LlmError::InvalidRequest("bad".into()))
        }
    })
    .await;
    assert!(matches!(result, Err(LlmError::InvalidRequest(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn retry_respects_server_retry_after_when_larger() {
    // Computed: 10ms; server says 5000ms → wait 5000ms.
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let policy = RetryPolicy {
        max_attempts: 2,
        base_ms: 10,
        max_ms: 100,
        jitter: false,
    };
    let start = tokio::time::Instant::now();
    let _: Result<u32> = retry_with_backoff(policy, move || {
        let c = c.clone();
        async move {
            let n = c.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 1 {
                Err(LlmError::RateLimited {
                    retry_after_ms: 5000,
                })
            } else {
                Err(LlmError::RateLimited { retry_after_ms: 0 })
            }
        }
    })
    .await;
    let elapsed = tokio::time::Instant::now().duration_since(start);
    // We waited at least 5s for the server-suggested interval.
    assert!(
        elapsed >= Duration::from_millis(5000),
        "elapsed = {elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn retry_gives_up_zero_max_attempts_returns_internal() {
    // Edge case: malformed policy with max_attempts=0.
    let policy = RetryPolicy {
        max_attempts: 0,
        base_ms: 0,
        max_ms: 0,
        jitter: false,
    };
    let result: Result<u32> = retry_with_backoff(policy, || async { Ok(1) }).await;
    // No attempts happen → Internal error.
    assert!(matches!(result, Err(LlmError::Internal(_))));
}

#[test]
fn jitter_factor_within_range() {
    for _ in 0..100 {
        let f = jitter_factor();
        assert!((0.5..1.5).contains(&f), "out of range: {f}");
    }
}

#[test]
fn xorshift_round_trip() {
    // Sanity: result is deterministic for same seed; different
    // seeds give different output.
    assert_eq!(xorshift64(1), xorshift64(1));
    assert_ne!(xorshift64(1), xorshift64(2));
}

/// A POST without `Idempotency-Key` must NOT have 5xx responses
/// retried — the upstream may have applied the side effect
/// before failing, and a re-send would duplicate it.
/// `Idempotency::Unsafe` callers therefore see Provider 5xx
/// surface on the first attempt. Transport errors and explicit
/// RateLimited still retry (the server either never saw the
/// request or explicitly asked us to wait).
#[tokio::test(start_paused = true)]
async fn post_not_retried_without_idempotency_key() {
    // 5xx with Unsafe → no retry.
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let result: Result<u32> = retry_with_backoff_with_idempotency(
        RetryPolicy {
            max_attempts: 5,
            base_ms: 1,
            max_ms: 10,
            jitter: false,
        },
        Idempotency::Unsafe,
        move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<u32, _>(LlmError::Provider {
                    status: 503,
                    message: "Service Unavailable".into(),
                })
            }
        },
    )
    .await;
    assert!(matches!(result, Err(LlmError::Provider { status: 503, .. })));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Unsafe POST must not retry 5xx responses"
    );

    // Same call with Safe DOES retry on 5xx (until exhausted).
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let _: Result<u32> = retry_with_backoff_with_idempotency(
        RetryPolicy {
            max_attempts: 4,
            base_ms: 1,
            max_ms: 10,
            jitter: false,
        },
        Idempotency::Safe,
        move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<u32, _>(LlmError::Provider {
                    status: 503,
                    message: "Service Unavailable".into(),
                })
            }
        },
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 4, "Safe path must retry 5xx");

    // Idempotency-Key header also unlocks 5xx retries.
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let _: Result<u32> = retry_with_backoff_with_idempotency(
        RetryPolicy {
            max_attempts: 3,
            base_ms: 1,
            max_ms: 10,
            jitter: false,
        },
        Idempotency::KeyHeader("uuid-123".into()),
        move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<u32, _>(LlmError::Provider {
                    status: 502,
                    message: "Bad gateway".into(),
                })
            }
        },
    )
    .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "KeyHeader must permit 5xx retry"
    );

    // Transport errors still retry under Unsafe — the request
    // never reached the server, so re-issuing is fine.
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    let _: Result<u32> = retry_with_backoff_with_idempotency(
        RetryPolicy {
            max_attempts: 3,
            base_ms: 1,
            max_ms: 10,
            jitter: false,
        },
        Idempotency::Unsafe,
        move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<u32, _>(LlmError::RateLimited { retry_after_ms: 1 })
            }
        },
    )
    .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "Unsafe POST must still retry RateLimited (explicit hint)"
    );
}
