use super::*;
use crate::session::journal::event::{JournalEvent, Label, OperationId, RecoverySource};
use crate::session::journal::partition::Anchor;

fn anchor() -> Anchor {
    let partition = Partition::Owner(1000);
    Anchor::empty(&partition, 1000, "0011223344556677")
}

fn started() -> JournalEvent {
    JournalEvent::MutationStarted {
        operation: OperationId::generate(),
        route: Label::new("system.package.install"),
        idempotency: crate::audit_policy::text_digest("k"),
        grant: None,
        session_mutation: None,
    }
}

fn committed() -> JournalEvent {
    JournalEvent::MutationCommitted {
        operation: OperationId::generate(),
        duration_ms: 1,
    }
}

fn capability_use() -> JournalEvent {
    JournalEvent::CapabilityUsed {
        grant: crate::session::journal::event::Reference::new("g-1"),
        route: Label::new("system.service.control"),
        caps: 1,
        uses_remaining: Some(4),
    }
}

fn tool() -> JournalEvent {
    JournalEvent::ToolStarted {
        turn: 0,
        tool: Label::new("cos_todo"),
        tool_use_id: Label::new("t"),
        known: true,
    }
}

#[test]
fn the_event_kind_decides_the_class_before_the_writer_does() {
    // A capability-use record is broker-written but agent-*driven*, so
    // it is bounded control traffic, not privileged capacity.
    assert_eq!(
        QuotaClass::of(&capability_use(), EventSource::Kernel, false),
        QuotaClass::Control
    );
    assert_eq!(
        QuotaClass::of(&started(), EventSource::Kernel, false),
        QuotaClass::Control
    );
    // Only records that retire or recover a bracket are closure.
    assert_eq!(
        QuotaClass::of(&committed(), EventSource::Kernel, false),
        QuotaClass::Closure
    );
    assert_eq!(
        QuotaClass::of(
            &JournalEvent::MutationOrphaned {
                operation: OperationId::generate(),
                route: Label::new("r"),
                detected_by: RecoverySource::DaemonStart,
                opened_in_epoch: 1,
            },
            EventSource::Recovery,
            false
        ),
        QuotaClass::Closure
    );
    // A closure record stays closure even if the caller asked for the
    // ingest budget, and a worker record stays worker traffic.
    assert_eq!(
        QuotaClass::of(&committed(), EventSource::Kernel, true),
        QuotaClass::Closure
    );
    assert_eq!(
        QuotaClass::of(&tool(), EventSource::Worker, true),
        QuotaClass::Worker
    );
    assert_eq!(
        QuotaClass::of(&tool(), EventSource::Kernel, true),
        QuotaClass::ContextIngest
    );
}

#[test]
fn only_closure_may_use_the_reserve() {
    assert!(QuotaClass::Closure.may_use_reserve());
    for class in [
        QuotaClass::Control,
        QuotaClass::Worker,
        QuotaClass::ContextIngest,
    ] {
        assert!(
            !class.may_use_reserve(),
            "{} must not be able to take the space that closes a mutation",
            class.as_str()
        );
    }
}

#[test]
fn the_reserve_grows_with_the_number_of_open_brackets() {
    let mut anchor = anchor();
    let empty = reserved_records(&anchor, 0);
    anchor.open_brackets = 10;
    let ten = reserved_records(&anchor, 0);
    assert_eq!(ten - empty, 10 * CLOSURE_RECORDS_PER_BRACKET);
    assert_eq!(
        reserved_records(&anchor, 1) - ten,
        CLOSURE_RECORDS_PER_BRACKET,
        "a start must reserve for its own closure"
    );
}

#[test]
fn control_traffic_cannot_take_the_space_an_open_bracket_needs() {
    let mut anchor = anchor();
    anchor.open_brackets = 4;
    // Exactly enough room for the outstanding brackets and nothing else.
    anchor.events = MAX_EVENTS_PER_PARTITION - reserved_records(&anchor, 0);

    let error = check(&anchor, QuotaClass::Control, false, 256)
        .expect_err("control traffic must not consume the reserve");
    assert!(
        error.to_string().contains("outstanding mutation"),
        "{error}"
    );
    check(&anchor, QuotaClass::Closure, false, 256).expect("closure records still fit");
}

