//! The single writer: lease, epoch, partition lock, durable append.
//!
//! ## One writer, and it proves it
//!
//! Appending needs a [`WriterLease`]. Acquiring one takes an exclusive
//! `flock` on `<journal>/writer.lock` that is held for the life of the
//! process, so a second daemon — or a stale worker that still has the
//! path — cannot obtain one. The lease also bumps a monotonic epoch
//! that is stamped into every record and into the head anchor, so even
//! if a lease were somehow re-created, records from the older lifetime
//! are recognisable and an older epoch can no longer commit.
//!
//! Nothing outside root `clawd` holds a descriptor on these files.
//! Workers, Apps and MCP servers reach the journal only by asking the
//! broker, which derives task, session and owner from the grant it
//! verified and passes [`EventSource::Worker`] so the ACL applies.
//!
//! ## What the head anchor buys
//!
//! `anchor.json` is the committed head: sequence, MAC, the active
//! segment and its byte length, per-class counters and the number of
//! open mutation brackets, signed under the same root-only keyring. It
//! is committed with `write → fsync → rename → fsync(dir)` *after* the
//! chain line is fsynced, which makes `active_bytes` the definition of
//! "committed":
//!
//! * bytes beyond it were never acknowledged to any caller, and are
//!   discarded on the next append (a torn write raises an alarm);
//! * bytes short of it mean a segment was truncated behind the daemon's
//!   back, which fails closed;
//! * a head MAC that does not match the record at that sequence means
//!   the chain was reordered or a record was swapped, which fails
//!   closed;
//! * an anchor that is *missing* while segments still hold bytes is
//!   damage, not a fresh partition. It is never adopted and never
//!   truncated — the bytes stay, mutations fail closed, and an operator
//!   decides.
//!
//! ## Rotation has no intermediate state
//!
//! Rotation moves no files: it commits one anchor naming a new active
//! segment index, the first sequence it will hold, and the MAC it chains
//! from. Before the commit the old segment is active; after it the new
//! index is active and its file does not exist, which is exactly what
//! "zero bytes committed" means. Every durably committed state is
//! reader-valid, so no crash boundary can look like tampering.
//!
//! ## Threat model, stated plainly
//!
//! This defeats a *local unprivileged* attacker, and any privileged
//! process that can reach the log but not the key. It does **not**
//! defeat root, or anyone with physical access, restoring a consistent
//! older snapshot of the key, the chain and the anchor together:
//! without a TPM or a remote anchor there is nothing outside this
//! machine to compare against, and this module deliberately does not
//! claim otherwise.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::acl::EventSource;
use super::event::{ContentRef, ContentStore, JournalEvent, MAX_EVENT_BYTES, SCHEMA_VERSION};
use super::keyring::{self, Keyring};
use super::partition::{Anchor, Partition, PendingRetention};
use super::quota::{self, QuotaClass};
use super::record::{JournalRecord, Preimage};
use super::{alarm, JournalError};

/// What one append asks for.
pub struct Append<'a> {
    pub partition: &'a Partition,
    pub owner_uid: u32,
    pub source: EventSource,
    pub event: JournalEvent,
    /// Charge this append to the attacker-influenced context-ingest
    /// budget rather than to broker control capacity.
    pub context_ingest: bool,
}

/// Where an accepted record landed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Appended {
    pub partition: String,
    pub seq: u64,
    pub epoch: u64,
    pub mac: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriterState {
    v: u32,
    epoch: u64,
    pid: u32,
    started_at_ms: u64,
    key_id: String,
    mac: String,
}

impl WriterState {
    fn preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(96);
        out.extend_from_slice(b"cos.session.journal.writer");
        out.extend_from_slice(&self.v.to_be_bytes());
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.extend_from_slice(&self.pid.to_be_bytes());
        out.extend_from_slice(&self.started_at_ms.to_be_bytes());
        out.extend_from_slice(&(self.key_id.len() as u64).to_be_bytes());
        out.extend_from_slice(self.key_id.as_bytes());
        out
    }
}

/// Proof that this process is the journal's writer.
///
/// Not `Clone`: the flock behind it has exactly one owner. Shared as an
/// `Arc` by the process-wide registry below so every subsystem appends
/// through the same lease and the same epoch.
pub struct WriterLease {
    root: PathBuf,
    epoch: u64,
    pid: u32,
    keyring: Keyring,
    _lock: File,
}

