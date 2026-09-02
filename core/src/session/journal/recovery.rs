//! Startup and resume recovery, and the unresolved-bracket set.
//!
//! Recovery answers one question per partition: *is there a privileged
//! mutation this machine started and never resolved?* It answers it from
//! the chain, not from the effect, because the effect is exactly what is
//! unknown.
//!
//! What recovery will do:
//!
//! * verify every partition and record a [`JournalEvent::RecoveryScanned`];
//! * mark each unresolved [`JournalEvent::MutationStarted`] as
//!   [`JournalEvent::MutationOrphaned`], once, carrying the epoch that
//!   opened it;
//! * publish the unresolved set so a replay of the same durable
//!   operation identity is refused for as long as it stays unresolved.
//!
//! What recovery will never do:
//!
//! * claim an orphan committed;
//! * re-run a mutation. Nothing here knows whether
//!   `system.package.install` got as far as `dpkg`, and a non-idempotent
//!   operation replayed on a guess is worse than an operator reading an
//!   explicit "indeterminate" and deciding;
//! * clear an orphan. `MutationOrphaned` and `MutationIndeterminate` are
//!   *flags*, not resolutions: they say the outcome is unknown, which is
//!   the state a replay has to keep being refused in. Only a definite
//!   outcome or an operator's [`JournalEvent::MutationResolved`] retires
//!   a bracket, so the refusal survives any number of restarts.
//!
//! A partition whose chain does not verify is quarantined: mutations on
//! it fail closed, and its records stay readable so an operator can see
//! what the damage is.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

use super::acl::EventSource;
use super::event::{JournalEvent, Label, OperationId, RecoverySource};
use super::partition::Partition;
use super::reader::{self, Health};
use super::writer::{Append, WriterLease};
use super::{alarm, JournalError};

/// One mutation this machine started and has not resolved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Unresolved {
    pub partition: String,
    pub operation: String,
    pub route: String,
    pub seq: u64,
    pub opened_in_epoch: u64,
    /// Keyed digest of the durable operation identity the bracket was
    /// opened under, so a replay is recognisable after a restart without
    /// the identity itself being stored.
    pub idempotency: String,
    /// Whether an orphan record has already been written for it.
    pub flagged: bool,
}

/// What one recovery pass found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub partitions: usize,
    pub verified: usize,
    pub quarantined: Vec<String>,
    pub orphans: Vec<Unresolved>,
}

/// Verify every partition and close the books on the previous lifetime.
pub fn run(lease: &WriterLease, detected_by: RecoverySource) -> Result<Report, JournalError> {
    let root = lease.root().to_path_buf();
    let mut report = Report::default();
    for partition in super::partition::list(&root)? {
        report.partitions += 1;
        match recover_partition(lease, &partition, detected_by) {
            Ok(PartitionOutcome::Verified(unresolved)) => {
                report.verified += 1;
                report.orphans.extend(unresolved);
            }
            Ok(PartitionOutcome::Quarantined(detail)) => {
                alarm::raise(alarm::Class::IntegrityFailed, &partition.key(), &detail);
                quarantine(&partition);
                report.quarantined.push(partition.key());
            }
            Err(error) => {
                alarm::raise(
                    alarm::Class::IntegrityFailed,
                    &partition.key(),
                    &error.to_string(),
                );
                quarantine(&partition);
                report.quarantined.push(partition.key());
            }
        }
    }
    Ok(report)
}

enum PartitionOutcome {
    Verified(Vec<Unresolved>),
    Quarantined(String),
}