#[test]
fn a_start_is_refused_when_its_own_closure_would_not_fit() {
    let mut anchor = anchor();
    anchor.open_brackets = 1;
    anchor.events =
        MAX_EVENTS_PER_PARTITION - reserved_records(&anchor, 1) - 1 + CLOSURE_RECORDS_PER_BRACKET;
    assert!(
        check(&anchor, QuotaClass::Control, true, 256).is_err(),
        "a bracket that cannot be closed must never be opened"
    );
}

#[test]
fn agent_driven_kernel_records_cannot_exhaust_the_closure_reserve() {
    // The blocker this replaces: capability use, approval mediation and
    // prompt snapshots are broker-written, so classifying by writer let
    // them fill the partition and strand open brackets.
    let mut anchor = anchor();
    anchor.open_brackets = 32;
    anchor.control_events = MAX_CONTROL_EVENTS;
    anchor.events = MAX_CONTROL_EVENTS;

    assert!(check(&anchor, QuotaClass::Control, false, 256).is_err());
    for _ in 0..(32 * CLOSURE_RECORDS_PER_BRACKET) {
        check(&anchor, QuotaClass::Closure, false, 256).expect("every close must fit");
    }
}

#[test]
fn no_combination_of_non_closure_classes_can_fill_a_partition() {
    let mut anchor = anchor();
    anchor.control_events = MAX_CONTROL_EVENTS;
    anchor.worker_events = MAX_WORKER_EVENTS;
    anchor.ingest_events = MAX_INGEST_EVENTS;
    anchor.events = MAX_CONTROL_EVENTS + MAX_WORKER_EVENTS + MAX_INGEST_EVENTS;

    assert!(anchor.events < MAX_EVENTS_PER_PARTITION);
    assert!(
        MAX_EVENTS_PER_PARTITION - anchor.events > RECOVERY_HEADROOM,
        "the untouchable remainder must still cover a recovery pass"
    );
    for class in [
        QuotaClass::Control,
        QuotaClass::Worker,
        QuotaClass::ContextIngest,
    ] {
        assert!(check(&anchor, class, false, 256).is_err());
    }
    check(&anchor, QuotaClass::Closure, false, 256).expect("closure still fits");
}

#[test]
fn context_ingest_cannot_exhaust_worker_or_control_capacity() {
    let mut anchor = anchor();
    anchor.ingest_events = MAX_INGEST_EVENTS;
    anchor.events = MAX_INGEST_EVENTS;
    assert!(check(&anchor, QuotaClass::ContextIngest, false, 256).is_err());
    check(&anchor, QuotaClass::Control, false, 256).expect("control capacity is untouched");
    check(&anchor, QuotaClass::Worker, false, 256).expect("worker capacity is untouched");
}

#[test]
fn the_byte_reserve_follows_the_same_rule() {
    let mut anchor = anchor();
    anchor.open_brackets = 2;
    anchor.total_bytes = MAX_PARTITION_BYTES - reserved_bytes(&anchor, 0);
    assert!(check(&anchor, QuotaClass::Worker, false, 1).is_err());
    check(&anchor, QuotaClass::Closure, false, 1).expect("closure bytes are reserved");
}

#[test]
fn rate_limits_bound_every_driven_class_and_exempt_closure() {
    reset_rate_limits();
    let partition = Partition::Owner(999_001);
    for class in [
        QuotaClass::Worker,
        QuotaClass::Control,
        QuotaClass::ContextIngest,
    ] {
        let mut refused = false;
        for _ in 0..5_000 {
            if admit_rate(&partition, class).is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused, "{} must be rate limited", class.as_str());
    }
    for _ in 0..10_000 {
        admit_rate(&partition, QuotaClass::Closure).expect("closure is never delayed");
    }
    reset_rate_limits();
}