impl std::fmt::Debug for WriterLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterLease")
            .field("root", &self.root)
            .field("epoch", &self.epoch)
            .field("pid", &self.pid)
            .finish()
    }
}

impl WriterLease {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn keyring(&self) -> &Keyring {
        &self.keyring
    }

    /// Append one record, or explain why it was refused.
    ///
    /// The lock is held only for the append itself. Callers that
    /// bracket a side effect open the bracket, release, run the effect
    /// and append the close — the lock never spans the mutation.
    pub fn append(&self, request: Append<'_>) -> Result<Appended, JournalError> {
        if !request.source.may_write(&request.event) {
            return Err(JournalError::Forbidden {
                writer: request.source.as_str(),
                event: request.event.kind(),
            });
        }
        let body = serde_json::to_vec(&request.event)
            .map_err(|error| JournalError::Encode(format!("encode journal event: {error}")))?;
        if body.len() > MAX_EVENT_BYTES {
            return Err(JournalError::Quota(format!(
                "journal event {} is {} bytes; the ceiling is {MAX_EVENT_BYTES}",
                request.event.kind(),
                body.len()
            )));
        }

        let class = QuotaClass::of(&request.event, request.source, request.context_ingest);
        quota::admit_rate(request.partition, class)?;

        let dir = request.partition.dir(&self.root);
        if !dir.exists() {
            let existing = super::partition::list(&self.root)?.len();
            if existing >= quota::MAX_PARTITIONS {
                return Err(JournalError::Quota(format!(
                    "the journal already holds {existing} partitions"
                )));
            }
            crate::storage::ensure_private_dir(&dir)
                .map_err(|error| JournalError::io(&dir, error))?;
            sync_parent(&dir)?;
        }

        with_partition_lock(&self.root, request.partition, || {
            self.append_locked(&request, class)
        })
    }

    fn append_locked(
        &self,
        request: &Append<'_>,
        class: QuotaClass,
    ) -> Result<Appended, JournalError> {
        let mut anchor = self.checked_anchor(request.partition, request.owner_uid)?;

        let active_path = anchor.active_path(&self.root, request.partition);
        reconcile_tail(&active_path, &anchor)?;

        let recorded_at_ms = now_ms();
        let seq = anchor.seq.saturating_add(1);
        let preimage = Preimage {
            schema: SCHEMA_VERSION,
            partition: &anchor.partition,
            owner_uid: request.owner_uid,
            seq,
            epoch: self.epoch,
            recorded_at_ms,
            source: request.source,
            event: &request.event,
            prev: &anchor.head_mac,
            key_id: self.keyring.active_id(),
        };
        let mac = preimage.seal(self.keyring.active_key())?;
        let record = JournalRecord {
            v: SCHEMA_VERSION,
            seq,
            epoch: self.epoch,
            recorded_at_ms,
            partition: anchor.partition.clone(),
            owner_uid: request.owner_uid,
            source: request.source,
            key_id: self.keyring.active_id().to_string(),
            prev: anchor.head_mac.clone(),
            mac: mac.clone(),
            event: request.event.clone(),
        };
        let line = record.encode_line()?;
        let written = line.len() as u64 + 1;
        quota::check(&anchor, class, request.event.opens_mutation(), written)?;

        append_line(&active_path, &line)?;

        if anchor.active_bytes == 0 {
            anchor.active_first_seq = seq;
            anchor.active_prev_mac = anchor.head_mac.clone();
        }
        if anchor.first_seq == 0 {
            anchor.first_seq = seq;
            anchor.first_prev_mac = anchor.head_mac.clone();
        }
        anchor.epoch = self.epoch;
        anchor.seq = seq;
        anchor.head_mac = mac.clone();
        anchor.active_bytes = anchor.active_bytes.saturating_add(written);
        anchor.total_bytes = anchor.total_bytes.saturating_add(written);
        anchor.events = anchor.events.saturating_add(1);
        match class {
            QuotaClass::Closure => anchor.closure_events = anchor.closure_events.saturating_add(1),
            QuotaClass::Control => anchor.control_events = anchor.control_events.saturating_add(1),
            QuotaClass::Worker => anchor.worker_events = anchor.worker_events.saturating_add(1),
            QuotaClass::ContextIngest => {
                anchor.ingest_events = anchor.ingest_events.saturating_add(1)
            }
        }
        if request.event.opens_mutation() {
            anchor.open_brackets = anchor.open_brackets.saturating_add(1);
        } else if request.event.resolves_mutation() {
            anchor.open_brackets = anchor.open_brackets.saturating_sub(1);
        }
        // A retention record and the pending marker it satisfies clear
        // in the same atomic commit, so the record is exactly-once
        // across a crash rather than at-least-once.
        if let (Some(pending), JournalEvent::RetentionApplied { .. }) =
            (anchor.pending_retention.as_ref(), &request.event)
        {
            if pending.retained_from_seq == anchor.active_first_seq {
                anchor.pending_retention = None;
            }
        }
        anchor.key_id = self.keyring.active_id().to_string();
        anchor.updated_at_ms = recorded_at_ms;
        anchor.seal(self.keyring.active_key());
        self.commit_anchor(request.partition, &anchor)?;

        Ok(Appended {
            partition: anchor.partition,
            seq,
            epoch: self.epoch,
            mac,
        })
    }

