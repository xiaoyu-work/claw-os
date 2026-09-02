use super::*;
use crate::session::journal::harness::Harness;

#[test]
fn an_alarm_reaches_its_own_file_and_is_bounded() {
    let harness = Harness::new();
    reset();

    raise(
        Class::IntegrityFailed,
        "owner/1000",
        "chain did not verify at seq 4",
    );

    let recorded = recent(10);
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].get("class").and_then(|v| v.as_str()),
        Some("journal.integrity-failed")
    );
    assert_eq!(
        recorded[0].get("partition").and_then(|v| v.as_str()),
        Some("owner/1000")
    );
    assert!(path().starts_with(harness.root()));
}

#[test]
fn repeats_of_one_class_are_suppressed_and_counted() {
    let _harness = Harness::new();
    reset();

    for _ in 0..50 {
        raise(Class::QuotaExhausted, "owner/1000", "at the ceiling");
    }
    assert_eq!(
        recent(100).len(),
        1,
        "an alarm loop must not become the outage it reports"
    );
}

#[test]
fn different_classes_are_not_suppressed_by_each_other() {
    let _harness = Harness::new();
    reset();

    raise(
        Class::AclViolation,
        "owner/1000",
        "worker asked for too much",
    );
    raise(Class::TornAppend, "owner/1000", "discarded 40 bytes");
    assert_eq!(recent(100).len(), 2);
}

#[test]
fn the_alarm_file_lives_outside_every_partition() {
    // A damaged partition must still be able to report itself.
    let harness = Harness::new();
    let partition_dir = harness.partition().dir(&harness.root());
    assert!(
        !path().starts_with(&partition_dir),
        "the alarm file must not depend on the partition it describes"
    );
}
