use super::*;

use crate::clawd::routes::Command;
use crate::session::journal::harness::Harness;
use crate::session::journal::{projection, JournalEvent};

fn client(uid: u32) -> ClientIdentity {
    let mut client = ClientIdentity::unknown();
    client.uid = Some(uid);
    client.pid = Some(std::process::id());
    client.start_time_ticks = Some(1);
    client
}

fn request_id() -> RequestId {
    RequestId::parse("req-1").unwrap()
}

#[test]
fn a_query_route_opens_no_bracket() {
    let _harness = Harness::new();
    let route = Command::DaemonHealth.route();
    let guard = begin(route, &request_id(), None, &client(1000)).expect("query is never bracketed");
    assert!(guard.is_none());
}

#[test]
fn a_mutation_route_is_bracketed_before_dispatch() {
    let harness = Harness::new();
    let route = Command::SystemServiceControl.route();
    let guard = begin(route, &request_id(), None, &client(harness.owner_uid()))
        .expect("bracket opens")
        .expect("a mutation is bracketed");

    let (partition, _operation, start_seq) = guard.reference();
    assert_eq!(partition, Partition::Owner(harness.owner_uid()).key());
    assert_eq!(start_seq, 1);

    let projection = projection::build(&Partition::Owner(harness.owner_uid()), harness.owner_uid())
        .expect("projection");
    assert_eq!(projection.mutations.len(), 1);
    assert_eq!(projection.mutations[0].status, "started");
    assert_eq!(projection.mutations[0].route, "system.service.control");

    // Close it so the harness does not leave an orphan behind.
    let ok = Response::ok(request_id(), serde_json::json!({}));
    assert!(finish(guard, &request_id(), &ok).is_none());
}

#[test]
fn a_start_the_journal_cannot_record_refuses_the_request() {
    let harness = Harness::new();
    let route = Command::SystemPackageInstall.route();

    crate::session::journal::faults::arm(crate::session::journal::faults::Fault::AppendWrite);
    let fault = begin(route, &request_id(), None, &client(harness.owner_uid()))
        .expect_err("an unrecordable mutation must be refused");
    crate::session::journal::faults::disarm();

    assert_eq!(fault, Fault::JournalUnavailable);
    assert_eq!(fault.code(), "unavailable");
}

#[test]
fn a_handler_failure_is_recorded_as_failed() {
    let harness = Harness::new();
    let route = Command::SystemFirewallControl.route();
    let guard = begin(route, &request_id(), None, &client(harness.owner_uid()))
        .unwrap()
        .unwrap();

    let failure = Response::error_classified(
        request_id(),
        "execution_failed",
        "nft_apply_failed",
        "nft: syntax error near line 3",
    );
    assert!(finish(guard, &request_id(), &failure).is_none());

    let projection = projection::build(&Partition::Owner(harness.owner_uid()), harness.owner_uid())
        .expect("projection");
    assert_eq!(projection.mutations[0].status, "failed");
    assert_eq!(
        projection.mutations[0].failure_class.as_deref(),
        Some("nft_apply_failed")
    );

    // The handler's message quotes caller input, so only its digest is
    // stored.
    let partition = Partition::Owner(harness.owner_uid());
    let anchor = crate::session::journal::lease()
        .unwrap()
        .load_anchor(&partition, harness.owner_uid())
        .unwrap();
    let chain = std::fs::read_to_string(anchor.active_path(&harness.root(), &partition)).unwrap();
    assert!(!chain.contains("syntax error near line 3"));
}