    /// The committed head for a partition, or an empty anchor.
    pub fn load_anchor(
        &self,
        partition: &Partition,
        owner_uid: u32,
    ) -> Result<Anchor, JournalError> {
        load_anchor(&self.root, partition, owner_uid, &self.keyring)
    }

    /// Load the head and refuse it if this writer may not extend it.
    fn checked_anchor(
        &self,
        partition: &Partition,
        owner_uid: u32,
    ) -> Result<Anchor, JournalError> {
        let anchor = self.load_anchor(partition, owner_uid)?;
        if anchor.epoch > self.epoch {
            return Err(JournalError::StaleWriter {
                partition: anchor.partition.clone(),
                committed_epoch: anchor.epoch,
                writer_epoch: self.epoch,
            });
        }
        if anchor.owner_uid != owner_uid {
            return Err(JournalError::Integrity(format!(
                "journal partition {} belongs to uid {}, not {owner_uid}",
                anchor.partition, anchor.owner_uid
            )));
        }
        Ok(anchor)
    }

    /// Close the active segment and start the next one.
    ///
    /// One durable anchor commit and no file operations, so there is no
    /// state in which the anchor and the segments disagree. Returns the
    /// retention the caller should record, or `None` when there was
    /// nothing to cut.
    pub fn rotate_active(
        &self,
        partition: &Partition,
        owner_uid: u32,
    ) -> Result<Option<PendingRetention>, JournalError> {
        with_partition_lock(&self.root, partition, || {
            let mut anchor = self.checked_anchor(partition, owner_uid)?;
            if let Some(pending) = anchor.pending_retention.clone() {
                // A previous rotation still owes its retention record;
                // hand that back rather than cutting again.
                return Ok(Some(pending));
            }
            if anchor.active_bytes == 0 || anchor.seq == 0 {
                return Ok(None);
            }

            let active_path = anchor.active_path(&self.root, partition);
            reconcile_tail(&active_path, &anchor)?;
            let bytes = std::fs::read(&active_path)
                .map_err(|error| JournalError::io(&active_path, error))?;
            let archive = ContentRef::of(ContentStore::SessionTurns, &bytes);

            let pending = PendingRetention {
                segment_index: anchor.active_index,
                retained_from_seq: anchor.seq.saturating_add(1),
                archive,
            };
            anchor.active_index = anchor.active_index.saturating_add(1);
            anchor.active_first_seq = anchor.seq.saturating_add(1);
            anchor.active_prev_mac = anchor.head_mac.clone();
            anchor.active_bytes = 0;
            anchor.pending_retention = Some(pending.clone());
            anchor.epoch = self.epoch;
            anchor.updated_at_ms = now_ms();
            anchor.seal(self.keyring.active_key());
            self.commit_anchor(partition, &anchor)?;
            Ok(Some(pending))
        })
    }

    fn commit_anchor(&self, partition: &Partition, anchor: &Anchor) -> Result<(), JournalError> {
        let path = partition.anchor_path(&self.root);
        let body = serde_json::to_vec(anchor)
            .map_err(|error| JournalError::Encode(format!("encode journal anchor: {error}")))?;
        super::write_durable(&path, &body).map_err(|error| JournalError::HeadUncommitted {
            partition: anchor.partition.clone(),
            seq: anchor.seq,
            detail: error.to_string(),
        })
    }
}

