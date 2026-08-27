use super::*;
use crate::session::journal::event::{RecoverySource, Resolution};
use crate::session::journal::harness::{probe, Harness};
use crate::session::journal::{begin_mutation, MutationStart};

fn open_bracket(harness: &Harness, route: &'static str, key: &str) {
    let bracket = begin_mutation(MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route,
        request_key: key,
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket opens");
    // Deliberately dropped without closing: this is the crash.
    std::mem::forget(bracket);
}

fn kinds(harness: &Harness) -> Vec<&'static str> {
    let lease = harness.lease();
    super::super::reader::read(
        &harness.root(),
        &harness.partition(),
        harness.owner_uid(),
        lease.keyring(),
    )
    .unwrap()
    .records
    .iter()
    .map(|record| record.event.kind())
    .collect()
}

#[test]
fn an_unclosed_bracket_becomes_an_orphan_not_a_success() {
    let harness = Harness::new();
    open_bracket(&harness, "system.package.install", "req-1");

    harness.cold_restart();
    let lease = harness.lease();
    let report = run(&lease, RecoverySource::DaemonStart).expect("recovery");

    assert_eq!(report.orphans.len(), 1);
    assert_eq!(report.orphans[0].route, "system.package.install");
    assert!(report.quarantined.is_empty());

    let kinds = kinds(&harness);
    assert!(kinds.contains(&"mutation_orphaned"));
    assert!(
        !kinds.contains(&"mutation_committed"),
        "recovery must never claim an orphan committed"
    );
}

#[test]
fn a_closed_bracket_is_not_an_orphan() {
    let harness = Harness::new();
    let bracket = begin_mutation(MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.service.control",
        request_key: "req-2",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket");
    bracket.commit().expect("commit");

    harness.cold_restart();
    let lease = harness.lease();
    let report = run(&lease, RecoverySource::DaemonStart).expect("recovery");
    assert!(report.orphans.is_empty());
}

#[test]
fn an_orphan_is_flagged_once_but_stays_unresolved_forever() {
    // The blocker this replaces: the orphan record used to *close* the
    // bracket, so the next restart forgot about it and the replay was
    // accepted.
    let harness = Harness::new();
    open_bracket(&harness, "system.package.install", "req-3");

    for _ in 0..3 {
        harness.cold_restart();
        let report = super::super::startup_recovery(RecoverySource::DaemonStart).expect("recovery");
        assert_eq!(
            report.orphans.len(),
            1,
            "an unresolved bracket must stay unresolved across restarts"
        );
        assert!(super::super::replays_unresolved(
            &harness.partition(),
            "system.package.install",
            "req-3"
        ));
    }

    let orphan_records = kinds(&harness)
        .iter()
        .filter(|kind| **kind == "mutation_orphaned")
        .count();
    assert_eq!(orphan_records, 1, "flagged once, not once per restart");
}

#[test]
fn an_indeterminate_bracket_is_not_treated_as_closed() {
    let harness = Harness::new();
    let bracket = begin_mutation(MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.storage.control",
        request_key: "req-4",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket");

    super::super::faults::arm(super::super::faults::Fault::AppendWrite);
    let _ = bracket.commit().expect_err("close fails");
    super::super::faults::disarm();

    harness.cold_restart();
    let report = super::super::startup_recovery(RecoverySource::DaemonStart).expect("recovery");
    assert_eq!(
        report.orphans.len(),
        1,
        "an indeterminate outcome is unknown, not resolved"
    );
    assert!(super::super::replays_unresolved(
        &harness.partition(),
        "system.storage.control",
        "req-4"
    ));
}

#[test]
fn only_an_explicit_resolution_lifts_the_refusal() {
    let harness = Harness::new();
    open_bracket(&harness, "system.package.install", "req-5");
    harness.cold_restart();
    let report = super::super::startup_recovery(RecoverySource::DaemonStart).expect("recovery");
    let operation = report.orphans[0].operation.clone();
    assert!(super::super::replays_unresolved(
        &harness.partition(),
        "system.package.install",
        "req-5"
    ));

    super::super::resolve_mutation(
        &harness.partition(),
        harness.owner_uid(),
        &operation,
        Resolution::Abandoned,
        0,
    )
    .expect("an operator may record what happened");

    assert!(
        !super::super::replays_unresolved(&harness.partition(), "system.package.install", "req-5"),
        "a resolved bracket no longer refuses its replay"
    );

    // And the resolution survives a restart.
    harness.cold_restart();
    let report = super::super::startup_recovery(RecoverySource::DaemonStart).expect("recovery");
    assert!(report.orphans.is_empty());
}

#[test]
fn resolving_something_that_is_not_unresolved_is_refused() {
    let harness = Harness::new();
    harness.append(probe(1));
    let error = super::super::resolve_mutation(
        &harness.partition(),
        harness.owner_uid(),
        "0123456789abcdef",
        Resolution::Committed,
        0,
    )
    .expect_err("there is nothing to resolve");
    assert!(
        matches!(error, JournalError::NotUnresolved { .. }),
        "{error}"
    );
}

#[test]
fn a_damaged_partition_is_quarantined_and_mutations_fail_closed() {
    let harness = Harness::new();
    harness.append(probe(1));
    harness.append(probe(2));

    // Drop the tail of the chain behind the daemon's back.
    let lines = harness.lines();
    std::fs::write(harness.active_path(), format!("{}\n", lines[0])).unwrap();

    harness.cold_restart();
    let lease = harness.lease();
    let report = run(&lease, RecoverySource::DaemonStart).expect("recovery");
    assert_eq!(report.quarantined, vec![harness.partition().key()]);
    assert!(is_quarantined(&harness.partition()));

    let error = begin_mutation(MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.package.install",
        request_key: "req-6",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect_err("a quarantined partition must refuse mutations");
    assert!(matches!(error, JournalError::Quarantined(_)), "{error}");
}

#[test]
fn a_deleted_head_quarantines_rather_than_erasing() {
    let harness = Harness::new();
    harness.append(probe(1));
    let chain_path = harness.active_path();
    let bytes = std::fs::read(&chain_path).unwrap();
    std::fs::remove_file(harness.anchor_path()).unwrap();

    harness.cold_restart();
    let lease = harness.lease();
    let report = run(&lease, RecoverySource::DaemonStart).expect("recovery");
    assert_eq!(report.quarantined, vec![harness.partition().key()]);
    assert_eq!(
        std::fs::read(&chain_path).unwrap(),
        bytes,
        "recovery must preserve the bytes it could not verify"
    );
}

#[test]
fn a_scan_records_what_it_examined() {
    let harness = Harness::new();
    harness.append(probe(1));

    harness.cold_restart();
    let lease = harness.lease();
    run(&lease, RecoverySource::SessionResume).expect("recovery");

    let chain = super::super::reader::read(
        &harness.root(),
        &harness.partition(),
        harness.owner_uid(),
        lease.keyring(),
    )
    .unwrap();
    let scan = chain
        .records
        .iter()
        .find(|record| record.event.kind() == "recovery_scanned")
        .expect("a scan record");
    assert_eq!(scan.source, super::super::EventSource::Recovery);
    assert_eq!(scan.epoch, lease.epoch());
}
