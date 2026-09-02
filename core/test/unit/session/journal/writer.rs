use super::*;
use crate::session::journal::event::{JournalEvent, Label};
use crate::session::journal::harness::{privileged_probe, probe, Harness};
use crate::session::journal::quota;

#[test]
fn appends_chain_and_commit_a_head() {
    let harness = Harness::new();
    let first = harness.append(probe(1));
    let second = harness.append(probe(2));

    assert_eq!(first.seq, 1);
    assert_eq!(second.seq, 2);

    let anchor = harness.anchor();
    assert_eq!(anchor.seq, 2);
    assert_eq!(anchor.head_mac, second.mac);
    assert_eq!(anchor.events, 2);
    assert_eq!(anchor.control_events, 2);
    assert_eq!(
        anchor.active_bytes,
        std::fs::metadata(harness.active_path()).unwrap().len()
    );
    assert_eq!(anchor.total_bytes, anchor.active_bytes);
}

#[test]
fn a_worker_cannot_append_a_privileged_event() {
    let harness = Harness::new();
    let error = super::super::record(
        &harness.partition(),
        harness.owner_uid(),
        EventSource::Worker,
        privileged_probe(),
    )
    .expect_err("the ACL must refuse this");
    assert!(matches!(error, JournalError::Forbidden { .. }), "{error}");
    assert!(
        harness.lines().is_empty(),
        "a refused append must leave no bytes behind"
    );
}

#[test]
fn a_second_writer_in_this_process_cannot_take_the_lease() {
    let harness = Harness::new();
    let _held = harness.lease();
    // `lease_for` caches per root, so ask for the flock directly: this
    // is the same call a second daemon would make.
    let error = acquire(&harness.root()).expect_err("two writers must not coexist");
    assert!(
        error.to_string().contains("already holds"),
        "unexpected error: {error}"
    );
}

#[test]
fn the_writer_epoch_increases_across_restarts() {
    let harness = Harness::new();
    let first = harness.lease().epoch();
    harness.restart();
    let second = harness.lease().epoch();
    assert!(second > first, "{second} must be after {first}");
}

#[test]
fn a_stale_epoch_cannot_append() {
    let harness = Harness::new();
    harness.append(probe(1));

    // Forge a head committed by a *later* daemon than the one holding
    // the lease, which is what a stale worker would find.
    let mut anchor = harness.anchor();
    anchor.epoch = harness.lease().epoch() + 5;
    harness.commit_anchor(anchor);

    let error = super::super::record(
        &harness.partition(),
        harness.owner_uid(),
        EventSource::Kernel,
        probe(2),
    )
    .expect_err("a stale writer must fail closed");
    assert!(matches!(error, JournalError::StaleWriter { .. }), "{error}");
}

#[test]
fn truncating_the_chain_behind_the_head_fails_closed() {
    let harness = Harness::new();
    harness.append(probe(1));
    harness.append(probe(2));

    let lines = harness.lines();
    std::fs::write(harness.active_path(), format!("{}\n", lines[0])).unwrap();

    let error = super::super::record(
        &harness.partition(),
        harness.owner_uid(),
        EventSource::Kernel,
        probe(3),
    )
    .expect_err("a truncated chain must fail closed");
    assert!(matches!(error, JournalError::Truncated { .. }), "{error}");
}

#[test]
fn a_deleted_head_never_erases_the_chain() {
    // The blocker this replaces: a missing anchor used to be read as a
    // brand-new partition, and the tail reconciler then truncated every
    // committed byte away.
    let harness = Harness::new();
    harness.append(probe(1));
    harness.append(probe(2));
    let chain_path = harness.active_path();
    let before = std::fs::read(&chain_path).unwrap();
    assert!(!before.is_empty());

    std::fs::remove_file(harness.anchor_path()).unwrap();

    let error = super::super::record(
        &harness.partition(),
        harness.owner_uid(),
        EventSource::Kernel,
        probe(3),
    )
    .expect_err("a missing head must fail closed");
    assert!(
        matches!(error, JournalError::AnchorMissing { .. }),
        "{error}"
    );
    assert_eq!(
        std::fs::read(&chain_path).unwrap(),
        before,
        "the committed bytes must be preserved for the operator"
    );
}