/// Read a partition's committed head without holding a lease.
///
/// Readers — the CLI, projections, diagnostics — use this. It verifies
/// the anchor MAC, so a rewritten head is reported rather than believed,
/// and it refuses to invent an empty head for a partition that still
/// holds chain bytes.
pub fn load_anchor(
    root: &Path,
    partition: &Partition,
    owner_uid: u32,
    keyring: &Keyring,
) -> Result<Anchor, JournalError> {
    let path = partition.anchor_path(root);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // "Never written" and "the head was deleted" look the same
            // on disk except for this. Adopting the second as the first
            // would let anyone who can unlink one small file convince
            // the daemon that a committed chain was uncommitted — and
            // the tail reconciler would then erase it.
            if partition.has_chain_bytes(root)? {
                return Err(JournalError::AnchorMissing {
                    partition: partition.key(),
                });
            }
            return Ok(Anchor::empty(partition, owner_uid, keyring.active_id()));
        }
        Err(error) => return Err(JournalError::io(&path, error)),
    };
    let anchor: Anchor = serde_json::from_slice(&data).map_err(|error| {
        JournalError::Integrity(format!(
            "journal anchor {} is unusable: {error}",
            path.display()
        ))
    })?;
    if anchor.v != SCHEMA_VERSION {
        return Err(JournalError::Integrity(format!(
            "journal anchor {} declares schema {}; this daemon knows {SCHEMA_VERSION}",
            path.display(),
            anchor.v
        )));
    }
    if anchor.partition != partition.key() {
        return Err(JournalError::Integrity(format!(
            "journal anchor {} names partition {}",
            path.display(),
            anchor.partition
        )));
    }
    let key = keyring.verify_key(&anchor.key_id).ok_or_else(|| {
        JournalError::Integrity(format!(
            "journal anchor {} is signed with key {}, which this daemon does not hold",
            path.display(),
            anchor.key_id
        ))
    })?;
    anchor.verify(key)?;
    Ok(anchor)
}

/// Bring the active segment back to exactly what the head committed.
///
/// Shorter than the committed length is truncation and fails closed.
/// Longer is a torn append that was never acknowledged to any caller —
/// the writer refused to dispatch, so no side effect can depend on it —
/// and it is discarded with an alarm.
fn reconcile_tail(path: &Path, anchor: &Anchor) -> Result<(), JournalError> {
    let length = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(JournalError::io(path, error)),
    };
    if length == anchor.active_bytes {
        return Ok(());
    }
    if length < anchor.active_bytes {
        return Err(JournalError::Truncated {
            partition: anchor.partition.clone(),
            committed_bytes: anchor.active_bytes,
            found_bytes: length,
        });
    }
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| JournalError::io(path, error))?;
    file.set_len(anchor.active_bytes)
        .map_err(|error| JournalError::io(path, error))?;
    file.sync_all()
        .map_err(|error| JournalError::io(path, error))?;
    alarm::raise(
        alarm::Class::TornAppend,
        &anchor.partition,
        &format!(
            "discarded {} uncommitted byte(s) past the committed head at seq {}",
            length - anchor.active_bytes,
            anchor.seq
        ),
    );
    Ok(())
}

fn append_line(path: &Path, line: &str) -> Result<(), JournalError> {
    // Test-only, and gated at the call site so a release build has no
    // hook here at all — not even one that returns `Ok`.
    #[cfg(test)]
    super::faults::fail_if_armed(super::faults::Fault::AppendWrite)
        .map_err(|detail| JournalError::io(path, std::io::Error::other(detail)))?;
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            crate::storage::ensure_private_dir(parent)
                .map_err(|error| JournalError::io(parent, error))?;
            sync_parent(parent)?;
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| JournalError::io(path, error))?;
    let created_at = file
        .seek(SeekFrom::End(0))
        .map_err(|error| JournalError::io(path, error))?;
    file.write_all(line.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .map_err(|error| JournalError::io(path, error))?;
    file.sync_all()
        .map_err(|error| JournalError::io(path, error))?;
    if created_at == 0 {
        sync_parent(path)?;
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), JournalError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    super::sync_dir(parent)
}

