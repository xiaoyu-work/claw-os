//! The authoritative session event journal.
//!
//! ## What it is
//!
//! One ordered, signed, append-only chain per session (and one per
//! owner for privileged work that has no session), owned by root
//! `clawd`. It answers two questions nothing else in the system can
//! answer on its own:
//!
//! 1. **What happened, in what order?** Conversation bodies, the memory
//!    database, the broker audit log and the system operations journal
//!    each hold a slice of a session's story, in their own order, with
//!    their own retention. The chain is the single ordering, and the
//!    others become projections of it or carry references into it.
//! 2. **Did that privileged mutation finish?** Every `Kind::Mutation`
//!    route appends a durable start before it may touch anything, and a
//!    committed/failed record once the effect's outcome is known. A
//!    crash between the two is visible as an orphan rather than as
//!    silence.
//!
//! ## What it is not
//!
//! It is **not** authority. Replaying a `CapabilityIssued` record grants
//! nothing: the capability authority holds its own live grants, bound to
//! processes and use budgets, and an approval is only usable through the
//! approvals store's own generation counters. The chain stores keyed,
//! non-reversible references to those decisions and their outcomes, so
//! an attacker who can write the log — which, by construction, means
//! root — still cannot mint a grant by writing one down.
//!
//! It is also not a content store. Prompts, model output, tool input and
//! error text never enter it; see [`event`] for the two reference shapes
//! that carry them instead.
//!
//! ## Layout
//!
//! ```text
//! $COS_DATA_DIR/journal/
//!   keys/<key-id>.key      root-only MAC keys, 0600, create_new
//!   keys/active.json       which key signs new records
//!   writer.lock            flock: one writer per machine
//!   writer.json            monotonic writer epoch, signed
//!   alarms.jsonl           bounded, independent failure channel
//!   sessions/<sid>/events.jsonl   the chain
//!   sessions/<sid>/anchor.json    committed head, signed
//!   sessions/<sid>/archive/       rotated segments
//!   owners/<uid>/...              same shape, no session
//! ```
//!
//! ## Reading order for a change here
//!
//! [`event`] for the schema, [`acl`] for who may write what, [`record`]
//! for the bytes the MAC covers, [`writer`] for the single-writer and
//! durability rules, [`recovery`] for what happens after a crash.

pub mod acl;
pub mod alarm;
pub mod event;
pub mod keyring;
pub mod partition;
pub mod projection;
pub mod quota;
pub mod reader;
pub mod record;
pub mod recovery;
pub mod writer;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub use acl::EventSource;
pub use event::{
    ApprovalOutcome, CancelSource, ContentRef, ContentStore, Digest, GrantEnd, Indeterminate,
    JournalEvent, Label, OperationId, Origin, RecoverySource, Reference, Resolution,
    RetentionReason, SegmentKind, SessionOutcome, Trust, SCHEMA_VERSION,
};
pub use partition::{Anchor, Partition};
pub use reader::{Chain, Health};
pub use record::JournalRecord;
pub use recovery::Unresolved;
pub use writer::{Append, Appended, WriterLease};

/// Root of the journal tree.
pub fn root() -> PathBuf {
    crate::paths::session_journal_dir()
}

/// Everything the journal can refuse to do.
///
/// The split matters at the mutation boundary: [`Self::HeadUncommitted`]
/// after a side effect means the outcome is *unknown*, while every other
/// variant on the start path means nothing was dispatched.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("journal key: {0}")]
    Key(String),

    #[error("journal io on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("journal integrity: {0}")]
    Integrity(String),

    #[error("journal encode: {0}")]
    Encode(String),

    #[error("journal quota: {0}")]
    Quota(String),

    #[error("journal writer: {0}")]
    Writer(String),

    #[error("a {writer} caller may not record a {event} event")]
    Forbidden {
        writer: &'static str,
        event: &'static str,
    },

    #[error(
        "journal partition {partition} committed {committed_bytes} byte(s) but only \
         {found_bytes} remain; the chain was truncated"
    )]
    Truncated {
        partition: String,
        committed_bytes: u64,
        found_bytes: u64,
    },

    #[error(
        "journal partition {partition} was committed by writer epoch {committed_epoch}; \
         this writer holds epoch {writer_epoch}"
    )]
    StaleWriter {
        partition: String,
        committed_epoch: u64,
        writer_epoch: u64,
    },

    #[error("journal head for {partition} at seq {seq} was not committed: {detail}")]
    HeadUncommitted {
        partition: String,
        seq: u64,
        detail: String,
    },

    #[error(
        "journal partition {partition} still holds chain bytes but its committed head is \
         missing; the chain is preserved and mutations fail closed until an operator resolves it"
    )]
    AnchorMissing { partition: String },

    #[error("journal partition {0} is quarantined after a failed integrity check")]
    Quarantined(String),

    #[error("journal operation {operation} in {partition} is not an unresolved mutation")]
    NotUnresolved {
        partition: String,
        operation: String,
    },
}