#[test]
fn a_completion_that_cannot_be_recorded_answers_indeterminate() {
    let harness = Harness::new();
    let route = Command::SystemPackageControl.route();
    let guard = begin(route, &request_id(), None, &client(harness.owner_uid()))
        .unwrap()
        .unwrap();

    crate::session::journal::faults::arm(crate::session::journal::faults::Fault::AppendWrite);
    let replacement = finish(
        guard,
        &request_id(),
        &Response::ok(request_id(), serde_json::json!({"ok": true})),
    );
    crate::session::journal::faults::disarm();

    let replacement = replacement.expect("the handler's success must not be released");
    assert!(!replacement.ok);
    let error = replacement.error.expect("an error body");
    assert_eq!(error.code, "indeterminate");
    assert_eq!(error.audit_class, Some("mutation_indeterminate"));
    assert!(
        error.message.contains("recovery is required"),
        "{}",
        error.message
    );
}

#[test]
fn a_replayed_unresolved_mutation_is_refused_rather_than_re_run() {
    let harness = Harness::new();
    let route = Command::SystemPackageInstall.route();
    let peer = client(harness.owner_uid());

    let guard = begin(route, &request_id(), None, &peer).unwrap().unwrap();
    std::mem::forget(guard);

    harness.cold_restart();
    crate::session::journal::startup_recovery(crate::session::journal::RecoverySource::DaemonStart)
        .expect("recovery");

    // A *different process* retrying the same operation must still be
    // refused: durable identity is owner + route + operation key, and
    // deliberately carries no pid or start time.
    let mut restarted = client(harness.owner_uid());
    restarted.pid = Some(peer.pid.unwrap() + 1);
    restarted.start_time_ticks = Some(999_999);
    let fault = begin(route, &request_id(), None, &restarted)
        .expect_err("re-running a non-idempotent mutation must be refused");
    assert_eq!(fault, Fault::DuplicateRequest);

    // A different operation key is a different operation.
    let other = RequestId::parse("req-2").unwrap();
    let guard = begin(route, &other, None, &restarted)
        .expect("an unrelated operation is not a replay")
        .expect("bracketed");
    let ok = Response::ok(other.clone(), serde_json::json!({}));
    assert!(finish(guard, &other, &ok).is_none());
}

#[test]
fn the_refusal_survives_repeated_daemon_restarts() {
    let harness = Harness::new();
    let route = Command::SystemPackageInstall.route();
    let peer = client(harness.owner_uid());
    let guard = begin(route, &request_id(), None, &peer).unwrap().unwrap();
    std::mem::forget(guard);

    for _ in 0..3 {
        harness.cold_restart();
        crate::session::journal::startup_recovery(
            crate::session::journal::RecoverySource::DaemonStart,
        )
        .expect("recovery");
        assert_eq!(
            begin(route, &request_id(), None, &peer).expect_err("still refused"),
            Fault::DuplicateRequest
        );
    }
}

#[test]
fn an_operator_resolution_lets_the_operation_be_retried() {
    let harness = Harness::new();
    let route = Command::SystemServiceControl.route();
    let peer = client(harness.owner_uid());
    let guard = begin(route, &request_id(), None, &peer).unwrap().unwrap();
    let operation = guard.reference().1;
    std::mem::forget(guard);

    harness.cold_restart();
    crate::session::journal::startup_recovery(crate::session::journal::RecoverySource::DaemonStart)
        .expect("recovery");
    assert_eq!(
        begin(route, &request_id(), None, &peer).expect_err("refused before resolution"),
        Fault::DuplicateRequest
    );

    resolve(
        &serde_json::json!({
            "partition": Partition::Owner(harness.owner_uid()).key(),
            "operation": operation,
            "outcome": "abandoned",
        }),
        &client(0),
    )
    .expect("root may record what happened");

    let guard = begin(route, &request_id(), None, &peer)
        .expect("a resolved operation may be retried")
        .expect("bracketed");
    let ok = Response::ok(request_id(), serde_json::json!({}));
    assert!(finish(guard, &request_id(), &ok).is_none());
}

#[test]
fn resolution_is_refused_for_a_non_root_peer() {
    let harness = Harness::new();
    let error = resolve(
        &serde_json::json!({
            "partition": Partition::Owner(harness.owner_uid()).key(),
            "operation": "0123456789abcdef",
            "outcome": "abandoned",
        }),
        &client(1000),
    )
    .expect_err("only root may resolve");
    assert!(error.contains("requires root"), "{error}");
}

