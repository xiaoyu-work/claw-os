//! System-vitals heartbeat — the proactive "autonomic nervous system" of
//! clawd.
//!
//! ## Why this is a *system* heartbeat, not an *app* heartbeat
//!
//! An app-style assistant implements "proactivity" by waking an LLM on a
//! fixed timer to ask "is there anything I should tell the user?". That
//! burns tokens every tick, is user-facing, and is blind to the machine
//! it runs on.
//!
//! ClawOS is the operating system, so it does the opposite: a cheap,
//! always-on reflex loop samples real kernel vitals (load, memory, …)
//! every `interval`, and **only emits a `context.event` when something
//! actually crosses a threshold**. The LLM is never called here. Waking
//! the cognitive layer is deferred to the trigger engine
//! ([`crate::triggers`]): a rule may match an emitted event and enqueue
//! an agent job. No matching rule ⇒ no inference ⇒ no token cost. That
//! keeps a 24/7 daemon sustainable and local-first.
//!
//! ```text
//!   heartbeat (this)         triggers              agent worker
//!   sample /proc  ──emit──▶  rule match?  ──job──▶ reason + act (caps-gated)
//!   threshold+debounce       (decision)            (execution)
//! ```
//!
//! The same loop also drives `cron tick` and `triggers tick`, so the
//! scheduler runs without an external systemd timer — the daemon is its
//! own clock.
//!
//! ## Guardrails
//!
//! - **No LLM here.** Only event emission + tick driving.
//! - **Per-signal cooldown.** A flapping vital can't spam the event log
//!   (and therefore can't spam agent jobs); each signal key has a
//!   minimum re-fire interval.
//! - **Edge-triggered.** An event fires on the *crossing* into a bad
//!   state, plus periodic re-fires bounded by the cooldown — not on
//!   every beat while unhealthy.
//! - **Graceful degradation.** Missing `/proc` (non-Linux dev hosts)
//!   simply yields no vitals; the tick-driving half still runs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::client_identity::ClientIdentity;
use crate::caps::{Cap, CapSet, Role, Scope, Verb};

/// Source tag stamped on every heartbeat-emitted `context.event`. Trigger
/// rules match on this to react to system conditions.
pub const SOURCE: &str = "heartbeat";

/// Tunables, resolved from the environment at start (kept simple — a
/// daemon-internal loop doesn't need a full config surface).
#[derive(Debug, Clone)]
pub struct HeartbeatConfig {
    pub enabled: bool,
    pub interval: Duration,
    /// Warn when 1-minute load average per CPU exceeds this.
    pub load_per_core_warn: f64,
    /// Warn when available memory drops below this fraction of total.
    pub mem_avail_warn_ratio: f64,
    /// Minimum gap between two emissions of the same signal key.
    pub cooldown: Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(60),
            load_per_core_warn: 4.0,
            mem_avail_warn_ratio: 0.10,
            cooldown: Duration::from_secs(900),
        }
    }
}

impl HeartbeatConfig {
    /// Resolve from env. `CLAWD_HEARTBEAT=off|0|false` disables it;
    /// `CLAWD_HEARTBEAT_INTERVAL_SECS` overrides the cadence.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("CLAWD_HEARTBEAT") {
            let v = v.trim().to_ascii_lowercase();
            if matches!(v.as_str(), "off" | "0" | "false" | "no") {
                cfg.enabled = false;
            }
        }
        if let Some(secs) = std::env::var("CLAWD_HEARTBEAT_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
        {
            cfg.interval = Duration::from_secs(secs);
        }
        cfg
    }
}

/// A single sampled vital plus the severity decision for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// Stable key for cooldown bookkeeping + the emitted `event_type`.
    pub key: &'static str,
    pub severity: Severity,
    pub message: String,
    pub detail: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Healthy — nothing to emit.
    Ok,
    /// Worth recording / possibly notifying.
    Warn,
    /// Worth waking the agent (if a rule matches).
    Critical,
}

/// Sampled machine vitals. Fields are `None` when the source is
/// unavailable (e.g. `/proc` missing on a non-Linux dev host).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vitals {
    /// 1-minute load average.
    pub load1: Option<f64>,
    /// Logical CPU count, for normalising load.
    pub cpus: Option<usize>,
    /// Available-memory fraction in `0.0..=1.0`.
    pub mem_avail_ratio: Option<f64>,
}

