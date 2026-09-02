use super::*;
use crate::session::journal::acl::EventSource;
use crate::session::journal::event::{
    ContentRef, ContentStore, JournalEvent, Origin, RecoverySource, Resolution, Trust,
};
use crate::session::journal::harness::{probe, Harness};
use crate::session::journal::{begin_mutation, MutationStart};

#[test]
fn the_mutation_timeline_is_rebuilt_from_the_chain() {
    let harness = Harness::new();
    let committed = begin_mutation(MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.service.control",
        request_key: "req-1",
        grant: Some("g-abc"),
        session_mutation: Some(7),
        context_ingest: false,
    })
    .expect("bracket");
    let operation = committed.operation().as_str().to_string();
    committed.commit().expect("commit");

    let failed = begin_mutation(MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.package.install",
        request_key: "req-2",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket");
    failed
        .fail("execution_failed", "dpkg said no")
        .expect("fail");

    let projection = build(&harness.partition(), harness.owner_uid()).expect("projection");
    assert!(projection.health.is_verified());
    assert_eq!(projection.mutations.len(), 2);

    let first = &projection.mutations[0];
    assert_eq!(first.operation, operation);
    assert_eq!(first.status, "committed");
    assert_eq!(first.grant.as_deref(), Some("g-abc"));
    assert_eq!(first.session_mutation, Some(7));
    assert!(first.closed_seq.is_some());

    let second = &projection.mutations[1];
    assert_eq!(second.status, "failed");
    assert_eq!(second.failure_class.as_deref(), Some("execution_failed"));
}

#[test]
fn rebuilding_twice_produces_the_same_answer() {
    let harness = Harness::new();
    harness.append(probe(1));
    let bracket = begin_mutation(MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.network.control",
        request_key: "req-3",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket");
    bracket.commit().expect("commit");

    let first = build(&harness.partition(), harness.owner_uid()).unwrap();
    let second = build(&harness.partition(), harness.owner_uid()).unwrap();
    assert_eq!(
        first, second,
        "a projection has no state of its own, so it cannot drift"
    );
}

#[test]
fn the_lifecycle_view_carries_references_not_content() {
    let harness = Harness::new();
    super::super::record(
        &harness.partition(),
        harness.owner_uid(),
        EventSource::Kernel,
        JournalEvent::UserRequestRecorded {
            turn: 0,
            content: ContentRef::of(ContentStore::SessionTurns, b"book me a flight"),
            origin: Origin::User,
            trust: Trust::Trusted,
        },
    )
    .expect("append");
    super::super::record(
        &harness.partition(),
        harness.owner_uid(),
        EventSource::Kernel,
        JournalEvent::PromptSegmentInjected {
            turn: 0,
            segment: ContentRef::of(ContentStore::SessionTurns, b"tool said something"),
            segment_kind: super::super::SegmentKind::ToolResult,
            origin: Origin::Tool,
            trust: Trust::Untrusted,
        },
    )
    .expect("append");

    let projection = build(&harness.partition(), harness.owner_uid()).unwrap();
    let digests: Vec<&str> = projection
        .lifecycle
        .iter()
        .filter_map(|entry| entry.content_digest.as_deref())
        .collect();
    assert_eq!(digests.len(), 2);
    for digest in digests {
        assert_eq!(digest.len(), 64);
    }
    assert!(projection
        .lifecycle
        .iter()
        .any(|entry| entry.trust == Some(Trust::Untrusted)));
}

#[test]
fn the_system_operations_view_projects_the_same_chain() {
    let harness = Harness::new();
    let bracket = begin_mutation(MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.firewall.control",
        request_key: "req-4",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket");
    bracket.commit().expect("commit");

    let value = system_operations(&harness.partition(), harness.owner_uid()).unwrap();
    assert_eq!(
        value.get("source").and_then(|v| v.as_str()),
        Some("session.journal")
    );
    let operations = value.get("operations").and_then(|v| v.as_array()).unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(
        operations[0].get("status").and_then(|v| v.as_str()),
        Some("committed")
    );
}

#[test]
fn a_damaged_chain_still_projects_diagnostics() {
    let harness = Harness::new();
    harness.append(probe(1));
    harness.append(probe(2));
    let lines = harness.lines();
    std::fs::write(harness.active_path(), format!("{}\n", lines[0])).unwrap();

    let projection = build(&harness.partition(), harness.owner_uid()).expect("projection");
    assert!(
        projection.health.is_damaged(),
        "damage must be visible, not fatal to a read"
    );
}

#[test]
fn an_operator_resolution_shows_up_as_its_declared_outcome() {
    let harness = Harness::new();
    let bracket = begin_mutation(MutationStart {
        partition: harness.partition(),
        owner_uid: harness.owner_uid(),
        route: "system.package.install",
        request_key: "req-resolve",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket");
    let operation = bracket.operation().as_str().to_string();
    std::mem::forget(bracket);

    harness.cold_restart();
    super::super::startup_recovery(RecoverySource::DaemonStart).expect("recovery");
    super::super::resolve_mutation(
        &harness.partition(),
        harness.owner_uid(),
        &operation,
        Resolution::RolledBack,
        0,
    )
    .expect("resolve");

    let projection = build(&harness.partition(), harness.owner_uid()).unwrap();
    let entry = projection
        .mutations
        .iter()
        .find(|entry| entry.operation == operation)
        .expect("the operation is in the timeline");
    assert_eq!(entry.status, "resolved-rolled-back");
}
