//! Capacity the journal reserves, and who may spend it.
//!
//! Availability is a security property here: if anything a model, a
//! tool or a peer can drive is able to fill a partition, it can stop the
//! record that says a privileged mutation finished, and the system loses
//! the ability to tell "committed" from "unknown".
//!
//! ## Classes
//!
//! | Class | Written for | May use the reserve |
//! | --- | --- | --- |
//! | [`QuotaClass::Closure`] | closing, flagging, resolving and recovering mutations; minimal session closure | yes |
//! | [`QuotaClass::Control`] | broker-side traffic an agent or peer can *drive*: mutation starts, capability use, approval mediation, prompt snapshots, turns | no |
//! | [`QuotaClass::Worker`] | the `agentd` private channel | no |
//! | [`QuotaClass::ContextIngest`] | `context.event.append` brackets | no |
//!
//! The classification is by *event kind first*, not by writer: a
//! capability-use record is broker-written but agent-triggered, so it is
//! `Control`, not privileged capacity. Only the records that retire or
//! recover a bracket are `Closure`.
//!
//! ## Why the reserve is computed, not guessed
//!
//! A fixed reserve is a guess about how many brackets can be open at
//! once. Instead the anchor counts open brackets, and admission for
//! every non-closure class requires that the space left after the append
//! still covers every outstanding bracket's worst case —
//! [`CLOSURE_RECORDS_PER_BRACKET`] records each — plus
//! [`RECOVERY_HEADROOM`] for the recovery pass itself. Opening a new
//! bracket must reserve for itself too, so a bracket can never be opened
//! that cannot be closed.
//!
//! The per-class ceilings below are a second, independent bound: their
//! sum is strictly less than the partition ceiling, so even ignoring the
//! computed reserve no combination of non-closure traffic can reach it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::acl::EventSource;
use super::event::JournalEvent;
use super::partition::{Anchor, Partition};
use super::record::MAX_RECORD_BYTES;
use super::JournalError;

/// Records one partition may ever hold.
pub const MAX_EVENTS_PER_PARTITION: u64 = 262_144;

/// Records `Control` traffic may contribute over a partition's life.
pub const MAX_CONTROL_EVENTS: u64 = 96_000;

/// Records the `agentd` worker channel may contribute.
pub const MAX_WORKER_EVENTS: u64 = 96_000;

/// Records `context.event.append` brackets may contribute.
pub const MAX_INGEST_EVENTS: u64 = 16_000;

/// Worst-case closure records one bracket can need: a definite outcome,
/// an indeterminate marker when that outcome could not be written, an
/// orphan record from the next recovery pass, and an operator's
/// resolution.
pub const CLOSURE_RECORDS_PER_BRACKET: u64 = 4;

/// Closure records recovery itself needs beyond the per-bracket count:
/// one scan record per pass plus retention bookkeeping.
pub const RECOVERY_HEADROOM: u64 = 64;

/// Bytes the active segment may reach before rotation is required.
///
/// Small in test builds so the rotation path is exercised by real
/// appends rather than by a hand-made anchor: a fabricated byte count
/// would not produce a segment that verifies, and the property under
/// test is precisely that a rotated chain still does.
#[cfg(not(test))]
pub const ROTATE_BYTES: u64 = 8 * 1024 * 1024;
#[cfg(test)]
pub const ROTATE_BYTES: u64 = 4 * 1024;

/// Bytes one partition may hold across every retained segment.
pub const MAX_PARTITION_BYTES: u64 = 64 * 1024 * 1024;

/// Partitions that may exist at once.
pub const MAX_PARTITIONS: usize = 4_096;

/// Records accepted per second for one partition, and the burst each
/// class may spend at once. Closure records are never rate limited:
/// delaying them is the failure this module exists to prevent.
const WORKER_RATE_PER_SEC: f64 = 32.0;
const WORKER_BURST: f64 = 128.0;
const CONTROL_RATE_PER_SEC: f64 = 64.0;
const CONTROL_BURST: f64 = 256.0;

/// Which budget an append is charged to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuotaClass {
    Closure,
    Control,
    Worker,
    ContextIngest,
}

impl QuotaClass {
    /// Classify an append.
    ///
    /// The event kind decides first, so a record that retires or
    /// recovers a bracket is `Closure` no matter which source asked for
    /// it, and everything else falls into the bounded class that matches
    /// who can drive it.
    pub fn of(event: &JournalEvent, source: EventSource, context_ingest: bool) -> Self {
        if event.is_closure() {
            return Self::Closure;
        }
        match source {
            EventSource::Worker => Self::Worker,
            _ if context_ingest => Self::ContextIngest,
            _ => Self::Control,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closure => "closure",
            Self::Control => "control",
            Self::Worker => "worker",
            Self::ContextIngest => "context-ingest",
        }
    }

    /// Whether records of this class may consume the space held back for
    /// closing outstanding brackets.
    pub fn may_use_reserve(self) -> bool {
        matches!(self, Self::Closure)
    }

    fn lifetime_ceiling(self) -> u64 {
        match self {
            Self::Closure => MAX_EVENTS_PER_PARTITION,
            Self::Control => MAX_CONTROL_EVENTS,
            Self::Worker => MAX_WORKER_EVENTS,
            Self::ContextIngest => MAX_INGEST_EVENTS,
        }
    }

    fn used(self, anchor: &Anchor) -> u64 {
        match self {
            Self::Closure => anchor.closure_events,
            Self::Control => anchor.control_events,
            Self::Worker => anchor.worker_events,
            Self::ContextIngest => anchor.ingest_events,
        }
    }
}

