//! Rate limiting + retry-with-backoff for LLM provider calls.
//!
//! Two independent primitives:
//!
//!   * [`TokenBucket`] — classic token-bucket rate limiter. Bound
//!     concurrent calls to a provider so we don't get cut off by
//!     server-side throttling. `capacity` tokens fill at
//!     `refill_per_sec` per second. `acquire(n).await` waits for
//!     enough tokens; `try_acquire(n)` is non-blocking.
//!
//!   * [`RetryPolicy`] + [`retry_with_backoff`] — exponential
//!     backoff retry around a fallible async operation. Honours
//!     server-suggested retry intervals when the operation fails
//!     with [`LlmError::RateLimited { retry_after_ms }`].
//!
//! ## Backoff classification
//!
//! [`retry_with_backoff`] retries on:
//!   * [`LlmError::RateLimited`] — uses `max(retry_after_ms, computed_backoff)`
//!     so we never retry sooner than the server asked.
//!   * [`LlmError::Transport`] — assumed transient (DNS, TCP, TLS,
//!     server-side connection drops).
//!   * [`LlmError::Provider`] with `status >= 500` — server-side
//!     errors. 4xx (except 429) are caller bugs and are surfaced
//!     immediately.
//!
//! Other [`LlmError`] variants are non-transient and surface on the
//! first attempt.
//!
//! ## Why no external crate for jitter
//!
//! `rand` would pull a dependency into core for one tiny usage. We
//! seed a deterministic xorshift64 from system time per process
//! invocation — sufficient for spreading retry herds, no
//! cryptographic property needed.
//!
//! Library-only this commit; no provider auto-wires this yet.
//! Wiring is an opt-in per-provider decision (e.g. anthropic could
//! hold a `TokenBucket` keyed on the API key).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::agent::llm::{LlmError, Result};

// ---- Token bucket ---------------------------------------------------

/// Async token-bucket rate limiter.
///
/// Tokens accrue at `refill_per_sec` continuously up to `capacity`.
/// Cloning the `TokenBucket` shares the underlying state — same
/// bucket, same tokens.
#[derive(Clone)]
pub struct TokenBucket {
    inner: Arc<Mutex<TokenBucketInner>>,
}

