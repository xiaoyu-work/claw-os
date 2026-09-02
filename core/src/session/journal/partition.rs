//! Partitions, segments, and the committed head anchor.
//!
//! A partition is one chain. Sessions get their own so a session's
//! evidence is contiguous and can be archived with the session;
//! privileged work with no session — a package install a user drove
//! from the CLI — lands in that owner's partition instead of a shared
//! file, so one account's volume cannot delay another's mutation.
//!
//! The on-disk name is derived, never taken from a caller: a session
//! partition is named by an already-validated [`SessionId`] and an owner
//! partition by an integer uid, so nothing here can traverse out of the
//! journal root.
//!
//! ## Segments make rotation a single atomic step
//!
//! A chain is a sequence of numbered segment files. Only the highest —
//! the *active* segment — is ever appended to; the rest are immutable.
//! Rotation is therefore not a file operation at all: it is one durable
//! anchor commit that names a new active index, the first sequence that
//! index will hold, and the MAC it chains from.
//!
//! That matters because every intermediate state has to be readable. A
//! design that renames the live file has a window where the anchor and
//! the file disagree, and *both* orderings of "commit then rename" and
//! "rename then commit" leave a crash state that a reader must call
//! either truncation or tampering. With segments there is no window:
//! before the commit the old active segment is current, after it the new
//! index is current and its file does not exist yet, which is exactly
//! what "zero bytes committed" means.

use std::path::{Path, PathBuf};

use super::event::{ContentRef, SCHEMA_VERSION};
use super::record::GENESIS_MAC;
use super::JournalError;
use crate::session::SessionId;

/// Which chain an event belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Partition {
    /// A durable session's own chain.
    Session(SessionId),
    /// Privileged work an owner drove without a durable session.
    Owner(u32),
}

impl Partition {
    /// Stable key recorded in every record and bound into its MAC.
    pub fn key(&self) -> String {
        match self {
            Self::Session(sid) => format!("session/{}", sid.as_str()),
            Self::Owner(uid) => format!("owner/{uid}"),
        }
    }

    /// Parse a key back into a partition, refusing anything that was
    /// not produced by [`Partition::key`].
    pub fn parse(key: &str) -> Option<Self> {
        if let Some(sid) = key.strip_prefix("session/") {
            return sid.parse::<SessionId>().ok().map(Self::Session);
        }
        key.strip_prefix("owner/")
            .and_then(|uid| uid.parse::<u32>().ok())
            .map(Self::Owner)
    }

    /// Directory holding this partition's chain, under `root`.
    pub fn dir(&self, root: &Path) -> PathBuf {
        match self {
            Self::Session(sid) => root.join("sessions").join(sid.as_str()),
            Self::Owner(uid) => root.join("owners").join(uid.to_string()),
        }
    }

    pub fn segments_dir(&self, root: &Path) -> PathBuf {
        self.dir(root).join("segments")
    }

    /// Path of one numbered segment. Zero-padded so a directory listing
    /// sorts in chain order.
    pub fn segment_path(&self, root: &Path, index: u64) -> PathBuf {
        self.segments_dir(root).join(format!("{index:020}.jsonl"))
    }

    pub fn anchor_path(&self, root: &Path) -> PathBuf {
        self.dir(root).join("anchor.json")
    }

    pub fn lock_path(&self, root: &Path) -> PathBuf {
        self.dir(root).join("partition.lock")
    }

    /// Every segment index present on disk, in chain order.
    pub fn segments(&self, root: &Path) -> Result<Vec<u64>, JournalError> {
        let dir = self.segments_dir(root);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(JournalError::io(&dir, error)),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| JournalError::io(&dir, error))?;
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            let Some(index) = name.strip_suffix(".jsonl") else {
                continue;
            };
            match index.parse::<u64>() {
                Ok(index) => out.push(index),
                Err(_) => {
                    return Err(JournalError::Integrity(format!(
                        "journal partition {self} holds an unusable segment name"
                    )))
                }
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// Whether this partition already holds committed chain bytes.
    ///
    /// Used to tell "never written" from "the head anchor is gone",
    /// which are the same on disk except for this.
    pub fn has_chain_bytes(&self, root: &Path) -> Result<bool, JournalError> {
        for index in self.segments(root)? {
            let path = self.segment_path(root, index);
            match std::fs::metadata(&path) {
                Ok(meta) if meta.len() > 0 => return Ok(true),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(JournalError::io(&path, error)),
            }
        }
        Ok(false)
    }

    /// Session this partition belongs to, when it has one.
    pub fn session(&self) -> Option<&SessionId> {
        match self {
            Self::Session(sid) => Some(sid),
            Self::Owner(_) => None,
        }
    }
}

impl std::fmt::Display for Partition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.key())
    }
}

/// Enumerate every partition that has a chain on disk.
pub fn list(root: &Path) -> Result<Vec<Partition>, JournalError> {
    let mut out = Vec::new();
    collect(&root.join("sessions"), &mut out, |name| {
        name.parse::<SessionId>().ok().map(Partition::Session)
    })?;
    collect(&root.join("owners"), &mut out, |name| {
        name.parse::<u32>().ok().map(Partition::Owner)
    })?;
    out.sort_by_key(|partition| partition.key());
    Ok(out)
}

fn collect(
    dir: &Path,
    out: &mut Vec<Partition>,
    parse: impl Fn(&str) -> Option<Partition>,
) -> Result<(), JournalError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(JournalError::io(dir, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| JournalError::io(dir, error))?;
        if !entry.path().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        if let Some(partition) = parse(&name) {
            out.push(partition);
        }
    }
    Ok(())
}

