//! Views rebuilt from the chain.
//!
//! The journal is the order and lifecycle authority; the surfaces users
//! and the agent already read are *projections* of it. Building them
//! here rather than writing them a second time at each call site is
//! what stops two records of the same event from disagreeing: a
//! projection has no state of its own, so re-running it on the same
//! chain always produces the same answer.
//!
//! Conversation bodies, the memory database and its index stay
//! owner-private content stores. They are not projections and are not
//! rebuilt from here — the journal records their immutable
//! content-addressed references, and those references are what makes a
//! projection able to point at content it must never copy.

use serde::Serialize;

use super::event::JournalEvent;
use super::partition::Partition;
use super::reader::{self, Health};
use super::JournalError;

/// One bracketed durable mutation, as the chain saw it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MutationEntry {
    pub operation: String,
    pub route: String,
    pub started_seq: u64,
    pub started_epoch: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_mutation: Option<u64>,
    /// `started`, `committed`, `failed`, `indeterminate`, `orphaned` or
    /// one of the `resolved-*` outcomes an operator recorded.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<String>,
}

/// One agent lifecycle step, in chain order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LifecycleEntry {
    pub seq: u64,
    pub kind: &'static str,
    pub source: super::acl::EventSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<super::event::Trust>,
}

/// Everything a projection pass produced for one partition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Projection {
    pub partition: String,
    pub health: Health,
    pub head_seq: u64,
    pub mutations: Vec<MutationEntry>,
    pub lifecycle: Vec<LifecycleEntry>,
}

/// Rebuild both projections for one partition.
pub fn build(partition: &Partition, owner_uid: u32) -> Result<Projection, JournalError> {
    let lease = super::lease()?;
    let chain = reader::read(lease.root(), partition, owner_uid, lease.keyring())?;

    let mut mutations: Vec<MutationEntry> = Vec::new();
    let mut lifecycle = Vec::new();

    for record in &chain.records {
        match &record.event {
            JournalEvent::MutationStarted {
                operation,
                route,
                grant,
                session_mutation,
                ..
            } => mutations.push(MutationEntry {
                operation: operation.as_str().to_string(),
                route: route.as_str().to_string(),
                started_seq: record.seq,
                started_epoch: record.epoch,
                grant: grant.as_ref().map(|grant| grant.as_str().to_string()),
                session_mutation: *session_mutation,
                status: "started",
                closed_seq: None,
                duration_ms: None,
                failure_class: None,
            }),
            JournalEvent::MutationCommitted {
                operation,
                duration_ms,
            } => close(&mut mutations, operation.as_str(), record.seq, |entry| {
                entry.status = "committed";
                entry.duration_ms = Some(*duration_ms);
            }),
            JournalEvent::MutationFailed {
                operation,
                duration_ms,
                class,
                ..
            } => close(&mut mutations, operation.as_str(), record.seq, |entry| {
                entry.status = "failed";
                entry.duration_ms = Some(*duration_ms);
                entry.failure_class = Some(class.as_str().to_string());
            }),
            JournalEvent::MutationIndeterminate { operation, .. } => {
                close(&mut mutations, operation.as_str(), record.seq, |entry| {
                    entry.status = "indeterminate";
                })
            }
            JournalEvent::MutationOrphaned { operation, .. } => {
                close(&mut mutations, operation.as_str(), record.seq, |entry| {
                    entry.status = "orphaned";
                })
            }
            JournalEvent::MutationResolved {
                operation, outcome, ..
            } => close(&mut mutations, operation.as_str(), record.seq, |entry| {
                entry.status = match outcome {
                    super::event::Resolution::Abandoned => "resolved-abandoned",
                    super::event::Resolution::Committed => "resolved-committed",
                    super::event::Resolution::RolledBack => "resolved-rolled-back",
                };
            }),
            _ => {}
        }

        if let Some(entry) = lifecycle_entry(record) {
            lifecycle.push(entry);
        }
    }

    Ok(Projection {
        partition: partition.key(),
        head_seq: chain.anchor.seq,
        health: chain.health,
        mutations,
        lifecycle,
    })
}

fn close(
    mutations: &mut [MutationEntry],
    operation: &str,
    seq: u64,
    apply: impl FnOnce(&mut MutationEntry),
) {
    if let Some(entry) = mutations
        .iter_mut()
        .rev()
        .find(|entry| entry.operation == operation)
    {
        entry.closed_seq = Some(seq);
        apply(entry);
    }
}

fn lifecycle_entry(record: &super::record::JournalRecord) -> Option<LifecycleEntry> {
    let kind = record.event.kind();
    let base = |turn: Option<u32>| LifecycleEntry {
        seq: record.seq,
        kind,
        source: record.source,
        turn,
        tool: None,
        content_digest: None,
        trust: None,
    };
    Some(match &record.event {
        JournalEvent::SessionStarted { .. }
        | JournalEvent::SessionResumed { .. }
        | JournalEvent::SessionCompleted { .. }
        | JournalEvent::SessionFailed { .. } => base(None),
        JournalEvent::SessionCancelled { turn, .. } => base(*turn),
        JournalEvent::UserRequestRecorded {
            turn,
            content,
            trust,
            ..
        } => LifecycleEntry {
            content_digest: Some(content.digest.as_str().to_string()),
            trust: Some(*trust),
            ..base(Some(*turn))
        },
        JournalEvent::ModelResponseRecorded { turn, content, .. } => LifecycleEntry {
            content_digest: Some(content.digest.as_str().to_string()),
            ..base(Some(*turn))
        },
        JournalEvent::ModelTurnCompleted { turn, .. } => base(Some(*turn)),
        JournalEvent::PromptSnapshotRecorded { turn, snapshot, .. } => LifecycleEntry {
            content_digest: Some(snapshot.digest.as_str().to_string()),
            ..base(Some(*turn))
        },
        JournalEvent::PromptSegmentInjected {
            turn,
            segment,
            trust,
            ..
        } => LifecycleEntry {
            content_digest: Some(segment.digest.as_str().to_string()),
            trust: Some(*trust),
            ..base(Some(*turn))
        },
        JournalEvent::ToolProposed { turn, tool, .. }
        | JournalEvent::ToolStarted { turn, tool, .. }
        | JournalEvent::ToolFinished { turn, tool, .. } => LifecycleEntry {
            tool: Some(tool.as_str().to_string()),
            ..base(Some(*turn))
        },
        _ => return None,
    })
}

/// The projection as the system-operations surface presents it.
///
/// `clawd`'s operations journal keeps its own append-only file for
/// non-journal sources, but for durable mutations it carries only a
/// reference to the chain — so this is the rebuildable answer to "what
/// happened", and the operations file is an index into it rather than a
/// second authority.
pub fn system_operations(
    partition: &Partition,
    owner_uid: u32,
) -> Result<serde_json::Value, JournalError> {
    let projection = build(partition, owner_uid)?;
    Ok(serde_json::json!({
        "schema": 1,
        "source": "session.journal",
        "partition": projection.partition,
        "head_seq": projection.head_seq,
        "health": projection.health,
        "operations": projection.mutations,
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/projection.rs"
    ));
}