/// Every bracket in `records` that no definite outcome or operator
/// resolution has retired, in chain order.
pub fn unresolved_in(
    partition: &Partition,
    records: &[super::record::JournalRecord],
) -> Vec<Unresolved> {
    let mut open: BTreeMap<OperationId, Unresolved> = BTreeMap::new();
    let mut resolved: HashSet<OperationId> = HashSet::new();
    let mut flagged: HashSet<OperationId> = HashSet::new();

    for record in records {
        match &record.event {
            JournalEvent::MutationStarted {
                operation,
                route,
                idempotency,
                ..
            } => {
                open.insert(
                    operation.clone(),
                    Unresolved {
                        partition: partition.key(),
                        operation: operation.as_str().to_string(),
                        route: route.as_str().to_string(),
                        seq: record.seq,
                        opened_in_epoch: record.epoch,
                        idempotency: idempotency.digest.clone(),
                        flagged: false,
                    },
                );
            }
            event if event.resolves_mutation() => {
                if let Some(operation) = event.operation() {
                    resolved.insert(operation.clone());
                }
            }
            event if event.flags_mutation() => {
                if let Some(operation) = event.operation() {
                    flagged.insert(operation.clone());
                }
            }
            _ => {}
        }
    }

    open.retain(|operation, _| !resolved.contains(operation));
    open.into_iter()
        .map(|(operation, mut entry)| {
            entry.flagged = flagged.contains(&operation);
            entry
        })
        .collect()
}

fn recover_partition(
    lease: &WriterLease,
    partition: &Partition,
    detected_by: RecoverySource,
) -> Result<PartitionOutcome, JournalError> {
    let chain = reader::read(
        lease.root(),
        partition,
        owner_hint(partition),
        lease.keyring(),
    )?;
    if let Health::Damaged { detail } = &chain.health {
        return Ok(PartitionOutcome::Quarantined(detail.clone()));
    }
    let owner_uid = chain.anchor.owner_uid;
    let unresolved = unresolved_in(partition, &chain.records);

    for entry in &unresolved {
        // Flag each bracket exactly once. A second orphan record for the
        // same operation on every restart would grow the chain without
        // saying anything new — and the refusal comes from the bracket
        // still being unresolved, not from the record.
        if entry.flagged {
            continue;
        }
        let Some(operation) = chain.records.iter().find_map(|record| {
            record
                .event
                .operation()
                .filter(|candidate| candidate.as_str() == entry.operation)
                .cloned()
        }) else {
            continue;
        };
        lease.append(Append {
            partition,
            owner_uid,
            source: EventSource::Recovery,
            event: JournalEvent::MutationOrphaned {
                operation,
                route: Label::new(&entry.route),
                detected_by,
                opened_in_epoch: entry.opened_in_epoch,
            },
            context_ingest: false,
        })?;
        alarm::raise(
            alarm::Class::OrphanedMutation,
            &entry.partition,
            &format!(
                "route {} opened at seq {} in epoch {} was never resolved",
                entry.route, entry.seq, entry.opened_in_epoch
            ),
        );
    }

    lease.append(Append {
        partition,
        owner_uid,
        source: EventSource::Recovery,
        event: JournalEvent::RecoveryScanned {
            detected_by,
            writer_epoch: lease.epoch(),
            events: chain.anchor.events,
            orphans: unresolved.len() as u32,
        },
        context_ingest: false,
    })?;

    Ok(PartitionOutcome::Verified(unresolved))
}

fn owner_hint(partition: &Partition) -> u32 {
    match partition {
        Partition::Owner(uid) => *uid,
        Partition::Session(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// Quarantine
// ---------------------------------------------------------------------------

use std::sync::Mutex;

static QUARANTINED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Refuse further mutations on a partition whose chain does not verify.
pub fn quarantine(partition: &Partition) {
    if let Ok(mut guard) = QUARANTINED.lock() {
        guard
            .get_or_insert_with(HashSet::new)
            .insert(partition.key());
    }
}

/// Whether mutations on this partition must fail closed.
pub fn is_quarantined(partition: &Partition) -> bool {
    QUARANTINED
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|set| set.contains(&partition.key())))
        .unwrap_or(false)
}

/// Every partition currently refusing mutations.
pub fn quarantined() -> Vec<String> {
    QUARANTINED
        .lock()
        .ok()
        .and_then(|guard| {
            guard.as_ref().map(|set| {
                let mut names: Vec<String> = set.iter().cloned().collect();
                names.sort();
                names
            })
        })
        .unwrap_or_default()
}

/// Clear quarantine state between tests and daemon lifetimes.
#[cfg(test)]
pub fn clear_quarantine() {
    if let Ok(mut guard) = QUARANTINED.lock() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/recovery.rs"
    ));
}