/// Serialize every writer touching one partition.
///
/// The lock sentinel is a file of its own that is never renamed, so a
/// rotation of the chain cannot invalidate a held lock.
fn with_partition_lock<T>(
    root: &Path,
    partition: &Partition,
    operation: impl FnOnce() -> Result<T, JournalError>,
) -> Result<T, JournalError> {
    let path = partition.lock_path(root);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|error| JournalError::io(&path, error))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(JournalError::io(&path, std::io::Error::last_os_error()));
        }
    }

    let result = operation();

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
    drop(file);
    result
}

// ---------------------------------------------------------------------------
// Lease acquisition
// ---------------------------------------------------------------------------

type LeaseRegistry = HashMap<PathBuf, Arc<WriterLease>>;

static LEASES: Mutex<Option<LeaseRegistry>> = Mutex::new(None);

/// The writer lease for `root`, acquiring it on first use.
///
/// Keyed by journal root so a test that rebinds `COS_DATA_DIR` gets its
/// own lease and epoch instead of inheriting another directory's.
pub fn lease_for(root: &Path) -> Result<Arc<WriterLease>, JournalError> {
    let mut guard = LEASES
        .lock()
        .map_err(|_| JournalError::Writer("journal lease registry is poisoned".to_string()))?;
    let registry = guard.get_or_insert_with(HashMap::new);
    if let Some(lease) = registry.get(root) {
        return Ok(Arc::clone(lease));
    }
    let lease = Arc::new(acquire(root)?);
    registry.insert(root.to_path_buf(), Arc::clone(&lease));
    Ok(lease)
}

/// Release every cached lease. Used when a test rebinds the data
/// directory, and by the integration harness between daemon lifetimes.
#[cfg(test)]
pub fn release_all() {
    if let Ok(mut guard) = LEASES.lock() {
        *guard = None;
    }
    quota::reset_rate_limits();
}

fn acquire(root: &Path) -> Result<WriterLease, JournalError> {
    crate::storage::ensure_private_dir(root).map_err(|error| JournalError::io(root, error))?;
    let keyring = keyring::load_or_create(root)?;

    let lock_path = root.join("writer.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(&lock_path)
        .map_err(|error| JournalError::io(&lock_path, error))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Err(JournalError::Writer(
                    "another process already holds the session journal writer lease".to_string(),
                ));
            }
            return Err(JournalError::io(&lock_path, error));
        }
    }
    #[cfg(not(unix))]
    {
        return Err(JournalError::Writer(
            "the session journal requires flock(2) for single-writer semantics".to_string(),
        ));
    }

    #[cfg(unix)]
    {
        let state_path = root.join("writer.json");
        let previous = read_writer_state(&state_path, &keyring)?;
        let epoch = previous
            .map(|state| state.epoch)
            .unwrap_or(0)
            .saturating_add(1);
        let mut state = WriterState {
            v: SCHEMA_VERSION,
            epoch,
            pid: std::process::id(),
            started_at_ms: now_ms(),
            key_id: keyring.active_id().to_string(),
            mac: String::new(),
        };
        state.mac = crate::crypto::hmac_sha256_hex(keyring.active_key(), &state.preimage());
        let body = serde_json::to_vec(&state)
            .map_err(|error| JournalError::Encode(format!("encode journal writer: {error}")))?;
        super::write_durable(&state_path, &body)?;

        Ok(WriterLease {
            root: root.to_path_buf(),
            epoch,
            pid: state.pid,
            keyring,
            _lock: lock,
        })
    }
}

#[cfg(unix)]
fn read_writer_state(path: &Path, keyring: &Keyring) -> Result<Option<WriterState>, JournalError> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(JournalError::io(path, error)),
    };
    let state: WriterState = serde_json::from_slice(&data).map_err(|error| {
        JournalError::Integrity(format!(
            "journal writer state {} is unusable: {error}",
            path.display()
        ))
    })?;
    let key = keyring.verify_key(&state.key_id).ok_or_else(|| {
        JournalError::Integrity(format!(
            "journal writer state {} is signed with an unknown key",
            path.display()
        ))
    })?;
    if crate::crypto::hmac_sha256_hex(key, &state.preimage()) != state.mac {
        return Err(JournalError::Integrity(format!(
            "journal writer state {} does not match its MAC",
            path.display()
        )));
    }
    Ok(Some(state))
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/writer.rs"
    ));
}
