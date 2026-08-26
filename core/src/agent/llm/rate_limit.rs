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

/// Idempotency hint passed to [`retry_with_backoff_with_idempotency`].
///
/// The standard `retry_with_backoff` retries non-idempotent POSTs on
/// 5xx — which is a real bug, because the upstream may have processed
/// the request before returning the 5xx and a retry would issue the
/// effect twice. Callers that know the request is safe to retry pass
/// `Idempotency::Safe`; callers that have an `Idempotency-Key` header
/// pass `Idempotency::KeyHeader(key)`; everything else passes
/// `Idempotency::Unsafe` and gets stricter retry rules.
#[derive(Debug, Clone)]
pub enum Idempotency {
    /// HTTP method is GET/HEAD/OPTIONS, or the call is otherwise
    /// known-idempotent at the upstream. Same retry rules as the
    /// legacy `retry_with_backoff`.
    Safe,
    /// Caller is attaching an `Idempotency-Key: <key>` header (or
    /// equivalent) — upstream will collapse duplicate requests with
    /// the same key, so retrying on 5xx is OK. The key is stored
    /// here only for diagnostics.
    KeyHeader(String),
    /// Non-idempotent POST without a dedup key — retrying on a 5xx
    /// response can cause duplicate side effects. Only retry on
    /// `Transport` (the request never reached the server) and on
    /// explicit `RateLimited` (the server told us to wait).
    Unsafe,
}

impl Idempotency {
    /// Whether the retry helper is allowed to retry a transient
    /// 5xx server-response error under this idempotency hint.
    fn allows_5xx_retry(&self) -> bool {
        matches!(self, Idempotency::Safe | Idempotency::KeyHeader(_))
    }
}

/// True if `err` should trigger a retry under the given idempotency
/// hint. This is the stricter cousin of [`is_transient`] used by
/// [`retry_with_backoff_with_idempotency`].
fn is_transient_idem(err: &LlmError, hint: &Idempotency) -> bool {
    match err {
        LlmError::RateLimited { .. } | LlmError::Transport(_) => true,
        LlmError::Provider { status, .. } => *status >= 500 && hint.allows_5xx_retry(),
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
///
/// **CAUTION**: this function treats every call as idempotent — it
/// will retry 5xx errors even when the caller is doing a POST without
/// an idempotency key, which can cause duplicate side effects at the
/// upstream. New call sites SHOULD use
/// [`retry_with_backoff_with_idempotency`] and pass
/// [`Idempotency::Unsafe`] for non-idempotent POSTs.
pub async fn retry_with_backoff<T, F, Fut>(policy: RetryPolicy, op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    retry_with_backoff_with_idempotency(policy, Idempotency::Safe, op).await
}

/// Like [`retry_with_backoff`] but takes an explicit idempotency
/// hint. POSTs without an `Idempotency-Key` header (or upstream-side
/// dedup guarantee) MUST pass [`Idempotency::Unsafe`] so a 5xx
/// response does NOT trigger a retry — the upstream may have
/// processed the request before failing and a re-send would cause
/// duplicate side effects.
pub async fn retry_with_backoff_with_idempotency<T, F, Fut>(
    policy: RetryPolicy,
    idempotency: Idempotency,
    mut op: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_err: Option<LlmError> = None;
    for attempt in 1..=policy.max_attempts {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !is_transient_idem(&e, &idempotency) || attempt == policy.max_attempts {
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/rate_limit.rs"
    ));
}