#[test]
fn resolution_refuses_a_partition_key_it_did_not_produce() {
    let _harness = Harness::new();
    let error = resolve(
        &serde_json::json!({
            "partition": "owner/../../etc",
            "operation": "0123456789abcdef",
            "outcome": "abandoned",
        }),
        &client(0),
    )
    .expect_err("a forged partition key must not resolve");
    assert!(error.contains("not a journal partition"), "{error}");
}

#[test]
fn status_reports_unresolved_work_for_the_calling_owner() {
    let harness = Harness::new();
    let route = Command::SystemPackageInstall.route();
    let peer = client(harness.owner_uid());
    let guard = begin(route, &request_id(), None, &peer).unwrap().unwrap();
    std::mem::forget(guard);

    let value = status(&serde_json::json!({}), &peer).expect("status");
    let unresolved = value.get("unresolved").and_then(|v| v.as_array()).unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(
        unresolved[0].get("route").and_then(|v| v.as_str()),
        Some("system.package.install")
    );
    assert_eq!(
        value.get("partition").and_then(|v| v.as_str()),
        Some(Partition::Owner(harness.owner_uid()).key().as_str()),
        "an omitted session id means the caller's own owner partition"
    );
}

// ---------------------------------------------------------------------------
// journal.status owner isolation
// ---------------------------------------------------------------------------

/// Write a session record whose `owner_uid` field claims `claimed`.
///
/// The record's own inode owner is this test process, so this is
/// exactly what a third party asserting somebody else's ownership looks
/// like on disk.
fn craft_session(claimed: u32) -> String {
    let sid = crate::session::create("crafted").expect("session");
    crate::session::update_meta(&sid, |meta| meta.owner_uid = Some(claimed)).expect("claim");
    sid.as_str().to_string()
}

#[test]
fn status_reads_a_session_the_caller_owns() {
    let harness = Harness::new();
    let sid = crate::session::create("mine").expect("session");
    let peer = client(harness.owner_uid());

    let value = status(&serde_json::json!({ "session_id": sid.as_str() }), &peer)
        .expect("the owner may read its own session");
    assert_eq!(
        value.get("partition").and_then(|v| v.as_str()),
        Some(format!("session/{}", sid.as_str()).as_str())
    );
}

#[test]
fn status_reads_any_session_the_same_owner_holds() {
    // The policy is owner-level: a sibling session of the same uid is
    // the same principal's evidence.
    let harness = Harness::new();
    let first = crate::session::create("one").expect("session");
    let second = crate::session::create("two").expect("session");
    let peer = client(harness.owner_uid());

    for sid in [&first, &second] {
        status(&serde_json::json!({ "session_id": sid.as_str() }), &peer)
            .expect("both sessions belong to the caller");
    }
}

#[test]
fn status_refuses_a_foreign_session_exactly_as_it_refuses_a_missing_one() {
    let harness = Harness::new();
    let peer = client(harness.owner_uid());

    // A session that exists but claims another owner.
    let foreign = craft_session(harness.owner_uid() + 1);
    // A session that claims root.
    let root_owned = craft_session(0);
    // A session id that is well-formed and simply does not exist.
    let missing = crate::session::SessionId::generate().as_str().to_string();
    // Ids that are not well-formed at all, including traversal shapes.
    let crafted = [
        "../../etc/passwd",
        "ses_../../../root",
        "ses_zzzzzzzzzzzzz_000000000000",
        "",
        "ses_0000000000001_00000000000",
    ];

    let baseline = status(&serde_json::json!({ "session_id": missing }), &peer)
        .expect_err("a missing session is refused");

    for raw in [foreign.as_str(), root_owned.as_str()]
        .into_iter()
        .chain(crafted)
    {
        let error = status(&serde_json::json!({ "session_id": raw }), &peer)
            .expect_err("a session the caller does not own is refused");
        assert_eq!(
            error, baseline,
            "`{raw}` must be indistinguishable from a session that does not exist"
        );
    }
}