/// Read `/proc/loadavg` → 1-minute load average.
fn read_load1() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/loadavg").ok()?;
    s.split_whitespace().next()?.parse().ok()
}

/// Count logical CPUs via the std API (falls back to 1).
fn read_cpus() -> Option<usize> {
    std::thread::available_parallelism().ok().map(|n| n.get())
}

/// Read `/proc/meminfo` → available-memory fraction.
fn read_mem_avail_ratio() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut avail = None;
    for line in s.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("MemTotal:") => total = it.next().and_then(|v| v.parse::<f64>().ok()),
            Some("MemAvailable:") => avail = it.next().and_then(|v| v.parse::<f64>().ok()),
            _ => {}
        }
    }
    match (total, avail) {
        (Some(t), Some(a)) if t > 0.0 => Some(a / t),
        _ => None,
    }
}

/// Sample current vitals from the kernel.
pub fn sample() -> Vitals {
    Vitals {
        load1: read_load1(),
        cpus: read_cpus(),
        mem_avail_ratio: read_mem_avail_ratio(),
    }
}

/// Map vitals → signals, given thresholds. Pure: no I/O, fully testable.
/// Returns one entry per checked vital; `Severity::Ok` entries are
/// filtered out before emission by the caller.
pub fn evaluate(v: &Vitals, cfg: &HeartbeatConfig) -> Vec<Signal> {
    let mut out = Vec::new();

    if let (Some(load), Some(cpus)) = (v.load1, v.cpus) {
        let per_core = if cpus > 0 { load / cpus as f64 } else { load };
        let severity = if per_core >= cfg.load_per_core_warn * 2.0 {
            Severity::Critical
        } else if per_core >= cfg.load_per_core_warn {
            Severity::Warn
        } else {
            Severity::Ok
        };
        if severity != Severity::Ok {
            out.push(Signal {
                key: "load_high",
                severity,
                message: format!(
                    "system load is high: {load:.2} over {cpus} CPUs ({per_core:.2}/core)"
                ),
                detail: json!({ "load1": load, "cpus": cpus, "per_core": per_core }),
            });
        }
    }

    if let Some(ratio) = v.mem_avail_ratio {
        let severity = if ratio <= cfg.mem_avail_warn_ratio / 2.0 {
            Severity::Critical
        } else if ratio <= cfg.mem_avail_warn_ratio {
            Severity::Warn
        } else {
            Severity::Ok
        };
        if severity != Severity::Ok {
            out.push(Signal {
                key: "memory_low",
                severity,
                message: format!(
                    "available memory is low: {:.1}% free",
                    ratio * 100.0
                ),
                detail: json!({ "mem_avail_ratio": ratio }),
            });
        }
    }

    out
}

/// Tracks per-signal last-fire times so a persistently-bad vital
/// re-emits at most once per cooldown window (storm protection).
#[derive(Default)]
pub struct CooldownState {
    last_fired: HashMap<&'static str, Instant>,
}

impl CooldownState {
    /// Returns true if `key` may fire now (never fired, or cooldown
    /// elapsed). Records the fire time when it returns true.
    pub fn allow(&mut self, key: &'static str, cooldown: Duration, now: Instant) -> bool {
        match self.last_fired.get(&key) {
            Some(&t) if now.duration_since(t) < cooldown => false,
            _ => {
                self.last_fired.insert(key, now);
                true
            }
        }
    }
}

/// Emit one heartbeat signal as a `context.event`. Best-effort: a failed
/// write is logged and swallowed so the loop keeps running.
fn emit_signal(sig: &Signal) {
    let event_type = match sig.severity {
        Severity::Critical => format!("{}.critical", sig.key),
        _ => format!("{}.warn", sig.key),
    };
    let params = json!({
        "source": SOURCE,
        "event_type": event_type,
        "payload": {
            "severity": match sig.severity {
                Severity::Critical => "critical",
                Severity::Warn => "warn",
                Severity::Ok => "ok",
            },
            "message": sig.message,
            "vitals": sig.detail,
        },
    });
    if let Err(e) = super::context_events::append(params, &ClientIdentity::unknown()) {
        tracing::warn!(error = %e, key = sig.key, "heartbeat: failed to emit context.event");
    }
}