/// A segment that was closed by rotation and whose retention record has
/// not been written yet.
///
/// Carried in the anchor so the retention record is exactly-once across
/// a crash: the rotation commit sets it, and the commit that stores the
/// matching [`super::event::JournalEvent::RetentionApplied`] clears it in
/// the same atomic write.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRetention {
    pub segment_index: u64,
    pub retained_from_seq: u64,
    pub archive: ContentRef,
}

/// The head this partition's chain currently commits to.
///
/// Stored outside the segments and signed under the same keyring, so
/// truncating a segment does not move the head and rewriting the head
/// needs the root-only key. Both must agree before an append is
/// accepted; a mismatch is reported rather than repaired.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub v: u32,
    pub partition: String,
    pub owner_uid: u32,
    /// Writer epoch that last committed the head. Monotonic.
    pub epoch: u64,
    /// Sequence of the most recent record. 0 for an empty chain.
    pub seq: u64,
    /// MAC of the record at `seq`, or the genesis value.
    pub head_mac: String,

    /// Lowest sequence still retained across every segment on disk.
    /// 0 while the chain is empty.
    pub first_seq: u64,
    /// MAC the record at `first_seq` chains from. The genesis value
    /// while nothing has been trimmed.
    pub first_prev_mac: String,

    /// Segment currently being appended to.
    pub active_index: u64,
    /// First sequence the active segment holds. `seq + 1` while it is
    /// still empty.
    pub active_first_seq: u64,
    /// MAC the active segment's first record chains from.
    pub active_prev_mac: String,
    /// Bytes committed to the active segment. This is the definition of
    /// "committed": anything past it was never acknowledged.
    pub active_bytes: u64,

    /// Bytes committed across every retained segment.
    pub total_bytes: u64,
    /// Records the chain has ever carried.
    pub events: u64,
    /// Mutation brackets opened and not yet retired. Drives the
    /// computed closure reserve.
    pub open_brackets: u32,

    /// Records charged to each quota class, so a flood in one class
    /// cannot be laundered as another.
    pub closure_events: u64,
    pub control_events: u64,
    pub worker_events: u64,
    pub ingest_events: u64,

    /// A rotation whose retention record still has to be written.
    #[serde(default)]
    pub pending_retention: Option<PendingRetention>,

    pub key_id: String,
    pub updated_at_ms: u64,
    /// MAC over every field above.
    pub mac: String,
}

impl Anchor {
    pub fn empty(partition: &Partition, owner_uid: u32, key_id: &str) -> Self {
        Self {
            v: SCHEMA_VERSION,
            partition: partition.key(),
            owner_uid,
            epoch: 0,
            seq: 0,
            head_mac: GENESIS_MAC.to_string(),
            first_seq: 0,
            first_prev_mac: GENESIS_MAC.to_string(),
            active_index: 0,
            active_first_seq: 1,
            active_prev_mac: GENESIS_MAC.to_string(),
            active_bytes: 0,
            total_bytes: 0,
            events: 0,
            open_brackets: 0,
            closure_events: 0,
            control_events: 0,
            worker_events: 0,
            ingest_events: 0,
            pending_retention: None,
            key_id: key_id.to_string(),
            updated_at_ms: 0,
            mac: String::new(),
        }
    }

    /// Path of the segment appends currently land in.
    pub fn active_path(&self, root: &Path, partition: &Partition) -> PathBuf {
        partition.segment_path(root, self.active_index)
    }

    fn preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(320);
        push(&mut out, b"cos.session.journal.anchor");
        out.extend_from_slice(&self.v.to_be_bytes());
        push(&mut out, self.partition.as_bytes());
        out.extend_from_slice(&self.owner_uid.to_be_bytes());
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        push(&mut out, self.head_mac.as_bytes());
        out.extend_from_slice(&self.first_seq.to_be_bytes());
        push(&mut out, self.first_prev_mac.as_bytes());
        out.extend_from_slice(&self.active_index.to_be_bytes());
        out.extend_from_slice(&self.active_first_seq.to_be_bytes());
        push(&mut out, self.active_prev_mac.as_bytes());
        out.extend_from_slice(&self.active_bytes.to_be_bytes());
        out.extend_from_slice(&self.total_bytes.to_be_bytes());
        out.extend_from_slice(&self.events.to_be_bytes());
        out.extend_from_slice(&self.open_brackets.to_be_bytes());
        out.extend_from_slice(&self.closure_events.to_be_bytes());
        out.extend_from_slice(&self.control_events.to_be_bytes());
        out.extend_from_slice(&self.worker_events.to_be_bytes());
        out.extend_from_slice(&self.ingest_events.to_be_bytes());
        match &self.pending_retention {
            Some(pending) => {
                out.push(1);
                out.extend_from_slice(&pending.segment_index.to_be_bytes());
                out.extend_from_slice(&pending.retained_from_seq.to_be_bytes());
                push(&mut out, pending.archive.digest.as_str().as_bytes());
                out.extend_from_slice(&pending.archive.bytes.to_be_bytes());
            }
            None => out.push(0),
        }
        push(&mut out, self.key_id.as_bytes());
        out.extend_from_slice(&self.updated_at_ms.to_be_bytes());
        out
    }

    pub fn seal(&mut self, key: &[u8]) {
        self.mac = crate::crypto::hmac_sha256_hex(key, &self.preimage());
    }

    pub fn verify(&self, key: &[u8]) -> Result<(), JournalError> {
        let expected = crate::crypto::hmac_sha256_hex(key, &self.preimage());
        if expected == self.mac {
            Ok(())
        } else {
            Err(JournalError::Integrity(format!(
                "journal anchor for {} does not match its MAC",
                self.partition
            )))
        }
    }
}

fn push(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/partition.rs"
    ));
}