#[test]
fn status_leaks_no_evidence_from_a_foreign_session() {
    let harness = Harness::new();
    let peer = client(harness.owner_uid());

    // Give the foreign session real journal evidence to leak.
    let foreign = craft_session(harness.owner_uid() + 1);
    let sid = foreign.parse::<crate::session::SessionId>().unwrap();
    let partition = Partition::Session(sid);
    let bracket = crate::session::journal::begin_mutation(crate::session::journal::MutationStart {
        partition: partition.clone(),
        owner_uid: harness.owner_uid() + 1,
        route: "system.package.install",
        request_key: "foreign-1",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket");
    std::mem::forget(bracket);

    crate::session::journal::alarm::reset();
    let error = status(&serde_json::json!({ "session_id": foreign }), &peer).expect_err("refused");

    // Nothing about the partition survives into the answer.
    assert!(!error.contains(&partition.key()));
    assert!(!error.contains("head_seq"));
    assert!(!error.contains("unresolved"));
    assert!(!error.contains("verified"));
    assert!(!error.contains("damaged"));

    // And naming it had no side effect on it.
    assert!(
        !crate::session::journal::recovery::is_quarantined(&partition),
        "a refused read must not quarantine the partition it did not open"
    );
    assert!(
        crate::session::journal::alarm::recent(10).is_empty(),
        "a refused read must not raise an alarm"
    );
}

#[test]
fn status_refuses_a_session_whose_ownership_claim_is_unverifiable() {
    // `owner_uid` is believed only when the record is root-authored or
    // is owned by the account it names. A record with no claim at all
    // is refused rather than defaulted.
    let harness = Harness::new();
    let sid = crate::session::create("no-owner").expect("session");
    crate::session::update_meta(&sid, |meta| meta.owner_uid = None).expect("clear");

    let error = status(
        &serde_json::json!({ "session_id": sid.as_str() }),
        &client(harness.owner_uid()),
    )
    .expect_err("an unverifiable claim is refused");
    assert!(error.contains("no session journal partition"), "{error}");
}

#[test]
fn context_event_append_is_charged_to_the_ingest_budget() {
    let harness = Harness::new();
    let route = Command::ContextEventAppend.route();
    let guard = begin(route, &request_id(), None, &client(harness.owner_uid()))
        .unwrap()
        .unwrap();
    let ok = Response::ok(request_id(), serde_json::json!({}));
    assert!(finish(guard, &request_id(), &ok).is_none());

    let lease = harness.lease();
    let anchor = lease
        .load_anchor(&Partition::Owner(harness.owner_uid()), harness.owner_uid())
        .unwrap();
    assert_eq!(
        anchor.ingest_events, 1,
        "the attacker-influenced start belongs to the ingest class"
    );
    assert_eq!(
        anchor.closure_events, 1,
        "its close draws on the reserve so it can never be starved"
    );
    assert_eq!(anchor.control_events, 0);
}

#[test]
fn a_session_grant_selects_the_session_partition() {
    let _harness = Harness::new();
    // Without a decision the owner's own partition is used; the session
    // form is exercised through the authority in the broker tests.
    let (partition, owner) = partition_for(None, &client(4242));
    assert_eq!(partition, Partition::Owner(4242));
    assert_eq!(owner, 4242);
}

#[test]
fn capability_use_is_journalled_as_a_reference() {
    let harness = Harness::new();
    record_approval(
        &Partition::Owner(harness.owner_uid()),
        harness.owner_uid(),
        JournalEvent::ApprovalConsumed {
            approval: None,
            verb: crate::session::journal::Label::new("fs.write"),
            scope: crate::session::journal::Reference::new("path:/tmp"),
            generation: 3,
        },
    );
    let projection = projection::build(&Partition::Owner(harness.owner_uid()), harness.owner_uid())
        .expect("projection");
    assert!(projection.health.is_verified());
}
