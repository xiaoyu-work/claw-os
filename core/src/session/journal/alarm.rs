//! The bounded alarm channel.
//!
//! A read-only or health request must not be turned into an outage by a
//! journal that cannot write, but "carry on quietly" is how an attacker
//! disables evidence. So a failure that is not itself fatal raises an
//! alarm on a channel that does not depend on the journal: a
//! root-only file of its own, the daemon's `tracing` error stream, and
//! the user-visible system operations journal.
//!
//! Alarms are bounded on two axes so a loop cannot turn the alarm into
//! the denial of service it is reporting: the file is trimmed to
//! [`MAX_RETAINED`] records, and each class is written at most once per
//! [`REARM`] window with a suppression count carried on the next one.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What went wrong. Stable, input-free, and the key the rate limiter
/// works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    /// An append the caller was allowed to continue past failed.
    AppendFailed,
    /// Bytes past the committed head were discarded.
    TornAppend,
    /// A chain, head or key check failed.
    IntegrityFailed,
    /// A source asked for an event kind it may not write.
    AclViolation,
    /// A partition is at a capacity ceiling.
    QuotaExhausted,
    /// A mutation ran and its completion could not be recorded.
    MutationIndeterminate,
    /// Recovery found a bracket a previous daemon never closed.
    OrphanedMutation,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppendFailed => "journal.append-failed",
            Self::TornAppend => "journal.torn-append",
            Self::IntegrityFailed => "journal.integrity-failed",
            Self::AclViolation => "journal.acl-violation",
            Self::QuotaExhausted => "journal.quota-exhausted",
            Self::MutationIndeterminate => "journal.mutation-indeterminate",
            Self::OrphanedMutation => "journal.orphaned-mutation",
        }
    }
}

/// Records kept in the alarm file.
pub const MAX_RETAINED: usize = 1_024;

/// Minimum gap between two alarms of the same class.
const REARM: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct Gate {
    last: Instant,
    suppressed: u64,
}

static GATES: Mutex<Option<HashMap<Class, Gate>>> = Mutex::new(None);

/// Raise one alarm.
///
/// `partition` and `detail` are produced by this crate, never by a
/// caller: `detail` is assembled from counters and stable classes, so
/// the alarm file cannot become a place model or peer text is stored.
pub fn raise(class: Class, partition: &str, detail: &str) {
    let Some(suppressed) = admit(class) else {
        return;
    };
    let record = serde_json::json!({
        "ts": chrono::Utc::now(),
        "event": "journal.alarm",
        "class": class.as_str(),
        "partition": crate::audit_policy::safe_reference(partition),
        "detail": detail,
        "suppressed_since_last": suppressed,
    });

    tracing::error!(
        class = class.as_str(),
        partition = %crate::audit_policy::safe_reference(partition),
        suppressed_since_last = suppressed,
        "session journal alarm: {detail}"
    );
    if let Err(error) = append(&record) {
        tracing::error!(error = %error, "failed to write the session journal alarm file");
    }
    crate::clawd::system_journal::record_journal_alarm(class.as_str(), partition, detail);
}

fn admit(class: Class) -> Option<u64> {
    let now = Instant::now();
    let mut guard = GATES.lock().ok()?;
    let gates = guard.get_or_insert_with(HashMap::new);
    match gates.get_mut(&class) {
        Some(gate) if now.duration_since(gate.last) < REARM => {
            gate.suppressed = gate.suppressed.saturating_add(1);
            None
        }
        Some(gate) => {
            let suppressed = std::mem::take(&mut gate.suppressed);
            gate.last = now;
            Some(suppressed)
        }
        None => {
            gates.insert(
                class,
                Gate {
                    last: now,
                    suppressed: 0,
                },
            );
            Some(0)
        }
    }
}

/// Path of the alarm file: root-owned, private, and deliberately not
/// inside a partition directory so a damaged partition is still able to
/// report itself.
pub fn path() -> std::path::PathBuf {
    super::root().join("alarms.jsonl")
}

fn append(record: &serde_json::Value) -> Result<(), String> {
    let path = path();
    if let Some(parent) = path.parent() {
        crate::storage::ensure_private_dir(parent)
            .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    }
    let line = serde_json::to_string(record).map_err(|error| error.to_string())?;
    crate::filelock::with_exclusive_path_lock(&path, || {
        let mut retained: Vec<String> = match std::fs::read_to_string(&path) {
            Ok(data) => data
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("read {}: {error}", path.display())),
        };
        retained.push(line.clone());
        if retained.len() > MAX_RETAINED {
            retained.drain(..retained.len() - MAX_RETAINED);
        }
        let mut body = retained.join("\n");
        body.push('\n');
        super::write_durable(&path, body.as_bytes()).map_err(|error| error.to_string())
    })
}

/// The most recent alarms, newest last. Diagnostics stay queryable even
/// when the chain they describe does not verify.
pub fn recent(limit: usize) -> Vec<serde_json::Value> {
    let Ok(data) = std::fs::read_to_string(path()) else {
        return Vec::new();
    };
    let mut out: Vec<serde_json::Value> = data
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if out.len() > limit {
        out.drain(..out.len() - limit);
    }
    out
}

/// Forget rate-limiter state so one test's alarms do not suppress the
/// next test's.
#[cfg(test)]
pub fn reset() {
    if let Ok(mut guard) = GATES.lock() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/alarm.rs"
    ));
}