struct TokenBucketInner {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Construct a bucket holding up to `capacity` tokens, refilling
    /// at `refill_per_sec` tokens per second. Starts full.
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(refill_per_sec > 0.0, "refill_per_sec must be > 0.0");
        Self {
            inner: Arc::new(Mutex::new(TokenBucketInner {
                capacity: capacity as f64,
                refill_per_sec,
                tokens: capacity as f64,
                last_refill: Instant::now(),
            })),
        }
    }

    /// Refill state-mutating helper: brings `tokens` up to the value
    /// we'd have if no acquire had happened since `last_refill`.
    fn refill(inner: &mut TokenBucketInner) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(inner.last_refill);
        let added = elapsed.as_secs_f64() * inner.refill_per_sec;
        inner.tokens = (inner.tokens + added).min(inner.capacity);
        inner.last_refill = now;
    }

    /// Try to take `n` tokens without blocking. Returns `true` on
    /// success.
    pub async fn try_acquire(&self, n: u32) -> bool {
        let mut g = self.inner.lock().await;
        Self::refill(&mut g);
        if g.tokens >= n as f64 {
            g.tokens -= n as f64;
            true
        } else {
            false
        }
    }

    /// Wait until `n` tokens are available, then consume them.
    /// `n` must be ≤ `capacity` — otherwise the call deadlocks (we
    /// debug-assert).
    pub async fn acquire(&self, n: u32) {
        loop {
            let wait = {
                let mut g = self.inner.lock().await;
                Self::refill(&mut g);
                debug_assert!(
                    n as f64 <= g.capacity,
                    "acquire({n}) > capacity({}) would deadlock",
                    g.capacity
                );
                if g.tokens >= n as f64 {
                    g.tokens -= n as f64;
                    return;
                }
                let deficit = n as f64 - g.tokens;
                let secs = deficit / g.refill_per_sec;
                Duration::from_secs_f64(secs.max(0.001))
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Current token count, refilled to "now" first.
    /// Primarily useful for tests / observability.
    pub async fn available(&self) -> f64 {
        let mut g = self.inner.lock().await;
        Self::refill(&mut g);
        g.tokens
    }

    pub async fn capacity(&self) -> f64 {
        self.inner.lock().await.capacity
    }
}

// ---- Retry policy ---------------------------------------------------

/// Exponential-backoff retry parameters.
///
/// `delay(attempt) = clamp(base_ms × 2^(attempt-1), max_ms)`
/// where `attempt` is 1-indexed (after the first failure).
/// If `jitter` is true, the actual delay is multiplied by a random
/// factor in [0.5, 1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Maximum number of *total* attempts including the first call.
    /// `1` means "no retries" — a single attempt.
    pub max_attempts: u32,
    pub base_ms: u64,
    pub max_ms: u64,
    pub jitter: bool,
}

impl RetryPolicy {
    /// Sensible default: 4 total attempts, 500 ms → 1 s → 2 s with jitter.
    pub fn standard() -> Self {
        Self {
            max_attempts: 4,
            base_ms: 500,
            max_ms: 8000,
            jitter: true,
        }
    }

    pub fn no_retry() -> Self {
        Self {
            max_attempts: 1,
            base_ms: 0,
            max_ms: 0,
            jitter: false,
        }
    }

    /// Pure compute of the post-failure delay for `attempt` (1-indexed).
    /// Doesn't apply server-suggested overrides.
    pub fn delay_for(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        // Use saturating shift to prevent overflow on huge attempt values.
        let exp = attempt.saturating_sub(1).min(20);
        let base = self
            .base_ms
            .saturating_mul(1u64.checked_shl(exp).unwrap_or(u64::MAX));
        let clamped = base.min(self.max_ms);
        let factor = if self.jitter { jitter_factor() } else { 1.0 };
        Duration::from_millis(((clamped as f64) * factor).round() as u64)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::standard()
    }
}

/// True if `err` should trigger a retry (transient).
pub fn is_transient(err: &LlmError) -> bool {
    match err {
        LlmError::RateLimited { .. } | LlmError::Transport(_) => true,
        LlmError::Provider { status, .. } => *status >= 500,
        _ => false,
    }
}

/// Run `op` with exponential-backoff retry. `op` is a closure
/// returning a future — invoked fresh on each attempt so callers
/// can rebuild stateful HTTP requests per try.
///
/// Returns the first `Ok(_)` or the last `Err(_)`. Stops retrying
/// when:
///   * The operation succeeds.
///   * The error is non-transient (see [`is_transient`]).
///   * `policy.max_attempts` total attempts have been spent.
pub async fn retry_with_backoff<T, F, Fut>(policy: RetryPolicy, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_err: Option<LlmError> = None;
    for attempt in 1..=policy.max_attempts {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !is_transient(&e) || attempt == policy.max_attempts {
                    return Err(e);
                }
                let suggested = match &e {
                    LlmError::RateLimited { retry_after_ms } => {
                        Some(Duration::from_millis(*retry_after_ms))
                    }
                    _ => None,
                };
                let computed = policy.delay_for(attempt);
                let wait = match suggested {
                    Some(s) => s.max(computed),
                    None => computed,
                };
                last_err = Some(e);
                tokio::time::sleep(wait).await;
            }
        }
    }
    // Loop body always returns; this branch is unreachable when
    // max_attempts > 0. Defensive in case max_attempts is 0
    // (shouldn't happen but don't UB).
    Err(last_err.unwrap_or(LlmError::Internal(
        "retry_with_backoff invoked with max_attempts == 0".into(),
    )))
}

// ---- Jitter ---------------------------------------------------------

/// Process-wide xorshift64 state. Lazily seeded from system time on
/// first use. Not cryptographic — purely for spreading retry herds.
static JITTER_STATE: AtomicU64 = AtomicU64::new(0);

fn jitter_factor() -> f64 {
    let seed = JITTER_STATE.load(Ordering::Relaxed);
    let s = if seed == 0 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64 ^ 0xdead_beef_cafe_babe)
            .unwrap_or(0xdead_beef_cafe_babe);
        // Avoid 0 (xorshift fixed-point) — coerce to non-zero.
        if now == 0 {
            0xdead_beef_cafe_babe
        } else {
            now
        }
    } else {
        seed
    };
    let next = xorshift64(s);
    JITTER_STATE.store(next, Ordering::Relaxed);
    // Map to [0.5, 1.5).
    let normed = (next & 0xffff_ffff) as f64 / (u32::MAX as f64 + 1.0);
    0.5 + normed
}

fn xorshift64(mut x: u64) -> u64 {
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

#[cfg(test)]
mod tests {
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
            // [500ms, 1500ms).
            assert!(
                d >= Duration::from_millis(500) && d < Duration::from_millis(1500),
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
}
