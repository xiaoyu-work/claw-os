//! Credential pool — multi-key rotation for LLM providers.
//!
//! When you have several API keys for the same provider (e.g. three
//! OpenAI keys for a higher aggregate rate limit, or a primary +
//! backup pair for failover), a single `api_key_credential` /
//! `api_key_env` field is too narrow. This module provides a typed
//! pool that:
//!
//!   * Receives N pre-resolved keys at construction.
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
use std::sync::{Arc, Mutex};
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

    #[error("credential pool '{name}' state is unavailable because its lock was poisoned")]
    StatePoisoned { name: String },

    #[error("credential pool '{name}' could not load credential '{credential}'")]
    CredentialSource {
        name: String,
        credential: String,
        #[source]
        source: crate::credential::CredentialError,
    },
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
            .field("len", &self.state.lock().ok().map(|s| s.entries.len()))
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
                reason: format!("pool size {} exceeds max {MAX_POOL_SIZE}", entries.len()),
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

    /// Legacy process-backed constructor. New provider composition resolves
    /// sources through `construction::resolve_api_credentials` and passes
    /// entries to [`Self::from_entries`].
    pub fn from_sources(
        name: impl Into<String>,
        credential_names: &[&str],
        env_names: &[&str],
        inline: &[&str],
        strategy: SelectionStrategy,
    ) -> Result<Self, PoolError> {
        use crate::agent::llm::construction::CredentialSource;

        let source = crate::agent::llm::construction::ProcessCredentialSource;
        let mut entries = Vec::new();
        let name = name.into();
        for credential in credential_names {
            match source.load_stored(credential) {
                Ok(Some(value)) => {
                    let value = value.trim();
                    if !value.is_empty() {
                        entries.push(PoolEntry::from_credential(*credential, value.to_string()));
                    }
                }
                Ok(None) => {}
                Err(source) => {
                    return Err(PoolError::CredentialSource {
                        name,
                        credential: (*credential).to_string(),
                        source: crate::credential::CredentialError::external(
                            "credential.source",
                            source,
                        ),
                    })
                }
            }
        }
        for environment in env_names {
            if let Some(value) = source.load_environment(environment) {
                let value = value.trim();
                if !value.is_empty() {
                    entries.push(PoolEntry::from_env(*environment, value.to_string()));
                }
            }
        }
        for value in inline {
            let value = value.trim();
            if !value.is_empty() {
                entries.push(PoolEntry::inline(value.to_string()));
            }
        }
        Self::from_entries(name, entries, strategy)
    }

    /// Legacy declaration helper retained for source compatibility.
    pub fn is_declared(cfg: &crate::config::AgentConfig) -> bool {
        crate::agent::llm::construction::ApiCredentialConfig::from_agent_config(cfg).pool_declared()
    }

    /// Legacy public constructor with source-preserving process resolution.
    /// New provider composition injects `TypedCredentialSource` directly.
    pub fn try_from_agent_config(
        name: impl Into<String>,
        cfg: &crate::config::AgentConfig,
    ) -> crate::agent::llm::Result<Option<Self>> {
        if !Self::is_declared(cfg) {
            return Ok(None);
        }
        let source = crate::agent::llm::construction::ProcessCredentialSource;
        let resolved = crate::agent::llm::construction::try_resolve_api_credentials(
            name,
            crate::agent::llm::construction::ApiCredentialConfig::from_agent_config(cfg),
            &source,
        )?;
        match resolved.pool {
            Some(pool) => Arc::try_unwrap(pool).map(Some).map_err(|_| {
                crate::agent::llm::LlmError::Internal(
                    "newly resolved credential pool was unexpectedly shared".to_string(),
                )
            }),
            None => Ok(None),
        }
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
        self.try_len().unwrap_or_else(|error| {
            tracing::error!(error = %error, "credential pool length unavailable");
            0
        })
    }

    pub fn try_len(&self) -> Result<usize, PoolError> {
        Ok(self.lock_state()?.entries.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn try_is_empty(&self) -> Result<bool, PoolError> {
        self.try_len().map(|len| len == 0)
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
        let mut st = self.lock_state()?;
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
        if let Err(error) = self.try_report_success(lease) {
            tracing::error!(error = %error, "credential pool success accounting unavailable");
        }
    }

    pub fn try_report_success(&self, lease: &Lease) -> Result<(), PoolError> {
        let mut st = self.lock_state()?;
        if let Some(e) = st.entries.get_mut(lease.index) {
            e.successes = e.successes.saturating_add(1);
            // A success clears any past failure class so subsequent
            // sticky decisions don't keep flipping. (We deliberately
            // don't reset failures count -- it's lifetime-cumulative
            // for diagnostics.)
            e.last_failure_class = None;
        }
        Ok(())
    }

    /// Tell the pool that the request using this lease failed.
    /// Cooldown is applied if `class == CooldownWorthy`.
    pub fn report_failure(&self, lease: &Lease, class: FailureClass) {
        if let Err(error) = self.try_report_failure(lease, class) {
            tracing::error!(error = %error, "credential pool failure accounting unavailable");
        }
    }

    pub fn try_report_failure(&self, lease: &Lease, class: FailureClass) -> Result<(), PoolError> {
        self.try_report_failure_at(lease, class, Instant::now())
    }

    pub fn report_failure_at(&self, lease: &Lease, class: FailureClass, now: Instant) {
        if let Err(error) = self.try_report_failure_at(lease, class, now) {
            tracing::error!(error = %error, "credential pool failure accounting unavailable");
        }
    }

    pub fn try_report_failure_at(
        &self,
        lease: &Lease,
        class: FailureClass,
        now: Instant,
    ) -> Result<(), PoolError> {
        let mut st = self.lock_state()?;
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
        Ok(())
    }

    /// Snapshot of every key's health.
    pub fn stats(&self) -> Vec<KeyStats> {
        self.try_stats().unwrap_or_else(|error| {
            tracing::error!(error = %error, "credential pool stats unavailable");
            Vec::new()
        })
    }

    pub fn try_stats(&self) -> Result<Vec<KeyStats>, PoolError> {
        self.try_stats_at(Instant::now())
    }

    pub fn stats_at(&self, now: Instant) -> Vec<KeyStats> {
        self.try_stats_at(now).unwrap_or_else(|error| {
            tracing::error!(error = %error, "credential pool stats unavailable");
            Vec::new()
        })
    }

    pub fn try_stats_at(&self, now: Instant) -> Result<Vec<KeyStats>, PoolError> {
        let st = self.lock_state()?;
        Ok(st
            .entries
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
            .collect())
    }

    /// Group lifetime totals across the whole pool.
    pub fn aggregate(&self) -> AggregateStats {
        self.try_aggregate().unwrap_or_else(|error| {
            tracing::error!(error = %error, "credential pool aggregate unavailable");
            AggregateStats {
                size: 0,
                total_successes: 0,
                total_failures: 0,
                last_failure_by_class: HashMap::new(),
            }
        })
    }

    pub fn try_aggregate(&self) -> Result<AggregateStats, PoolError> {
        let st = self.lock_state()?;
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
        Ok(AggregateStats {
            size: st.entries.len(),
            total_successes,
            total_failures,
            last_failure_by_class: by_class,
        })
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, PoolState>, PoolError> {
        self.state.lock().map_err(|_| PoolError::StatePoisoned {
            name: self.name.clone(),
        })
    }

    #[cfg(test)]
    fn poison_for_test(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = self.state.lock().unwrap();
            panic!("poison credential pool");
        }));
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/credential_pool.rs"
    ));
}
