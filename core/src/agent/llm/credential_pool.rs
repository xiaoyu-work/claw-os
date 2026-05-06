//! Credential pool — multi-key rotation for LLM providers.
//!
//! When you have several API keys for the same provider (e.g. three
//! OpenAI keys for a higher aggregate rate limit, or a primary +
//! backup pair for failover), a single `api_key_credential` /
//! `api_key_env` field is too narrow. This module provides a typed
//! pool that:
//!
//!   * Loads N keys at construction (eagerly, from
//!     `crate::credential::try_load` and/or `std::env::var`).
//!   * Hands one out per call via a [`SelectionStrategy`].
//!   * Tracks per-key health: success count, failure count, last
//!     error class, and an optional `cooldown_until` timestamp that
//!     skips the key until it expires.
//!   * Returns a [`PoolError`] when no key is currently usable.
//!
//! The pool is **stateless across process restarts** — health
//! tracking lives only in RAM. That's deliberate: stale auth state
//! from a previous process should not block a fresh start.
//!
//! ## Strategies
//!
//!   * [`SelectionStrategy::Sticky`] — always return the first
//!     usable key in declared order. Switches only after explicit
//!     [`Pool::report_failure`] with [`FailureClass::CooldownWorthy`].
//!   * [`SelectionStrategy::RoundRobin`] — rotate keys; skip any
//!     in cooldown.
//!   * [`SelectionStrategy::LeastErrors`] — return the key with the
//!     lowest cumulative failure count; ties broken by declared
//!     order. Tends to converge on the healthiest key.
//!
//! ## Why not random / weighted?
//!
//! Random distribution is harder to reason about under pathological
//! workloads (a single bad key in a 50-key pool still gets ~2% of
//! traffic). Weighted strategies need a feedback loop the kernel
//! doesn't have today. Sticky / RR / LeastErrors cover the common
//! cases; subclasses can be added later behind the same trait.
//!
//! ## Error classes
//!
//! Callers report failures back via [`Pool::report_failure`] so the
//! pool knows whether a key is *probably bad* (auth failure, quota
//! exhausted -> cooldown for a while), *transiently bad* (network /
//! 5xx -> bump failure count, no cooldown), or *not the key's
//! fault* (4xx caller error, malformed request -> ignore, count as
//! success since the wire reached the upstream).
//!
//! ## Concurrency
//!
//! State lives behind a `Mutex<PoolState>`. Selection is a fast
//! O(N) scan; for N up to a few hundred this is fine and avoids
//! lock contention from a more complex per-key Atomic dance. If you
//! ever need a 10k-key pool, swap the implementation behind the
//! same `Pool` API.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use thiserror::Error;

/// Default cooldown applied to a key after a [`FailureClass::CooldownWorthy`]
/// failure. Tunable per [`Pool`] via [`PoolBuilder::cooldown`].
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);

/// Cap on the number of distinct keys a single pool may hold. The
/// limit isn't enforced for technical reasons — it's a sanity check
/// against runaway config.
pub const MAX_POOL_SIZE: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionStrategy {
    Sticky,
    RoundRobin,
    LeastErrors,
}

impl SelectionStrategy {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "rr" | "round-robin" | "round_robin" | "roundrobin" => Self::RoundRobin,
            "least-errors" | "least_errors" | "leasterrors" => Self::LeastErrors,
            _ => Self::Sticky,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sticky => "sticky",
            Self::RoundRobin => "round-robin",
            Self::LeastErrors => "least-errors",
        }
    }
}

/// Where a single pool entry was loaded from. Carried for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    Credential(String),
    Env(String),
    Inline,
}

impl fmt::Display for KeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeySource::Credential(name) => write!(f, "credential:{name}"),
            KeySource::Env(name) => write!(f, "env:{name}"),
            KeySource::Inline => f.write_str("inline"),
        }
    }
}

/// One credential in the pool. The `value` is held in memory; do
/// **not** serialize a [`PoolEntry`].
#[derive(Debug, Clone)]
pub struct PoolEntry {
    pub source: KeySource,
    pub value: String,
}