/// One heartbeat beat: sample, evaluate, and emit while respecting
/// cooldowns. Scheduler execution has its own supervised loop so a long
/// proactive job cannot stop vitals sampling.
pub fn beat(cfg: &HeartbeatConfig, cooldowns: &mut CooldownState, now: Instant) {
    beat_vitals(cfg, cooldowns, now);
}

fn beat_vitals(cfg: &HeartbeatConfig, cooldowns: &mut CooldownState, now: Instant) {
    let vitals = sample();
    for sig in evaluate(&vitals, cfg) {
        if cooldowns.allow(sig.key, cfg.cooldown, now) {
            emit_signal(&sig);
        }
    }
}

/// Run the heartbeat loop until `shutdown` flips. Intended to be spawned
/// once by `clawd` alongside the agent worker. Async so it shares the
/// daemon's tokio runtime and uses a non-blocking interval timer.
pub async fn run_loop(cfg: HeartbeatConfig, shutdown: Arc<AtomicBool>) {
    if !cfg.enabled {
        tracing::info!("heartbeat disabled (CLAWD_HEARTBEAT=off)");
        return;
    }
    tracing::info!(
        interval_secs = cfg.interval.as_secs(),
        "heartbeat started — system-vitals reflex loop"
    );
    let scheduler_interval = cfg.interval;
    tokio::join!(
        run_loop_scoped(cfg, Arc::clone(&shutdown)),
        run_scheduler_loop(scheduler_interval, shutdown),
    );
}

async fn run_loop_scoped(cfg: HeartbeatConfig, shutdown: Arc<AtomicBool>) {
    let mut cooldowns = CooldownState::default();
    let mut ticker = tokio::time::interval(cfg.interval);
    // Skip missed ticks rather than bursting to catch up after a stall.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("heartbeat stopping");
            return;
        }
        beat_vitals(&cfg, &mut cooldowns, Instant::now());
    }
}

async fn run_scheduler_loop(interval: Duration, shutdown: Arc<AtomicBool>) {
    let scheduler = match SchedulerSession::register() {
        Ok(session) => session,
        Err(error) => {
            tracing::error!(error = %error, "heartbeat scheduler session unavailable");
            return;
        }
    };
    tokio::join!(
        run_scheduler_subsystem(
            "cron",
            scheduler.id.clone(),
            interval,
            Arc::clone(&shutdown),
        ),
        run_scheduler_subsystem("triggers", scheduler.id.clone(), interval, shutdown),
    );
}

async fn run_scheduler_subsystem(
    subsystem: &'static str,
    session_id: String,
    interval: Duration,
    shutdown: Arc<AtomicBool>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        drive_scheduler_blocking(subsystem, session_id.clone()).await;
    }
}

async fn drive_scheduler_blocking(subsystem: &'static str, session_id: String) {
    let result = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|error| format!("create heartbeat scheduler runtime: {error}"))?;
        runtime.block_on(crate::proc::with_session_override(session_id, async {
            match subsystem {
                "cron" => crate::cron::run("tick", &[]).map(|_| ()),
                "triggers" => crate::triggers::run("tick", &[]).map(|_| ()),
                _ => unreachable!(),
            }
        }))
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(subsystem, error = %error, "heartbeat scheduler executor failed")
        }
        Err(error) => {
            tracing::warn!(subsystem, error = %error, "heartbeat scheduler task failed")
        }
    }
}

struct SchedulerSession {
    id: String,
}

impl SchedulerSession {
    fn register() -> Result<Self, String> {
        let id = format!("scheduler-{}", uuid::Uuid::new_v4().simple());
        let mut caps = CapSet::new();
        caps.insert(Cap::new(Verb::SYS_KERNEL, Scope::Wild));
        let info = crate::proc::SessionInfo {
            session_id: id.clone(),
            pid: std::process::id(),
            command: vec!["clawd-heartbeat".to_string()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some("scheduler".to_string()),
            parent: None,
            workdir: None,
            exit_code: None,
            ended_at: None,
            tier: Some(Role::Kernel.credential_tier()),
            scope: Some("scheduler".to_string()),
            priority: None,
            caps: Some(caps),
            transient_caps: None,
            role: Some(Role::Kernel.name().to_string()),
            app_id: None,
            pending_bind: false,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        };
        crate::proc::register_session(info)?;
        Ok(Self { id })
    }
}

impl Drop for SchedulerSession {
    fn drop(&mut self) {
        crate::proc::deregister_session(&self.id);
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/heartbeat.rs"
    ));
}