impl JournalError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn key(path: &Path, source: std::io::Error) -> Self {
        Self::Key(format!("{}: {source}", path.display()))
    }

    /// Stable classification for an alarm or an audit record. Carries
    /// no path, no message and no caller bytes.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Key(_) => "journal_key",
            Self::Io { .. } => "journal_io",
            Self::Integrity(_) => "journal_integrity",
            Self::Encode(_) => "journal_encode",
            Self::Quota(_) => "journal_quota",
            Self::Writer(_) => "journal_writer",
            Self::Forbidden { .. } => "journal_forbidden",
            Self::Truncated { .. } => "journal_truncated",
            Self::StaleWriter { .. } => "journal_stale_writer",
            Self::HeadUncommitted { .. } => "journal_head_uncommitted",
            Self::AnchorMissing { .. } => "journal_anchor_missing",
            Self::Quarantined(_) => "journal_quarantined",
            Self::NotUnresolved { .. } => "journal_not_unresolved",
        }
    }
}

// ---------------------------------------------------------------------------
// Durable primitives
// ---------------------------------------------------------------------------

/// Write bytes so that a crash leaves either the old file or the new
/// one: tmp with `0600`, fsync, rename, fsync the directory.
pub(crate) fn write_durable(path: &Path, body: &[u8]) -> Result<(), JournalError> {
    use std::io::Write;

    // Test-only, and gated at the call site so a release build has no
    // hook here at all — not even one that returns `Ok`.
    #[cfg(test)]
    faults::fail_if_armed(faults::Fault::AnchorCommit)
        .map_err(|detail| JournalError::io(path, std::io::Error::other(detail)))?;

    let Some(parent) = path.parent() else {
        return Err(JournalError::Integrity(
            "journal path has no parent directory".to_string(),
        ));
    };
    crate::storage::ensure_private_dir(parent).map_err(|error| JournalError::io(parent, error))?;

    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = parent.join(tmp_name);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        let mut file = options
            .open(&tmp)
            .map_err(|error| JournalError::io(&tmp, error))?;
        file.write_all(body)
            .map_err(|error| JournalError::io(&tmp, error))?;
        file.sync_all()
            .map_err(|error| JournalError::io(&tmp, error))?;
    }
    std::fs::rename(&tmp, path).map_err(|error| JournalError::io(path, error))?;
    sync_dir(parent)
}

#[cfg(unix)]
pub(crate) fn sync_dir(dir: &Path) -> Result<(), JournalError> {
    std::fs::File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|error| JournalError::io(dir, error))
}

#[cfg(not(unix))]
pub(crate) fn sync_dir(_dir: &Path) -> Result<(), JournalError> {
    Ok(())
}

/// Deliberate failure injection for the durability paths.
///
/// **Absent from any non-test build.** The module, the `Fault` enum, the
/// armed state and the failure branch are all `#[cfg(test)]`, and so are
/// the two call sites, so a release binary contains no hook to call, no
/// state to arm and no branch to take. There is deliberately no
/// production no-op shim: a function that exists only to return `Ok`
/// still gives an attacker a symbol to look for and a maintainer a place
/// to add an input channel later.
///
/// Nothing here reads the environment or any configuration. The switch
/// is a private atomic that only this crate's tests can reach, because
/// the load-bearing properties — "a failed start never dispatches", "a
/// failed completion is indeterminate" — are exactly the paths a real
/// disk failure takes, and there is no portable way to make `write(2)`
/// or `rename(2)` fail on demand.
#[cfg(test)]
pub(crate) mod faults {
    use std::sync::atomic::{AtomicU8, Ordering};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Fault {
        AppendWrite,
        AnchorCommit,
    }

    impl Fault {
        fn code(self) -> u8 {
            match self {
                Self::AppendWrite => 1,
                Self::AnchorCommit => 2,
            }
        }
    }

    static ARMED: AtomicU8 = AtomicU8::new(0);

    pub fn arm(fault: Fault) {
        ARMED.store(fault.code(), Ordering::SeqCst);
    }