impl PoolEntry {
    pub fn from_credential(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            source: KeySource::Credential(name.into()),
            value: value.into(),
        }
    }

    pub fn from_env(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            source: KeySource::Env(name.into()),
            value: value.into(),
        }
    }

    pub fn inline(value: impl Into<String>) -> Self {
        Self {
            source: KeySource::Inline,
            value: value.into(),
        }
    }
}

/// A failure class reported back to the pool after a request. The
/// pool decides whether to cooldown / count it / ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Auth failed (401 / 403), quota exhausted (429 with
    /// long-lived headers), or account suspended. Cooldown the key.
    CooldownWorthy,
    /// Transient network / 5xx / timeout. Bump failure count, no
    /// cooldown — the key is probably fine, the upstream isn't.
    Transient,
    /// Caller's fault (400 invalid request, schema mismatch). Don't
    /// blame the key; treat as a success in pool accounting so we
    /// don't drift to a different key for caller bugs.
    CallerError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeasePolicy {
    /// Snapshot the key value at the moment of lease — subsequent
    /// pool mutations (e.g. cooldown updates from a parallel call)
    /// don't invalidate this lease's value.
    SnapshotValue,
}

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("credential pool '{name}' is empty")]
    Empty { name: String },

    #[error(
        "credential pool '{name}' has no usable key (size={size}, all in cooldown until +{wait_ms}ms)"
    )]
    AllCoolingDown {
        name: String,
        size: usize,
        wait_ms: u128,
    },

    #[error("credential pool '{name}' rejected entry: {reason}")]
    InvalidEntry { name: String, reason: String },
}

/// Per-key health snapshot. Returned from [`Pool::stats`].
#[derive(Debug, Clone)]
pub struct KeyStats {
    pub source: KeySource,
    pub successes: u64,
    pub failures: u64,
    pub last_failure_class: Option<FailureClass>,
    /// Remaining cooldown when the snapshot was taken (`None` =
    /// available immediately).
    pub cooldown_remaining_ms: Option<u128>,
}

#[derive(Debug)]
struct EntryState {
    entry: PoolEntry,
    successes: u64,
    failures: u64,
    last_failure_class: Option<FailureClass>,
    cooldown_until: Option<Instant>,
}

#[derive(Debug)]
struct PoolState {
    entries: Vec<EntryState>,
    rr_cursor: usize,
}

/// One credential pool, keyed by `name` (used in error messages and
/// stats).
pub struct Pool {
    name: String,
    strategy: SelectionStrategy,
    cooldown: Duration,
    state: Mutex<PoolState>,
}