#[test]
fn a_torn_tail_is_discarded_and_alarmed() {
    let harness = Harness::new();
    harness.append(probe(1));
    let committed = std::fs::metadata(harness.active_path()).unwrap().len();

    // A crash between the chain write and the head commit leaves bytes
    // nobody was ever told about.
    let mut torn = std::fs::read(harness.active_path()).unwrap();
    torn.extend_from_slice(b"{\"v\":1,\"seq\":2,\"partial");
    std::fs::write(harness.active_path(), torn).unwrap();

    super::super::alarm::reset();
    let appended = super::super::record(
        &harness.partition(),
        harness.owner_uid(),
        EventSource::Kernel,
        probe(2),
    )
    .expect("the writer reconciles an uncommitted tail");

    assert_eq!(appended.seq, 2);
    assert_eq!(harness.lines().len(), 2, "the torn bytes must be gone");
    assert!(std::fs::metadata(harness.active_path()).unwrap().len() > committed);
    assert!(super::super::alarm::recent(10)
        .iter()
        .any(|record| record.get("class").and_then(|v| v.as_str()) == Some("journal.torn-append")));
}

#[test]
fn a_failed_chain_write_leaves_the_head_untouched() {
    let harness = Harness::new();
    harness.append(probe(1));
    let before = harness.anchor();

    super::super::faults::arm(super::super::faults::Fault::AppendWrite);
    let error = super::super::record(
        &harness.partition(),
        harness.owner_uid(),
        EventSource::Kernel,
        probe(2),
    )
    .expect_err("an unwritable chain must refuse");
    super::super::faults::disarm();
    assert!(matches!(error, JournalError::Io { .. }), "{error}");

    let after = harness.anchor();
    assert_eq!(before.seq, after.seq);
    assert_eq!(before.head_mac, after.head_mac);
}

#[test]
fn a_failed_head_commit_is_reported_as_uncommitted() {
    let harness = Harness::new();
    harness.append(probe(1));

    super::super::faults::arm(super::super::faults::Fault::AnchorCommit);
    let error = super::super::record(
        &harness.partition(),
        harness.owner_uid(),
        EventSource::Kernel,
        probe(2),
    )
    .expect_err("an uncommittable head must not report success");
    super::super::faults::disarm();
    assert!(
        matches!(error, JournalError::HeadUncommitted { .. }),
        "{error}"
    );

    // The next append discards the unacknowledged bytes and carries on.
    let appended = harness.append(probe(3));
    assert_eq!(appended.seq, 2);
}

#[test]
fn an_oversized_event_is_refused() {
    let harness = Harness::new();
    let error = super::super::record(
        &harness.partition(),
        harness.owner_uid(),
        EventSource::Kernel,
        JournalEvent::SessionFailed {
            class: Label::new("provider_error"),
            detail: crate::audit_policy::TextDigest {
                bytes: 1,
                digest: "a".repeat(super::super::event::MAX_EVENT_BYTES),
            },
        },
    )
    .expect_err("an oversized body must be refused");
    assert!(matches!(error, JournalError::Quota(_)), "{error}");
}

// ---------------------------------------------------------------------------
// Rotation
// ---------------------------------------------------------------------------

#[test]
fn rotation_is_one_commit_and_moves_no_files() {
    let harness = Harness::new();
    for turn in 0..3 {
        harness.append(probe(turn));
    }
    let before = harness.anchor();
    let old_segment = harness.segment_path(before.active_index);
    let old_bytes = std::fs::read(&old_segment).unwrap();

    let pending = {
        let lease = harness.lease();
        lease
            .rotate_active(&harness.partition(), harness.owner_uid())
            .expect("rotate")
            .expect("something to cut")
    };

    let after = harness.anchor();
    assert_eq!(after.active_index, before.active_index + 1);
    assert_eq!(after.active_first_seq, before.seq + 1);
    assert_eq!(after.active_prev_mac, before.head_mac);
    assert_eq!(after.active_bytes, 0);
    assert_eq!(after.seq, before.seq, "rotation appends nothing");
    assert_eq!(
        std::fs::read(&old_segment).unwrap(),
        old_bytes,
        "the closed segment is never moved or rewritten"
    );
    assert!(!harness.segment_path(after.active_index).exists());
    assert_eq!(after.pending_retention.as_ref().unwrap(), &pending);
}

