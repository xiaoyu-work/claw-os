// Shared setup for the journal's unit tests.
//
// Every test here writes real files and takes a real flock, because
// that is what the properties under test are made of. The harness
// therefore rebinds `COS_DATA_DIR` to a tempdir, takes the process-wide
// env lock, and drops the cached writer lease on both ends so one
// test's epoch never leaks into the next.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::MutexGuard;

use tempfile::TempDir;

use super::acl::EventSource;
use super::event::{JournalEvent, Label, OperationId};
use super::partition::{Anchor, Partition};
use super::writer::WriterLease;

pub(crate) struct Harness {
    _lock: MutexGuard<'static, ()>,
    previous: Option<OsString>,
    tmp: TempDir,
}

impl Harness {
    pub(crate) fn new() -> Self {
        let lock = crate::test_env::lock_env();
        super::writer::release_all();
        super::recovery::clear_quarantine();
        super::clear_unresolved();
        super::alarm::reset();
        let tmp = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var_os("COS_DATA_DIR");
        std::env::set_var("COS_DATA_DIR", tmp.path());
        Self {
            _lock: lock,
            previous,
            tmp,
        }
    }

    pub(crate) fn data_dir(&self) -> PathBuf {
        self.tmp.path().to_path_buf()
    }

    pub(crate) fn root(&self) -> PathBuf {
        super::root()
    }

    pub(crate) fn lease(&self) -> std::sync::Arc<WriterLease> {
        super::lease().expect("writer lease")
    }

    /// A partition owned by this process's uid, so the ownership checks
    /// the writer makes are the real ones.
    pub(crate) fn partition(&self) -> Partition {
        Partition::Owner(current_uid())
    }

    pub(crate) fn owner_uid(&self) -> u32 {
        current_uid()
    }

    pub(crate) fn anchor(&self) -> Anchor {
        self.lease()
            .load_anchor(&self.partition(), self.owner_uid())
            .expect("anchor")
    }

    /// Overwrite the committed head with a hand-made one. Used to reach
    /// quota and epoch states a test would otherwise need minutes of
    /// appends to produce.
    pub(crate) fn commit_anchor(&self, mut anchor: Anchor) {
        let lease = self.lease();
        anchor.seal(lease.keyring().active_key());
        super::write_durable(
            &self.anchor_path(),
            &serde_json::to_vec(&anchor).expect("encode anchor"),
        )
        .expect("commit anchor");
    }

    /// Append a benign control event and return where it landed.
    pub(crate) fn append(&self, event: JournalEvent) -> super::writer::Appended {
        super::record(
            &self.partition(),
            self.owner_uid(),
            EventSource::Kernel,
            event,
        )
        .expect("append")
    }

    /// Path of the segment appends currently land in.
    pub(crate) fn active_path(&self) -> PathBuf {
        self.anchor().active_path(&self.root(), &self.partition())
    }

    pub(crate) fn segment_path(&self, index: u64) -> PathBuf {
        self.partition().segment_path(&self.root(), index)
    }

    pub(crate) fn anchor_path(&self) -> PathBuf {
        self.partition().anchor_path(&self.root())
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(self.active_path())
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    /// Every stored byte of the chain, across all segments.
    pub(crate) fn chain_text(&self) -> String {
        let partition = self.partition();
        partition
            .segments(&self.root())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|index| {
                std::fs::read_to_string(partition.segment_path(&self.root(), index)).ok()
            })
            .collect()
    }

    pub(crate) fn health(&self) -> super::reader::Health {
        let lease = self.lease();
        super::reader::read(
            &self.root(),
            &self.partition(),
            self.owner_uid(),
            lease.keyring(),
        )
        .expect("read")
        .health
    }

    /// Simulate a daemon restart: drop the lease so the next use
    /// acquires a fresh one with the next epoch.
    pub(crate) fn restart(&self) {
        super::writer::release_all();
    }

    /// Simulate a *cold* restart: a fresh daemon that has never seen
    /// this partition, so everything it knows comes off disk.
    pub(crate) fn cold_restart(&self) {
        super::writer::release_all();
        super::clear_unresolved();
        super::recovery::clear_quarantine();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        super::writer::release_all();
        super::recovery::clear_quarantine();
        super::clear_unresolved();
        super::faults::disarm();
        match self.previous.take() {
            Some(value) => std::env::set_var("COS_DATA_DIR", value),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}

pub(crate) fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() as u32 }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// A cheap, always-legal control event for tests that only care about
/// chain mechanics.
pub(crate) fn probe(turn: u32) -> JournalEvent {
    JournalEvent::ToolStarted {
        turn,
        tool: Label::new("cos_todo"),
        tool_use_id: Label::new("tool-1"),
        known: true,
    }
}

/// A closure-class event, for tests about the reserve.
pub(crate) fn closure_probe() -> JournalEvent {
    JournalEvent::RecoveryScanned {
        detected_by: super::event::RecoverySource::DaemonStart,
        writer_epoch: 1,
        events: 0,
        orphans: 0,
    }
}

/// A privileged event a worker may never write.
pub(crate) fn privileged_probe() -> JournalEvent {
    JournalEvent::MutationStarted {
        operation: OperationId::generate(),
        route: Label::new("system.package.install"),
        idempotency: crate::audit_policy::text_digest("k"),
        grant: None,
        session_mutation: None,
    }
}