impl fmt::Debug for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool")
            .field("name", &self.name)
            .field("strategy", &self.strategy)
            .field("cooldown", &self.cooldown)
            .field(
                "len",
                &self.state.lock().map(|s| s.entries.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl Pool {
    /// Build a pool from a list of pre-loaded entries.
    pub fn from_entries(
        name: impl Into<String>,
        entries: Vec<PoolEntry>,
        strategy: SelectionStrategy,
    ) -> Result<Self, PoolError> {
        let name = name.into();
        if entries.is_empty() {
            return Err(PoolError::Empty { name });
        }
        if entries.len() > MAX_POOL_SIZE {
            return Err(PoolError::InvalidEntry {
                name,
                reason: format!(
                    "pool size {} exceeds max {MAX_POOL_SIZE}",
                    entries.len()
                ),
            });
        }
        for (i, e) in entries.iter().enumerate() {
            if e.value.trim().is_empty() {
                return Err(PoolError::InvalidEntry {
                    name,
                    reason: format!("entry #{i} ({}) has empty value", e.source),
                });
            }
        }
        let state = PoolState {
            entries: entries
                .into_iter()
                .map(|e| EntryState {
                    entry: e,
                    successes: 0,
                    failures: 0,
                    last_failure_class: None,
                    cooldown_until: None,
                })
                .collect(),
            rr_cursor: 0,
        };
        Ok(Self {
            name,
            strategy,
            cooldown: DEFAULT_COOLDOWN,
            state: Mutex::new(state),
        })
    }

    /// Build a pool by resolving credential names + env-var names
    /// against the active credential store + process env. Empty /
    /// missing entries are silently dropped; the order of arguments
    /// is preserved (`credential_names` first, then `env_names`).
    /// Inline keys are appended last.
    ///
    /// Returns `PoolError::Empty` only if **none** of the sources
    /// resolved to a non-empty value.
    pub fn from_sources(
        name: impl Into<String>,
        credential_names: &[&str],
        env_names: &[&str],
        inline: &[&str],
        strategy: SelectionStrategy,
    ) -> Result<Self, PoolError> {
        let mut entries = Vec::new();
        for cname in credential_names {
            if let Ok(Some(value)) = crate::credential::try_load(cname, "agent") {
                let v = value.trim();
                if !v.is_empty() {
                    entries.push(PoolEntry::from_credential(*cname, v.to_string()));
                }
            }
        }
        for ename in env_names {
            if let Ok(value) = std::env::var(ename) {
                let v = value.trim();
                if !v.is_empty() {
                    entries.push(PoolEntry::from_env(*ename, v.to_string()));
                }
            }
        }
        for raw in inline {
            let v = raw.trim();
            if !v.is_empty() {
                entries.push(PoolEntry::inline(v.to_string()));
            }
        }
        Self::from_entries(name, entries, strategy)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn strategy(&self) -> SelectionStrategy {
        self.strategy
    }

    pub fn cooldown(&self) -> Duration {
        self.cooldown
    }

    /// Override the cooldown applied after [`FailureClass::CooldownWorthy`].
    /// `Duration::ZERO` disables cooldown entirely (counted but not
    /// avoided).
    pub fn set_cooldown(&mut self, cooldown: Duration) {
        self.cooldown = cooldown;
    }

    pub fn len(&self) -> usize {
        self.state.lock().unwrap().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Acquire a lease for the next request. The lease is a value +
    /// an opaque token used by [`Pool::report_success`] /
    /// [`Pool::report_failure`].
    pub fn acquire(&self) -> Result<Lease, PoolError> {
        self.acquire_at(Instant::now())
    }

    /// Same as [`Pool::acquire`] but with an explicit clock for
    /// testing.
    pub fn acquire_at(&self, now: Instant) -> Result<Lease, PoolError> {
        let mut st = self.state.lock().unwrap();
        let total = st.entries.len();
        if total == 0 {
            return Err(PoolError::Empty {
                name: self.name.clone(),
            });
        }
        let pick = match self.strategy {
            SelectionStrategy::Sticky => sticky_pick(&st.entries, now),
            SelectionStrategy::RoundRobin => {
                let idx = round_robin_pick(&st.entries, st.rr_cursor, now);
                if let Some(i) = idx {
                    st.rr_cursor = (i + 1) % total.max(1);
                }
                idx
            }
            SelectionStrategy::LeastErrors => least_errors_pick(&st.entries, now),
        };
        let Some(idx) = pick else {
            // Compute the shortest remaining cooldown so callers can
            // back off intelligently.
            let wait_ms = st
                .entries
                .iter()
                .filter_map(|e| e.cooldown_until)
                .map(|until| {
                    if until > now {
                        (until - now).as_millis()
                    } else {
                        0
                    }
                })
                .min()
                .unwrap_or(0);
            return Err(PoolError::AllCoolingDown {
                name: self.name.clone(),
                size: total,
                wait_ms,
            });
        };
        let entry = &st.entries[idx].entry;
        Ok(Lease {
            index: idx,
            value: entry.value.clone(),
            source: entry.source.clone(),
            policy: LeasePolicy::SnapshotValue,
        })
    }

    /// Tell the pool that the request using this lease succeeded.
    pub fn report_success(&self, lease: &Lease) {
        let mut st = self.state.lock().unwrap();
        if let Some(e) = st.entries.get_mut(lease.index) {
            e.successes = e.successes.saturating_add(1);
            // A success clears any past failure class so subsequent
            // sticky decisions don't keep flipping. (We deliberately
            // don't reset failures count -- it's lifetime-cumulative
            // for diagnostics.)
            e.last_failure_class = None;
        }
    }

    /// Tell the pool that the request using this lease failed.
    /// Cooldown is applied if `class == CooldownWorthy`.
    pub fn report_failure(&self, lease: &Lease, class: FailureClass) {
        self.report_failure_at(lease, class, Instant::now())
    }

    pub fn report_failure_at(&self, lease: &Lease, class: FailureClass, now: Instant) {
        let mut st = self.state.lock().unwrap();
        let cooldown = self.cooldown;
        if let Some(e) = st.entries.get_mut(lease.index) {
            match class {
                FailureClass::CallerError => {
                    // Don't blame the key.
                    e.successes = e.successes.saturating_add(1);
                }
                FailureClass::Transient => {
                    e.failures = e.failures.saturating_add(1);
                    e.last_failure_class = Some(FailureClass::Transient);
                }
                FailureClass::CooldownWorthy => {
                    e.failures = e.failures.saturating_add(1);
                    e.last_failure_class = Some(FailureClass::CooldownWorthy);
                    if cooldown > Duration::ZERO {
                        e.cooldown_until = Some(now + cooldown);
                    }
                }
            }
        }
    }

    /// Snapshot of every key's health.
    pub fn stats(&self) -> Vec<KeyStats> {
        self.stats_at(Instant::now())
    }

    pub fn stats_at(&self, now: Instant) -> Vec<KeyStats> {
        let st = self.state.lock().unwrap();
        st.entries
            .iter()
            .map(|e| KeyStats {
                source: e.entry.source.clone(),
                successes: e.successes,
                failures: e.failures,
                last_failure_class: e.last_failure_class,
                cooldown_remaining_ms: e
                    .cooldown_until
                    .filter(|u| *u > now)
                    .map(|u| (u - now).as_millis()),
            })
            .collect()
    }

    /// Group lifetime totals across the whole pool.
    pub fn aggregate(&self) -> AggregateStats {
        let st = self.state.lock().unwrap();
        let mut total_successes = 0u64;
        let mut total_failures = 0u64;
        let mut by_class: HashMap<&'static str, u64> = HashMap::new();
        for e in &st.entries {
            total_successes = total_successes.saturating_add(e.successes);
            total_failures = total_failures.saturating_add(e.failures);
            if let Some(c) = e.last_failure_class {
                *by_class
                    .entry(match c {
                        FailureClass::CallerError => "caller_error",
                        FailureClass::Transient => "transient",
                        FailureClass::CooldownWorthy => "cooldown_worthy",
                    })
                    .or_insert(0) += 1;
            }
        }
        AggregateStats {
            size: st.entries.len(),
            total_successes,
            total_failures,
            last_failure_by_class: by_class,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AggregateStats {
    pub size: usize,
    pub total_successes: u64,
    pub total_failures: u64,
    pub last_failure_by_class: HashMap<&'static str, u64>,
}

/// A leased credential. Hold this for the duration of one request,
/// then call [`Pool::report_success`] / [`Pool::report_failure`].
#[derive(Debug, Clone)]
pub struct Lease {
    index: usize,
    value: String,
    source: KeySource,
    policy: LeasePolicy,
}

impl Lease {
    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn source(&self) -> &KeySource {
        &self.source
    }

    pub fn policy(&self) -> LeasePolicy {
        self.policy
    }

    /// Position in the pool at lease time. Useful for diagnostics
    /// only — pool indices are not stable across rebuilds.
    pub fn index(&self) -> usize {
        self.index
    }
}

fn is_available(state: &EntryState, now: Instant) -> bool {
    match state.cooldown_until {
        Some(until) => until <= now,
        None => true,
    }
}

fn sticky_pick(entries: &[EntryState], now: Instant) -> Option<usize> {
    entries.iter().position(|e| is_available(e, now))
}

fn round_robin_pick(entries: &[EntryState], cursor: usize, now: Instant) -> Option<usize> {
    let n = entries.len();
    if n == 0 {
        return None;
    }
    for i in 0..n {
        let idx = (cursor + i) % n;
        if is_available(&entries[idx], now) {
            return Some(idx);
        }
    }
    None
}

fn least_errors_pick(entries: &[EntryState], now: Instant) -> Option<usize> {
    let mut best: Option<(usize, u64)> = None;
    for (i, e) in entries.iter().enumerate() {
        if !is_available(e, now) {
            continue;
        }
        let f = e.failures;
        match best {
            None => best = Some((i, f)),
            Some((_, b)) if f < b => best = Some((i, f)),
            _ => {}
        }
    }
    best.map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, val: &str) -> PoolEntry {
        PoolEntry::from_env(name.to_string(), val.to_string())
    }

    fn pool(values: &[(&str, &str)], strat: SelectionStrategy) -> Pool {
        Pool::from_entries(
            "test",
            values.iter().map(|(n, v)| p(n, v)).collect(),
            strat,
        )
        .unwrap()
    }

    // ---- construction ------------------------------------------------------

    #[test]
    fn from_entries_rejects_empty() {
        let err = Pool::from_entries("z", vec![], SelectionStrategy::Sticky).unwrap_err();
        assert!(matches!(err, PoolError::Empty { .. }));
    }

    #[test]
    fn from_entries_rejects_too_many() {
        let entries: Vec<_> = (0..(MAX_POOL_SIZE + 1))
            .map(|i| PoolEntry::inline(format!("k{i}")))
            .collect();
        let err = Pool::from_entries("z", entries, SelectionStrategy::Sticky).unwrap_err();
        assert!(matches!(err, PoolError::InvalidEntry { .. }));
    }

    #[test]
    fn from_entries_rejects_empty_value() {
        let err = Pool::from_entries(
            "z",
            vec![PoolEntry::inline("ok"), PoolEntry::inline("   ")],
            SelectionStrategy::Sticky,
        )
        .unwrap_err();
        match err {
            PoolError::InvalidEntry { reason, .. } => {
                assert!(reason.contains("entry #1"), "got: {reason}");
            }
            other => panic!("expected InvalidEntry, got {other:?}"),
        }
    }

    #[test]
    fn from_sources_inline_only() {
        let pool = Pool::from_sources(
            "x",
            &[],
            &[],
            &["sk-a", "  ", "sk-b"],
            SelectionStrategy::Sticky,
        )
        .unwrap();
        assert_eq!(pool.len(), 2); // empty entry skipped
    }

    #[test]
    fn from_sources_env_resolves() {
        std::env::set_var("COS_TEST_POOL_KEY_A", "sk-aa");
        std::env::set_var("COS_TEST_POOL_KEY_B", "sk-bb");
        let pool = Pool::from_sources(
            "envpool",
            &[],
            &["COS_TEST_POOL_KEY_A", "COS_TEST_POOL_KEY_B"],
            &[],
            SelectionStrategy::RoundRobin,
        )
        .unwrap();
        assert_eq!(pool.len(), 2);
        std::env::remove_var("COS_TEST_POOL_KEY_A");
        std::env::remove_var("COS_TEST_POOL_KEY_B");
    }

    #[test]
    fn from_sources_skips_missing_env() {
        std::env::remove_var("COS_TEST_POOL_KEY_MISSING_X");
        let err = Pool::from_sources(
            "missing",
            &[],
            &["COS_TEST_POOL_KEY_MISSING_X"],
            &[],
            SelectionStrategy::Sticky,
        )
        .unwrap_err();
        assert!(matches!(err, PoolError::Empty { .. }));
    }

    // ---- selection: sticky -------------------------------------------------

    #[test]
    fn sticky_returns_first_repeatedly() {
        let pool = pool(&[("E1", "k1"), ("E2", "k2")], SelectionStrategy::Sticky);
        for _ in 0..5 {
            let l = pool.acquire().unwrap();
            assert_eq!(l.value(), "k1");
            pool.report_success(&l);
        }
    }

    #[test]
    fn sticky_skips_cooled_down_first_key() {
        let pool = pool(&[("E1", "k1"), ("E2", "k2")], SelectionStrategy::Sticky);
        let l1 = pool.acquire().unwrap();
        assert_eq!(l1.value(), "k1");
        pool.report_failure(&l1, FailureClass::CooldownWorthy);
        let l2 = pool.acquire().unwrap();
        assert_eq!(l2.value(), "k2");
    }

    // ---- selection: round-robin --------------------------------------------

    #[test]
    fn round_robin_rotates_through_all() {
        let pool = pool(
            &[("A", "kA"), ("B", "kB"), ("C", "kC")],
            SelectionStrategy::RoundRobin,
        );
        let mut seq = vec![];
        for _ in 0..6 {
            let l = pool.acquire().unwrap();
            seq.push(l.value().to_string());
            pool.report_success(&l);
        }
        assert_eq!(
            seq,
            vec![
                "kA".to_string(),
                "kB".to_string(),
                "kC".to_string(),
                "kA".to_string(),
                "kB".to_string(),
                "kC".to_string(),
            ]
        );
    }

    #[test]
    fn round_robin_skips_keys_in_cooldown() {
        let pool = pool(
            &[("A", "kA"), ("B", "kB"), ("C", "kC")],
            SelectionStrategy::RoundRobin,
        );
        let l = pool.acquire().unwrap();
        assert_eq!(l.value(), "kA");
        pool.report_failure(&l, FailureClass::CooldownWorthy);
        let l2 = pool.acquire().unwrap();
        assert_eq!(l2.value(), "kB");
        let l3 = pool.acquire().unwrap();
        assert_eq!(l3.value(), "kC");
        let l4 = pool.acquire().unwrap();
        // wraps around — kA still cooling down, picks kB
        assert_eq!(l4.value(), "kB");
    }

    // ---- selection: least-errors -------------------------------------------

    #[test]
    fn least_errors_picks_healthier_key() {
        let pool = pool(
            &[("A", "kA"), ("B", "kB")],
            SelectionStrategy::LeastErrors,
        );
        let l1 = pool.acquire().unwrap();
        // Tie -> first
        assert_eq!(l1.value(), "kA");
        pool.report_failure(&l1, FailureClass::Transient);
        let l2 = pool.acquire().unwrap();
        assert_eq!(l2.value(), "kB");
    }

    // ---- failure classes ---------------------------------------------------

    #[test]
    fn caller_error_does_not_count_as_failure() {
        let pool = pool(&[("A", "kA")], SelectionStrategy::Sticky);
        let l = pool.acquire().unwrap();
        pool.report_failure(&l, FailureClass::CallerError);
        let s = pool.stats();
        assert_eq!(s[0].failures, 0);
        assert_eq!(s[0].successes, 1);
    }

    #[test]
    fn transient_failure_no_cooldown() {
        let pool = pool(&[("A", "kA")], SelectionStrategy::Sticky);
        let l = pool.acquire().unwrap();
        pool.report_failure(&l, FailureClass::Transient);
        let s = pool.stats();
        assert_eq!(s[0].failures, 1);
        assert!(s[0].cooldown_remaining_ms.is_none());
    }

    #[test]
    fn cooldown_worthy_sets_cooldown() {
        let pool = pool(&[("A", "kA")], SelectionStrategy::Sticky);
        let l = pool.acquire().unwrap();
        pool.report_failure(&l, FailureClass::CooldownWorthy);
        let s = pool.stats();
        assert!(s[0].cooldown_remaining_ms.is_some());
    }

    #[test]
    fn all_cooling_down_returns_error_with_wait_hint() {
        let pool = pool(&[("A", "kA"), ("B", "kB")], SelectionStrategy::RoundRobin);
        let l1 = pool.acquire().unwrap();
        pool.report_failure(&l1, FailureClass::CooldownWorthy);
        let l2 = pool.acquire().unwrap();
        pool.report_failure(&l2, FailureClass::CooldownWorthy);
        let err = pool.acquire().unwrap_err();
        match err {
            PoolError::AllCoolingDown { wait_ms, size, .. } => {
                assert_eq!(size, 2);
                // 0 < wait_ms < cooldown duration
                assert!(wait_ms > 0);
                assert!(wait_ms <= DEFAULT_COOLDOWN.as_millis());
            }
            other => panic!("expected AllCoolingDown, got {other:?}"),
        }
    }

    #[test]
    fn cooldown_expires_at_specified_time() {
        let mut p_ = pool(&[("A", "kA")], SelectionStrategy::Sticky);
        p_.set_cooldown(Duration::from_millis(50));
        let now = Instant::now();
        let l = p_.acquire_at(now).unwrap();
        p_.report_failure_at(&l, FailureClass::CooldownWorthy, now);
        // Right at cooldown boundary -> blocked
        let err = p_.acquire_at(now + Duration::from_millis(49)).unwrap_err();
        assert!(matches!(err, PoolError::AllCoolingDown { .. }));
        // Past cooldown -> available
        let l2 = p_.acquire_at(now + Duration::from_millis(60)).unwrap();
        assert_eq!(l2.value(), "kA");
    }

    #[test]
    fn cooldown_zero_disables_cooldown() {
        let mut p_ = pool(&[("A", "kA")], SelectionStrategy::Sticky);
        p_.set_cooldown(Duration::ZERO);
        let l = p_.acquire().unwrap();
        p_.report_failure(&l, FailureClass::CooldownWorthy);
        // Counted as failure but immediately available again.
        let s = p_.stats();
        assert_eq!(s[0].failures, 1);
        assert!(s[0].cooldown_remaining_ms.is_none());
        let l2 = p_.acquire().unwrap();
        assert_eq!(l2.value(), "kA");
    }

    // ---- success report ----------------------------------------------------

    #[test]
    fn success_clears_last_failure_class_but_not_failures_count() {
        let pool = pool(&[("A", "kA")], SelectionStrategy::Sticky);
        let l = pool.acquire().unwrap();
        pool.report_failure(&l, FailureClass::Transient);
        let s = pool.stats();
        assert_eq!(s[0].last_failure_class, Some(FailureClass::Transient));
        let l2 = pool.acquire().unwrap();
        pool.report_success(&l2);
        let s2 = pool.stats();
        assert!(s2[0].last_failure_class.is_none());
        assert_eq!(s2[0].failures, 1, "lifetime failures preserved");
    }

    // ---- aggregate ---------------------------------------------------------

    #[test]
    fn aggregate_sums_across_keys() {
        let pool = pool(&[("A", "kA"), ("B", "kB")], SelectionStrategy::RoundRobin);
        for _ in 0..3 {
            let l = pool.acquire().unwrap();
            pool.report_success(&l);
        }
        let l = pool.acquire().unwrap();
        pool.report_failure(&l, FailureClass::Transient);
        let agg = pool.aggregate();
        assert_eq!(agg.size, 2);
        assert_eq!(agg.total_successes, 3);
        assert_eq!(agg.total_failures, 1);
        assert_eq!(agg.last_failure_by_class.get("transient"), Some(&1));
    }

    // ---- strategy parsing --------------------------------------------------

    #[test]
    fn strategy_str_parses_all_aliases() {
        assert_eq!(
            SelectionStrategy::from_str_lossy("rr"),
            SelectionStrategy::RoundRobin
        );
        assert_eq!(
            SelectionStrategy::from_str_lossy("ROUND_ROBIN"),
            SelectionStrategy::RoundRobin
        );
        assert_eq!(
            SelectionStrategy::from_str_lossy("least-errors"),
            SelectionStrategy::LeastErrors
        );
        assert_eq!(
            SelectionStrategy::from_str_lossy(""),
            SelectionStrategy::Sticky
        );
        assert_eq!(
            SelectionStrategy::from_str_lossy("anything-else"),
            SelectionStrategy::Sticky
        );
    }

    #[test]
    fn strategy_as_str_round_trips() {
        for s in [
            SelectionStrategy::Sticky,
            SelectionStrategy::RoundRobin,
            SelectionStrategy::LeastErrors,
        ] {
            assert_eq!(SelectionStrategy::from_str_lossy(s.as_str()), s);
        }
    }

    // ---- key source display ------------------------------------------------

    #[test]
    fn key_source_display_round_trips() {
        assert_eq!(
            KeySource::Credential("openai_a".into()).to_string(),
            "credential:openai_a"
        );
        assert_eq!(
            KeySource::Env("OPENAI_API_KEY".into()).to_string(),
            "env:OPENAI_API_KEY"
        );
        assert_eq!(KeySource::Inline.to_string(), "inline");
    }

    // ---- lease ------------------------------------------------------------

    #[test]
    fn lease_snapshots_value_at_acquire() {
        // Even after a parallel cooldown bumps the cursor, the
        // existing lease's value remains stable.
        let pool = pool(&[("A", "kA")], SelectionStrategy::Sticky);
        let l = pool.acquire().unwrap();
        assert_eq!(l.value(), "kA");
        assert_eq!(l.policy(), LeasePolicy::SnapshotValue);
    }

    #[test]
    fn lease_index_reflects_pool_position() {
        let pool = pool(&[("A", "kA"), ("B", "kB")], SelectionStrategy::RoundRobin);
        let l1 = pool.acquire().unwrap();
        assert_eq!(l1.index(), 0);
        pool.report_success(&l1);
        let l2 = pool.acquire().unwrap();
        assert_eq!(l2.index(), 1);
    }
}