#[test]
fn a_crash_right_after_the_rotation_commit_is_reader_valid() {
    // This is the state the previous design could not represent: the
    // head says a new segment is active and its file does not exist yet.
    let harness = Harness::new();
    for turn in 0..3 {
        harness.append(probe(turn));
    }
    {
        let lease = harness.lease();
        lease
            .rotate_active(&harness.partition(), harness.owner_uid())
            .expect("rotate");
    }

    harness.cold_restart();
    let health = harness.health();
    assert!(health.is_verified(), "{health:?}");

    // And a mutation can still be recorded and closed.
    let bracket = super::super::begin_mutation(super::super::MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.service.control",
        request_key: "after-rotation",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket opens across a rotation boundary");
    bracket.commit().expect("commit");
    assert!(harness.health().is_verified());
}

#[test]
fn a_crash_before_the_rotation_commit_leaves_the_old_chain_valid() {
    let harness = Harness::new();
    for turn in 0..3 {
        harness.append(probe(turn));
    }
    let before = harness.anchor();

    // Nothing was committed, so this is exactly the pre-rotation state.
    harness.cold_restart();
    let after = harness.anchor();
    assert_eq!(after.active_index, before.active_index);
    assert_eq!(after.seq, before.seq);
    assert!(harness.health().is_verified());
}

#[test]
fn the_retention_record_is_written_exactly_once_across_a_crash() {
    let harness = Harness::new();
    for turn in 0..3 {
        harness.append(probe(turn));
    }
    {
        let lease = harness.lease();
        lease
            .rotate_active(&harness.partition(), harness.owner_uid())
            .expect("rotate");
    }
    assert!(harness.anchor().pending_retention.is_some());

    // A crash before the retention record: the next append writes it,
    // and the marker clears in the same commit.
    harness.cold_restart();
    harness.append(probe(9));

    assert!(harness.anchor().pending_retention.is_none());
    let lease = harness.lease();
    let chain = super::super::reader::read(
        &harness.root(),
        &harness.partition(),
        harness.owner_uid(),
        lease.keyring(),
    )
    .unwrap();
    let retentions = chain
        .records
        .iter()
        .filter(|record| record.event.kind() == "retention_applied")
        .count();
    assert_eq!(retentions, 1, "exactly once, not at-least-once");
    assert!(chain.health.is_verified(), "{:?}", chain.health);
}

#[test]
fn rotation_preserves_chain_continuity_across_segments() {
    let harness = Harness::new();
    // Enough real records to cross the rotation size and keep going, so
    // the segments under test are ones the writer actually produced.
    for turn in 0..40 {
        harness.append(probe(turn));
    }

    let lease = harness.lease();
    let chain = super::super::reader::read(
        &harness.root(),
        &harness.partition(),
        harness.owner_uid(),
        lease.keyring(),
    )
    .unwrap();
    assert!(chain.health.is_verified(), "{:?}", chain.health);
    assert!(
        harness.anchor().active_index >= 1,
        "the test must actually rotate"
    );
    // Sequences are contiguous across the segment boundary and the
    // chain links hold, which is what makes an archived segment
    // evidence rather than a gap.
    for pair in chain.records.windows(2) {
        assert_eq!(pair[1].seq, pair[0].seq + 1);
        assert_eq!(pair[1].prev, pair[0].mac);
    }
    assert!(harness.segment_path(0).exists());
    assert!(harness.segment_path(1).exists());
    assert!(chain
        .records
        .iter()
        .any(|record| record.event.kind() == "retention_applied"));
}

#[test]
fn a_mutation_close_that_triggers_rotation_still_closes() {
    let harness = Harness::new();
    // Fill the active segment to just under the rotation size, then open
    // a bracket, so closing it is what crosses the boundary.
    while harness.anchor().active_bytes < quota::ROTATE_BYTES - 512 {
        harness.append(probe(0));
    }
    let bracket = super::super::begin_mutation(super::super::MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.package.install",
        request_key: "rotating-close",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket");
    while harness.anchor().active_bytes < quota::ROTATE_BYTES {
        harness.append(probe(0));
    }

    bracket.commit().expect("a close must survive rotation");
    assert!(harness.anchor().active_index >= 1, "rotation happened");
    let lease = harness.lease();
    let chain = super::super::reader::read(
        &harness.root(),
        &harness.partition(),
        harness.owner_uid(),
        lease.keyring(),
    )
    .unwrap();
    assert!(chain.health.is_verified(), "{:?}", chain.health);
    assert!(chain
        .records
        .iter()
        .any(|record| record.event.kind() == "mutation_committed"));
}

#[test]
fn a_closure_record_is_written_even_when_maintenance_fails() {
    let harness = Harness::new();
    let bracket = super::super::begin_mutation(super::super::MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.service.control",
        request_key: "maintenance-fails",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket");

    // Leave a pending retention that names a sequence no append can
    // satisfy, so maintenance keeps failing to clear it.
    let mut anchor = harness.anchor();
    anchor.pending_retention = Some(super::super::partition::PendingRetention {
        segment_index: 0,
        retained_from_seq: u64::MAX,
        archive: super::super::ContentRef::of(super::super::ContentStore::SessionTurns, b"x"),
    });
    harness.commit_anchor(anchor);

    bracket
        .commit()
        .expect("a close must not be blocked by maintenance");
    assert!(harness.health().is_verified());
}