    pub fn disarm() {
        ARMED.store(0, Ordering::SeqCst);
    }

    /// Fail if this fault is currently armed. The only caller of this is
    /// a `#[cfg(test)]` statement in the durability path.
    pub fn fail_if_armed(fault: Fault) -> Result<(), String> {
        if ARMED.load(Ordering::SeqCst) == fault.code() {
            Err(format!("injected journal fault: {fault:?}"))
        } else {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Facade
// ---------------------------------------------------------------------------

/// The process's writer lease, acquiring it on first use.
pub fn lease() -> Result<Arc<WriterLease>, JournalError> {
    writer::lease_for(&root())
}

/// Append one event.
///
/// This is the only entry point outside the writer itself. Callers name
/// their [`EventSource`]; the ACL, the quota class and the chain are
/// enforced below them.
pub fn record(
    partition: &Partition,
    owner_uid: u32,
    source: EventSource,
    event: JournalEvent,
) -> Result<Appended, JournalError> {
    record_classified(partition, owner_uid, source, event, false)
}

/// Append one event, charging it to the context-ingest budget.
pub fn record_context_ingest(
    partition: &Partition,
    owner_uid: u32,
    source: EventSource,
    event: JournalEvent,
) -> Result<Appended, JournalError> {
    record_classified(partition, owner_uid, source, event, true)
}

fn record_classified(
    partition: &Partition,
    owner_uid: u32,
    source: EventSource,
    event: JournalEvent,
    context_ingest: bool,
) -> Result<Appended, JournalError> {
    if recovery::is_quarantined(partition) {
        return Err(JournalError::Quarantined(partition.key()));
    }
    let lease = lease()?;
    // Maintenance may not stand between a mutation and its closing
    // record: a closure append proceeds even if rotation or a retention
    // record could not be written, because "the journal is full" must
    // never become "the outcome is unknown".
    let closure = event.is_closure();
    match maintain(&lease, partition, owner_uid) {
        Ok(()) => {}
        Err(error) if closure => {
            alarm::raise(
                alarm::Class::AppendFailed,
                &partition.key(),
                &format!("journal maintenance was skipped before a closure record: {error}"),
            );
        }
        Err(error) => return Err(error),
    }
    lease.append(Append {
        partition,
        owner_uid,
        source,
        event,
        context_ingest,
    })
}

/// Append an event whose loss must not fail the caller — the read-only
/// and health paths.
///
/// The failure is still not silent: it raises a bounded alarm on the
/// independent channel, which is what makes "journalling is down" a
/// visible condition rather than a quiet one.
pub fn record_best_effort(
    partition: &Partition,
    owner_uid: u32,
    source: EventSource,
    event: JournalEvent,
) {
    let kind = event.kind();
    if let Err(error) = record(partition, owner_uid, source, event) {
        let class = match &error {
            JournalError::Forbidden { .. } => alarm::Class::AclViolation,
            JournalError::Quota(_) => alarm::Class::QuotaExhausted,
            JournalError::Integrity(_)
            | JournalError::Truncated { .. }
            | JournalError::StaleWriter { .. }
            | JournalError::AnchorMissing { .. }
            | JournalError::Quarantined(_) => alarm::Class::IntegrityFailed,
            _ => alarm::Class::AppendFailed,
        };
        alarm::raise(
            class,
            &partition.key(),
            &format!("{kind} was not recorded: {error}"),
        );
    }
}
// ---------------------------------------------------------------------------
// Mutation bracketing
// ---------------------------------------------------------------------------

/// What a caller must name to open a bracket.
pub struct MutationStart<'a> {
    pub partition: Partition,
    pub owner_uid: u32,
    /// The registry's own route name. Never a caller string.
    pub route: &'static str,
    /// The caller's stable operation key. See [`operation_identity`] for
    /// what makes it durable and why PID and start time are not part of
    /// it.
    pub request_key: &'a str,
    /// The keyed reference of the grant the route runs under. Never a
    /// handle, and never the grant itself.
    pub grant: Option<&'a str>,
    pub session_mutation: Option<u64>,
    /// Charge to the attacker-influenced ingest budget.
    pub context_ingest: bool,
}

/// An open bracket. Dropping one without resolving it is a crash, and
/// the next recovery pass reports it as an orphan that stays unresolved.
#[derive(Debug)]
#[must_use = "an open mutation bracket must be committed or failed"]
pub struct MutationBracket {
    partition: Partition,
    owner_uid: u32,
    operation: OperationId,
    route: &'static str,
    idempotency: String,
    context_ingest: bool,
    started: Instant,
    start_seq: u64,
}

/// The outcome of closing a bracket after the effect already ran.
#[derive(Debug, thiserror::Error)]
#[error("{detail}")]
pub struct UnresolvedMutation {
    pub partition: String,
    pub operation: String,
    pub detail: String,
}

impl MutationBracket {
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    /// Sequence the start landed at, for the reference other sinks carry
    /// instead of a second copy of the record.
    pub fn start_seq(&self) -> u64 {
        self.start_seq
    }

    pub fn partition(&self) -> &Partition {
        &self.partition
    }

    /// The effect completed and its durable result is known.
    pub fn commit(self) -> Result<Appended, UnresolvedMutation> {
        let duration_ms = self.started.elapsed().as_millis() as u64;
        let operation = self.operation.clone();
        self.close(JournalEvent::MutationCommitted {
            operation,
            duration_ms,
        })
    }

    /// The effect definitely did not happen, or definitely failed.
    pub fn fail(self, class: &str, message: &str) -> Result<Appended, UnresolvedMutation> {
        let duration_ms = self.started.elapsed().as_millis() as u64;
        let operation = self.operation.clone();
        self.close(JournalEvent::MutationFailed {
            operation,
            duration_ms,
            class: Label::new(class),
            error: crate::audit_policy::text_digest(message),
        })
    }

    fn close(self, event: JournalEvent) -> Result<Appended, UnresolvedMutation> {
        match record_classified(
            &self.partition,
            self.owner_uid,
            EventSource::Kernel,
            event,
            self.context_ingest,
        ) {
            Ok(appended) => {
                forget_unresolved(&self.partition, self.route, &self.idempotency);
                Ok(appended)
            }
            Err(error) => {
                // The effect already ran. Leave an explicit "unknown"
                // behind, keep refusing replays of this identity, and
                // tell the caller — which must not answer with an
                // ordinary success.
                remember_unresolved(&self.partition, self.route, &self.idempotency);
                let marker = record_classified(
                    &self.partition,
                    self.owner_uid,
                    EventSource::Kernel,
                    JournalEvent::MutationIndeterminate {
                        operation: self.operation.clone(),
                        reason: Indeterminate::CompletionUnrecorded,
                    },
                    self.context_ingest,
                );
                let detail = match marker {
                    Ok(_) => format!(
                        "the completion of operation {} was not recorded ({error}); the journal \
                         marked it indeterminate and recovery is required",
                        self.operation
                    ),
                    Err(second) => format!(
                        "the completion of operation {} was not recorded ({error}) and the \
                         indeterminate marker also failed ({second}); recovery is required",
                        self.operation
                    ),
                };
                alarm::raise(
                    alarm::Class::MutationIndeterminate,
                    &self.partition.key(),
                    &detail,
                );
                Err(UnresolvedMutation {
                    partition: self.partition.key(),
                    operation: self.operation.as_str().to_string(),
                    detail,
                })
            }
        }
    }
}

/// Open a bracket before a durable mutation runs.
///
/// A failure here means nothing was dispatched: the caller must refuse
/// the request rather than proceed unrecorded.
pub fn begin_mutation(start: MutationStart<'_>) -> Result<MutationBracket, JournalError> {
    let operation = OperationId::generate();
    let identity = operation_identity(start.owner_uid, start.route, start.request_key)?;
    let appended = record_classified(
        &start.partition,
        start.owner_uid,
        EventSource::Kernel,
        JournalEvent::MutationStarted {
            operation: operation.clone(),
            route: Label::new(start.route),
            idempotency: identity.clone(),
            grant: start.grant.map(Reference::new),
            session_mutation: start.session_mutation,
        },
        start.context_ingest,
    )?;
    Ok(MutationBracket {
        partition: start.partition,
        owner_uid: start.owner_uid,
        operation,
        route: start.route,
        idempotency: identity.digest,
        context_ingest: start.context_ingest,
        started: Instant::now(),
        start_seq: appended.seq,
    })
}
// ---------------------------------------------------------------------------
// Durable operation identity
// ---------------------------------------------------------------------------

/// The identity a replay is recognised by, keyed so it reveals nothing.
///
/// It is `owner uid + canonical route + the caller's operation key`, and
/// deliberately **not** the transport's duplicate-detection key: that one
/// mixes in pid and process start time, so the same operation retried by
/// a restarted client would hash differently and be accepted as new —
/// exactly the case where the first attempt's effect is unknown.
///
/// The digest is keyed under the journal's own root-only key rather than
/// [`crate::audit_policy`]'s per-process one, so it is stable across
/// daemon restarts — which is when "did this already run?" matters —
/// while a caller still cannot test a guess against it.
///
/// A caller that wants cross-restart replay protection must reuse its
/// operation key on retry. One that cannot is not guessed about: its
/// retry is a new operation, and the unresolved bracket left behind
/// keeps refusing until an operator resolves it.
pub fn operation_identity(
    owner_uid: u32,
    route: &str,
    request_key: &str,
) -> Result<crate::audit_policy::TextDigest, JournalError> {
    let lease = lease()?;
    let mut data = Vec::with_capacity(request_key.len() + 64);
    data.extend_from_slice(b"cos.session.journal.idempotency.v1");
    push_field(&mut data, &owner_uid.to_be_bytes());
    push_field(&mut data, route.as_bytes());
    push_field(&mut data, request_key.as_bytes());
    let mut digest = crate::crypto::hmac_sha256_hex(lease.keyring().active_key(), &data);
    digest.truncate(16);
    Ok(crate::audit_policy::TextDigest {
        bytes: request_key.len(),
        digest,
    })
}

fn push_field(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}

// ---------------------------------------------------------------------------
// Unresolved brackets
// ---------------------------------------------------------------------------

use std::collections::HashSet;
use std::sync::Mutex;

/// (partition key, route, idempotency digest) of every bracket this
/// machine has started and not resolved.
static UNRESOLVED: Mutex<Option<HashSet<(String, String, String)>>> = Mutex::new(None);

fn remember_unresolved(partition: &Partition, route: &str, digest: &str) {
    if let Ok(mut guard) = UNRESOLVED.lock() {
        guard.get_or_insert_with(HashSet::new).insert((
            partition.key(),
            route.to_string(),
            digest.to_string(),
        ));
    }
}

fn forget_unresolved(partition: &Partition, route: &str, digest: &str) {
    if let Ok(mut guard) = UNRESOLVED.lock() {
        if let Some(set) = guard.as_mut() {
            set.remove(&(partition.key(), route.to_string(), digest.to_string()));
        }
    }
}

/// Publish what a recovery pass found, so replays are refused for the
/// rest of this daemon's life as well.
pub fn register_unresolved(report: &recovery::Report) {
    let Ok(mut guard) = UNRESOLVED.lock() else {
        return;
    };
    let set = guard.get_or_insert_with(HashSet::new);
    for entry in &report.orphans {
        set.insert((
            entry.partition.clone(),
            entry.route.clone(),
            entry.idempotency.clone(),
        ));
    }
}

/// Whether this request replays a mutation whose outcome is still
/// unknown.
///
/// The caller must refuse rather than re-run: the effect may already
/// have landed, and the broker's in-memory duplicate detector cannot see
/// across a restart. The refusal lasts until an operator records a
/// [`Resolution`]; an orphan record does not lift it.
pub fn replays_unresolved(partition: &Partition, route: &str, request_key: &str) -> bool {
    let Ok(identity) = operation_identity(owner_of(partition), route, request_key) else {
        // Failing to derive the identity means the keyring is
        // unavailable, and a mutation will fail closed at its start
        // anyway. Do not claim the request is new.
        return true;
    };
    UNRESOLVED
        .lock()
        .ok()
        .and_then(|guard| {
            guard.as_ref().map(|set| {
                set.contains(&(partition.key(), route.to_string(), identity.digest.clone()))
            })
        })
        .unwrap_or(false)
}

fn owner_of(partition: &Partition) -> u32 {
    match partition {
        Partition::Owner(uid) => *uid,
        Partition::Session(_) => 0,
    }
}

/// Forget unresolved brackets. Used between tests and daemon lifetimes.
#[cfg(test)]
pub fn clear_unresolved() {
    if let Ok(mut guard) = UNRESOLVED.lock() {
        *guard = None;
    }
}

/// Record an operator's conclusion about an unresolved mutation.
///
/// This is the only thing that retires a bracket the machine could not
/// resolve. It re-runs nothing, rolls back nothing and grants nothing:
/// it records what a human verified, so the replay refusal can end.
/// `decided_by` is the root principal the broker authenticated, never a
/// name the caller chose.
pub fn resolve_mutation(
    partition: &Partition,
    owner_uid: u32,
    operation: &str,
    outcome: Resolution,
    decided_by: u32,
) -> Result<Appended, JournalError> {
    let lease = lease()?;
    let chain = reader::read(lease.root(), partition, owner_uid, lease.keyring())?;
    if chain.health.is_damaged() {
        return Err(JournalError::Quarantined(partition.key()));
    }
    let unresolved = recovery::unresolved_in(partition, &chain.records);
    let Some(entry) = unresolved
        .iter()
        .find(|entry| entry.operation == operation)
        .cloned()
    else {
        return Err(JournalError::NotUnresolved {
            partition: partition.key(),
            operation: crate::audit_policy::safe_identity(operation),
        });
    };
    let Some(operation_id) = chain.records.iter().find_map(|record| {
        record
            .event
            .operation()
            .filter(|candidate| candidate.as_str() == operation)
            .cloned()
    }) else {
        return Err(JournalError::NotUnresolved {
            partition: partition.key(),
            operation: crate::audit_policy::safe_identity(operation),
        });
    };

    let appended = record(
        partition,
        owner_uid,
        EventSource::Kernel,
        JournalEvent::MutationResolved {
            operation: operation_id,
            outcome,
            decided_by: Reference::new(&format!("uid:{decided_by}")),
        },
    )?;
    forget_unresolved(partition, &entry.route, &entry.idempotency);
    Ok(appended)
}

/// Every bracket this machine has started and not resolved, for the
/// operator surface.
pub fn unresolved_mutations(
    partition: &Partition,
    owner_uid: u32,
) -> Result<Vec<recovery::Unresolved>, JournalError> {
    let lease = lease()?;
    let chain = reader::read(lease.root(), partition, owner_uid, lease.keyring())?;
    Ok(recovery::unresolved_in(partition, &chain.records))
}
// ---------------------------------------------------------------------------
// Rotation and retention
// ---------------------------------------------------------------------------

/// Keep the partition's storage shape healthy before an append.
///
/// Two steps, each of which is safe to be interrupted at any point:
///
/// 1. If a previous rotation still owes its retention record, write it.
///    The pending marker and the record clear in the same anchor commit,
///    so this is exactly-once rather than at-least-once.
/// 2. If the active segment has reached its rotation size, cut it. That
///    is a single anchor commit naming the next segment index; no file
///    is moved, so there is no state in which the anchor and the
///    segments disagree.
fn maintain(
    lease: &WriterLease,
    partition: &Partition,
    owner_uid: u32,
) -> Result<(), JournalError> {
    if let Some(pending) = lease.load_anchor(partition, owner_uid)?.pending_retention {
        write_retention(lease, partition, owner_uid, pending)?;
    }
    let anchor = lease.load_anchor(partition, owner_uid)?;
    if anchor.active_bytes < quota::ROTATE_BYTES {
        return Ok(());
    }
    rotate(lease, partition, owner_uid, RetentionReason::SizeRotation)
}

/// Cut the active segment now and record what it archived.
pub fn rotate(
    lease: &WriterLease,
    partition: &Partition,
    owner_uid: u32,
    reason: RetentionReason,
) -> Result<(), JournalError> {
    let Some(pending) = lease.rotate_active(partition, owner_uid)? else {
        return Ok(());
    };
    write_retention_with_reason(lease, partition, owner_uid, pending, reason)
}

fn write_retention(
    lease: &WriterLease,
    partition: &Partition,
    owner_uid: u32,
    pending: partition::PendingRetention,
) -> Result<(), JournalError> {
    write_retention_with_reason(
        lease,
        partition,
        owner_uid,
        pending,
        RetentionReason::SizeRotation,
    )
}

fn write_retention_with_reason(
    lease: &WriterLease,
    partition: &Partition,
    owner_uid: u32,
    pending: partition::PendingRetention,
    reason: RetentionReason,
) -> Result<(), JournalError> {
    lease.append(Append {
        partition,
        owner_uid,
        source: EventSource::Recovery,
        event: JournalEvent::RetentionApplied {
            reason,
            retained_from_seq: pending.retained_from_seq,
            archive: Some(pending.archive),
        },
        context_ingest: false,
    })?;
    Ok(())
}

/// Run the startup pass: verify every partition, flag unresolved
/// brackets, and report what an operator has to resolve.
pub fn startup_recovery(detected_by: RecoverySource) -> Result<recovery::Report, JournalError> {
    let lease = lease()?;
    let report = recovery::run(&lease, detected_by)?;
    register_unresolved(&report);
    Ok(report)
}

#[cfg(test)]
pub(crate) mod harness {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/harness.rs"
    ));
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/mod.rs"
    ));
}