/// Records that must stay writable for the brackets currently open.
///
/// `extra_brackets` is 1 when the append under consideration would open
/// a new bracket, so a start can only be admitted when its own closure
/// is already paid for.
pub fn reserved_records(anchor: &Anchor, extra_brackets: u64) -> u64 {
    (u64::from(anchor.open_brackets).saturating_add(extra_brackets))
        .saturating_mul(CLOSURE_RECORDS_PER_BRACKET)
        .saturating_add(RECOVERY_HEADROOM)
}

/// Bytes that must stay writable for the same reason.
pub fn reserved_bytes(anchor: &Anchor, extra_brackets: u64) -> u64 {
    reserved_records(anchor, extra_brackets).saturating_mul(MAX_RECORD_BYTES as u64)
}

/// Check that one more record of `class`, `bytes` long, fits.
pub fn check(
    anchor: &Anchor,
    class: QuotaClass,
    opens_bracket: bool,
    bytes: u64,
) -> Result<(), JournalError> {
    if anchor.events.saturating_add(1) > MAX_EVENTS_PER_PARTITION {
        return Err(JournalError::Quota(format!(
            "journal partition {} is at its {MAX_EVENTS_PER_PARTITION} event ceiling",
            anchor.partition
        )));
    }
    if class.used(anchor).saturating_add(1) > class.lifetime_ceiling() {
        return Err(JournalError::Quota(format!(
            "journal partition {} is at its {} class ceiling",
            anchor.partition,
            class.as_str()
        )));
    }

    if class.may_use_reserve() {
        if anchor.total_bytes.saturating_add(bytes) > MAX_PARTITION_BYTES {
            return Err(JournalError::Quota(format!(
                "journal partition {} is at its byte ceiling",
                anchor.partition
            )));
        }
        return Ok(());
    }

    let extra = u64::from(opens_bracket);
    let remaining_records = MAX_EVENTS_PER_PARTITION
        .saturating_sub(anchor.events)
        .saturating_sub(1);
    let needed_records = reserved_records(anchor, extra);
    if remaining_records < needed_records {
        return Err(JournalError::Quota(format!(
            "journal partition {} must keep {needed_records} record(s) free to close {} \
             outstanding mutation(s); a {} record cannot take them",
            anchor.partition,
            anchor.open_brackets,
            class.as_str()
        )));
    }

    let remaining_bytes = MAX_PARTITION_BYTES
        .saturating_sub(anchor.total_bytes)
        .saturating_sub(bytes);
    let needed_bytes = reserved_bytes(anchor, extra);
    if remaining_bytes < needed_bytes {
        return Err(JournalError::Quota(format!(
            "journal partition {} must keep {needed_bytes} byte(s) free to close {} \
             outstanding mutation(s); a {} record cannot take them",
            anchor.partition,
            anchor.open_brackets,
            class.as_str()
        )));
    }
    Ok(())
}

/// Token bucket bounding how fast one partition accepts records of a
/// rate-limited class. In memory by design: it smooths a looping model
/// or a chatty peer, while the durable ceilings above bound total
/// volume.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    refreshed: Instant,
}

type Buckets = HashMap<(String, QuotaClass), Bucket>;

static BUCKETS: Mutex<Option<Buckets>> = Mutex::new(None);

/// Spend one token for `partition` in `class`, or refuse.
///
/// `Closure` is exempt: rate limiting a record that closes a mutation
/// would recreate the availability failure this module prevents.
pub fn admit_rate(partition: &Partition, class: QuotaClass) -> Result<(), JournalError> {
    let (rate, burst) = match class {
        QuotaClass::Closure => return Ok(()),
        QuotaClass::Worker => (WORKER_RATE_PER_SEC, WORKER_BURST),
        QuotaClass::Control | QuotaClass::ContextIngest => (CONTROL_RATE_PER_SEC, CONTROL_BURST),
    };
    let key = (partition.key(), class);
    let now = Instant::now();
    let mut guard = BUCKETS
        .lock()
        .map_err(|_| JournalError::Quota("journal rate limiter is poisoned".to_string()))?;
    let buckets = guard.get_or_insert_with(HashMap::new);
    if buckets.len() > MAX_PARTITIONS {
        buckets.retain(|_, bucket| now.duration_since(bucket.refreshed) < Duration::from_secs(300));
    }
    let bucket = buckets.entry(key).or_insert(Bucket {
        tokens: burst,
        refreshed: now,
    });
    let elapsed = now.duration_since(bucket.refreshed).as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * rate).min(burst);
    bucket.refreshed = now;
    if bucket.tokens < 1.0 {
        return Err(JournalError::Quota(format!(
            "journal partition {partition} is over its {} event rate",
            class.as_str()
        )));
    }
    bucket.tokens -= 1.0;
    Ok(())
}

/// Forget every rate bucket. Used when a test rebinds the data
/// directory so one test's burst is not charged to the next.
#[cfg(test)]
pub fn reset_rate_limits() {
    if let Ok(mut guard) = BUCKETS.lock() {
        *guard = None;
    }
}

/// The per-class ceilings must not be able to reach the partition
/// ceiling even if the computed reserve were ignored.
const _: () = assert!(
    MAX_CONTROL_EVENTS + MAX_WORKER_EVENTS + MAX_INGEST_EVENTS < MAX_EVENTS_PER_PARTITION,
    "non-closure classes must not be able to fill a partition"
);

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/quota.rs"
    ));
}
