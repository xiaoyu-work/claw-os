use super::super::tests::{
    draft as activity_draft, receipt_declaration, receipt_report, schema_one_database,
    unverified_receipt, TestDirectory,
};
use super::super::{MIGRATE_TO_V2, SCHEMA};
use super::*;
use crate::activities::{
    ActivityPatch, ActivityReceipt, ActivityResource, ActivityService, ActivityState,
    ObjectRelationKind, ObjectStateValidity, ReceiptOutcome, ReceiptSource,
    DATABASE_SCHEMA_VERSION, DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT, SCHEMA_VERSION,
};
use chrono::SecondsFormat;
use std::path::Path;
use std::sync::{Arc, Barrier};

const SUBJECT: &str = "app://sample/record?id=release";
const TARGET: &str = "app://kv/entry?id=review";
const ALTERNATE: &str = "app://sample/record?id=alternate";

fn activity(service: &dyn ActivityService, owner: u32) -> Activity {
    let mut draft = activity_draft();
    draft.resources = [SUBJECT, TARGET, ALTERNATE]
        .into_iter()
        .map(|reference| ActivityResource {
            label: "Inert object reference".into(),
            reference: reference.into(),
        })
        .collect();
    service.create(owner, draft).unwrap()
}

fn annotation() -> ObjectStateDraft {
    ObjectStateDraft {
        id: uuid::Uuid::new_v4().to_string(),
        reference: SUBJECT.into(),
        content: ObjectStateContent::UserStatement {
            text: "Caller statement".into(),
        },
        observed_at: None,
        valid_until: None,
        supersedes: None,
    }
}

fn relation(target: &str) -> ObjectStateDraft {
    ObjectStateDraft {
        content: ObjectStateContent::Relation {
            relation: ObjectRelationKind::DependsOn,
            target: target.into(),
            note: "Planning only, not an execution dependency".into(),
        },
        ..annotation()
    }
}

fn app_report(id: &str) -> ObjectStateDraft {
    ObjectStateDraft {
        content: ObjectStateContent::AppReport {
            receipt_id: id.into(),
        },
        ..annotation()
    }
}

fn insert_raw(
    conn: &Connection,
    activity: &Activity,
    draft: &ObjectStateDraft,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO activity_object_state (
            id, activity_id, owner_uid, reference, recorded_at, source,
            draft_json, receipt_id, supersedes
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'caller_reported', ?6, ?7, ?8)",
        params![
            draft.id,
            activity.id,
            activity.owner_uid,
            draft.reference,
            timestamp(),
            serde_json::to_string(draft).unwrap(),
            receipt_id(draft),
            draft.supersedes,
        ],
    )
}

#[test]
fn object_metadata_persists_in_the_shared_activity_database_without_app_data_or_lifecycle_changes()
{
    let directory = TestDirectory::new();
    let (activity, expected, receipt) = {
        let provider = SqliteActivityService::open(directory.database()).unwrap();
        let service: &dyn ActivityService = &provider;
        let activity = activity(service, 7);
        let receipt = service
            .record_receipt(
                7,
                &activity.id,
                receipt_report(),
                Some(receipt_declaration()),
                None,
            )
            .unwrap();
        let statement = service
            .record_object_state(7, &activity.id, annotation())
            .unwrap();
        let reported = service
            .record_object_state(7, &activity.id, app_report(&receipt.id))
            .unwrap();
        let related = service
            .record_object_state(7, &activity.id, relation(TARGET))
            .unwrap();
        for entry in [&statement, &reported, &related] {
            assert_eq!(entry.id, entry.draft.id);
            assert_eq!(entry.activity_id, activity.id);
            assert_eq!(entry.owner_uid, 7);
            assert_eq!(entry.source, ObjectStateSource::CallerReported);
            assert_eq!(entry.validity, ObjectStateValidity::Unknown);
            parse_timestamp(&entry.recorded_at).unwrap();
            assert!(entry.superseded_by.is_none());
        }
        assert!(statement.receipt.is_none());
        assert!(related.receipt.is_none());
        assert_eq!(reported.receipt.as_ref(), Some(&receipt.report));
        let json = serde_json::to_value(&reported).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 9);
        assert_eq!(json["source"], "caller_reported");
        assert_eq!(
            json["receipt"],
            serde_json::to_value(&receipt.report).unwrap()
        );
        assert!(json["receipt"].get("declaration").is_none());
        assert!(json.get("confirmed").is_none());
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
        (activity, vec![related, reported, statement], receipt)
    };
    let reopened = SqliteActivityService::open(directory.database()).unwrap();
    assert_eq!(
        reopened.object_state(7, &activity.id, None, 0).unwrap(),
        expected
    );
    assert_eq!(
        reopened.receipts(7, &activity.id, 100).unwrap(),
        vec![receipt]
    );
    assert_eq!(reopened.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn object_state_owner_isolation_includes_root_and_per_owner_uuid_reuse() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let owners = [0, 7, 8, u32::MAX];
    let records: Vec<_> = owners
        .iter()
        .map(|owner| activity(&service, *owner))
        .collect();
    let draft = annotation();
    let missing = uuid::Uuid::new_v4().to_string();
    for activity in &records {
        let entry = service
            .record_object_state(
                activity.owner_uid,
                &activity.id.to_uppercase(),
                draft.clone(),
            )
            .unwrap();
        assert_eq!(entry.activity_id, activity.id);
        assert_eq!(entry.owner_uid, activity.owner_uid);
        assert_eq!(entry.id, draft.id);
        for owner in owners
            .into_iter()
            .filter(|owner| *owner != activity.owner_uid)
        {
            for id in [&activity.id, &missing] {
                assert!(matches!(
                    service.object_state(owner, id, None, 100),
                    Err(ActivityError::NotFound)
                ));
                assert!(matches!(
                    service.record_object_state(owner, id, draft.clone()),
                    Err(ActivityError::NotFound)
                ));
            }
        }
        assert_eq!(
            service
                .object_state(activity.owner_uid, &activity.id, None, 100)
                .unwrap(),
            vec![entry]
        );
    }
    let other = activity(&service, 7);
    assert!(matches!(
        service.record_object_state(7, &other.id, draft),
        Err(ActivityError::Conflict(_))
    ));
    assert!(service
        .object_state(7, &other.id, None, 100)
        .unwrap()
        .is_empty());
}

#[test]
fn subject_and_relation_target_must_both_be_attached_to_the_same_owned_activity() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let other = activity(&service, 7);
    let mut draft = activity_draft();
    draft.resources.push(ActivityResource {
        label: "Subject".into(),
        reference: SUBJECT.into(),
    });
    let activity = service.create(7, draft).unwrap();
    let mut unattached = annotation();
    unattached.reference = ALTERNATE.into();
    assert!(matches!(
        service.record_object_state(7, &activity.id, unattached),
        Err(ActivityError::Invalid(_))
    ));
    assert!(matches!(
        service.record_object_state(7, &activity.id, relation(TARGET)),
        Err(ActivityError::Invalid(_))
    ));
    assert!(service
        .object_state(7, &activity.id, None, 100)
        .unwrap()
        .is_empty());
    assert_eq!(service.get(7, &other.id).unwrap(), other);
    let attached = service
        .add_resource(
            7,
            &activity.id,
            ActivityResource {
                label: "Target".into(),
                reference: TARGET.into(),
            },
        )
        .unwrap();
    service
        .record_object_state(7, &activity.id, relation(TARGET))
        .unwrap();
    service
        .record_object_state(7, &activity.id, annotation())
        .unwrap();
    assert_eq!(service.get(7, &activity.id).unwrap(), attached);
    let mut ordinary = annotation();
    ordinary.reference = attached.resources[0].reference.clone();
    assert!(matches!(
        service.record_object_state(7, &activity.id, ordinary),
        Err(ActivityError::Invalid(_))
    ));
    for reference in [
        "app://sample/record?id=%72elease",
        "app://sample/record?revision=v1&id=release",
    ] {
        let mut invalid = annotation();
        invalid.reference = reference.into();
        assert!(matches!(
            service.record_object_state(7, &activity.id, invalid),
            Err(ActivityError::Invalid(_))
        ));
        assert!(matches!(
            service.object_state(7, &activity.id, Some(reference), 100),
            Err(ActivityError::Invalid(_))
        ));
    }
}

#[test]
fn linked_receipts_require_matching_owner_activity_and_app_but_do_not_attest_execution() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let owned = activity(&service, 7);
    let another = activity(&service, 7);
    let foreign = activity(&service, 8);
    let root = activity(&service, 0);
    let another_receipt = unverified_receipt(&service, 7, &another.id, receipt_report()).unwrap();
    let foreign_receipt = unverified_receipt(&service, 8, &foreign.id, receipt_report()).unwrap();
    let mut wrong_app = receipt_report();
    wrong_app.app_id = "kv".into();
    let wrong_app = unverified_receipt(&service, 7, &owned.id, wrong_app).unwrap();
    for id in [
        another_receipt.id,
        foreign_receipt.id,
        wrong_app.id,
        uuid::Uuid::new_v4().to_string(),
    ] {
        assert!(matches!(
            service.record_object_state(7, &owned.id, app_report(&id)),
            Err(ActivityError::Invalid(_))
        ));
    }
    assert!(service
        .object_state(7, &owned.id, None, 100)
        .unwrap()
        .is_empty());
    for outcome in [
        ReceiptOutcome::Returned,
        ReceiptOutcome::ReportedError,
        ReceiptOutcome::Indeterminate,
    ] {
        let mut report = receipt_report();
        report.outcome = outcome;
        if outcome != ReceiptOutcome::Returned {
            report.error = Some("Caller reports failure or uncertain execution".into());
        }
        if outcome == ReceiptOutcome::Indeterminate {
            report.result = None;
        }
        let receipt = unverified_receipt(&service, 7, &owned.id, report.clone()).unwrap();
        let input = app_report(
            &uuid::Uuid::parse_str(&receipt.id)
                .unwrap()
                .urn()
                .to_string(),
        );
        let entry = service.record_object_state(7, &owned.id, input).unwrap();
        assert_eq!(entry.receipt, Some(report));
        assert_eq!(
            entry.draft.content,
            ObjectStateContent::AppReport {
                receipt_id: receipt.id.clone()
            }
        );
        assert_eq!(entry.source, ObjectStateSource::CallerReported);
        assert!(matches!(
            service.record_object_state(0, &root.id, app_report(&receipt.id)),
            Err(ActivityError::Invalid(_))
        ));
    }
    assert_eq!(service.get(7, &owned.id).unwrap(), owned);
}

#[test]
fn canonical_retries_return_original_data_and_changed_drafts_conflict() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 7);
    let mut draft = annotation();
    draft.content = ObjectStateContent::UserStatement {
        text: "  Caller statement\n\t  ".into(),
    };
    draft.observed_at = Some("2000-01-01T01:00:00+01:00".into());
    draft.valid_until = Some("2000-01-02T01:00:00+01:00".into());
    let original = service
        .record_object_state(7, &activity.id, draft.clone())
        .unwrap();
    let id = uuid::Uuid::parse_str(&original.id).unwrap();
    assert_eq!(
        original.draft.content,
        ObjectStateContent::UserStatement {
            text: "Caller statement".into()
        }
    );
    assert_eq!(
        original.draft.observed_at.as_deref(),
        Some("2000-01-01T00:00:00.000000000Z")
    );
    for spelling in [
        id.to_string().to_uppercase(),
        id.simple().to_string(),
        id.urn().to_string(),
    ] {
        let mut retry = draft.clone();
        retry.id = spelling;
        retry.observed_at = Some("1999-12-31T16:00:00-08:00".into());
        retry.valid_until = Some("2000-01-01T16:00:00-08:00".into());
        assert_eq!(
            service.record_object_state(7, &activity.id, retry).unwrap(),
            original
        );
    }
    for changed in [
        ObjectStateDraft {
            reference: ALTERNATE.into(),
            ..draft.clone()
        },
        ObjectStateDraft {
            content: ObjectStateContent::AgentInference {
                text: "Caller statement".into(),
            },
            ..draft.clone()
        },
        ObjectStateDraft {
            valid_until: Some("2001-01-01T00:00:00Z".into()),
            ..draft.clone()
        },
        ObjectStateDraft {
            supersedes: Some(uuid::Uuid::new_v4().to_string()),
            ..draft
        },
    ] {
        assert!(matches!(
            service.record_object_state(7, &activity.id, changed),
            Err(ActivityError::Conflict(_))
        ));
    }
    assert_eq!(
        service.object_state(7, &activity.id, None, 100).unwrap(),
        vec![original]
    );
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn corrections_and_retractions_append_history_without_reclassifying_earlier_entries() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 7);
    let first = service
        .record_object_state(7, &activity.id, annotation())
        .unwrap();
    let second = service
        .record_object_state(
            7,
            &activity.id,
            ObjectStateDraft {
                content: ObjectStateContent::AgentInference {
                    text: "An inference, not verified authorship".into(),
                },
                supersedes: Some(first.id.to_uppercase()),
                ..annotation()
            },
        )
        .unwrap();
    let related = service
        .record_object_state(
            7,
            &activity.id,
            ObjectStateDraft {
                supersedes: Some(second.id.clone()),
                ..relation(TARGET)
            },
        )
        .unwrap();
    let changed_relation = service
        .record_object_state(
            7,
            &activity.id,
            ObjectStateDraft {
                content: ObjectStateContent::Relation {
                    relation: ObjectRelationKind::DerivedFrom,
                    target: ALTERNATE.into(),
                    note: "A revised planning link".into(),
                },
                supersedes: Some(related.id.clone()),
                ..annotation()
            },
        )
        .unwrap();
    let retraction = service
        .record_object_state(
            7,
            &activity.id,
            ObjectStateDraft {
                content: ObjectStateContent::Retracted {
                    reason: "The caller withdraws this link".into(),
                },
                supersedes: Some(changed_relation.id.clone()),
                ..annotation()
            },
        )
        .unwrap();
    let mut expected = vec![
        first.clone(),
        second,
        related,
        changed_relation,
        retraction.clone(),
    ];
    for index in 0..expected.len() - 1 {
        expected[index].superseded_by = Some(expected[index + 1].id.clone());
    }
    assert_eq!(
        service
            .record_object_state(7, &activity.id, first.draft.clone())
            .unwrap(),
        expected[0]
    );
    let history = service
        .object_state(7, &activity.id, Some(SUBJECT), 100)
        .unwrap();
    expected.reverse();
    assert_eq!(history, expected);
    assert_eq!(history.last().unwrap().draft.content, first.draft.content);
    for prior in [&first, &retraction] {
        assert!(matches!(
            service.record_object_state(
                7,
                &activity.id,
                ObjectStateDraft {
                    supersedes: Some(prior.id.clone()),
                    ..annotation()
                }
            ),
            Err(ActivityError::Conflict(_))
        ));
    }
    let fresh = service
        .record_object_state(7, &activity.id, annotation())
        .unwrap();
    assert!(fresh.draft.supersedes.is_none());
    assert!(fresh.superseded_by.is_none());
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn correction_edges_require_existing_same_owner_activity_and_exact_subject() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let owned = activity(&service, 7);
    let another = activity(&service, 7);
    let foreign = activity(&service, 8);
    let original = service
        .record_object_state(7, &owned.id, annotation())
        .unwrap();
    let other_entry = service
        .record_object_state(7, &another.id, annotation())
        .unwrap();
    let foreign_entry = service
        .record_object_state(8, &foreign.id, annotation())
        .unwrap();
    for previous in [
        other_entry.id,
        foreign_entry.id,
        uuid::Uuid::new_v4().to_string(),
    ] {
        assert!(matches!(
            service.record_object_state(
                7,
                &owned.id,
                ObjectStateDraft {
                    supersedes: Some(previous),
                    ..annotation()
                }
            ),
            Err(ActivityError::Invalid(_))
        ));
    }
    assert!(matches!(
        service.record_object_state(
            7,
            &owned.id,
            ObjectStateDraft {
                reference: ALTERNATE.into(),
                supersedes: Some(original.id.clone()),
                ..annotation()
            }
        ),
        Err(ActivityError::Invalid(_))
    ));
    assert!(matches!(
        service.record_object_state(
            7,
            &owned.id,
            ObjectStateDraft {
                supersedes: Some(original.id.to_uppercase()),
                ..original.draft.clone()
            }
        ),
        Err(ActivityError::Invalid(_))
    ));
    let conn = service.lock().unwrap();
    let invalid = ObjectStateDraft {
        supersedes: Some(uuid::Uuid::new_v4().to_string()),
        ..annotation()
    };
    assert!(insert_raw(&conn, &owned, &invalid).is_err());
    let invalid = ObjectStateDraft {
        reference: ALTERNATE.into(),
        supersedes: Some(original.id),
        ..annotation()
    };
    assert!(insert_raw(&conn, &owned, &invalid).is_err());
}

#[test]
fn concurrent_stale_corrections_have_exactly_one_winner_and_a_unique_database_edge() {
    let directory = TestDirectory::new();
    let first = SqliteActivityService::open(directory.database()).unwrap();
    let second = SqliteActivityService::open(directory.database()).unwrap();
    let activity = activity(&first, 7);
    let original = first
        .record_object_state(7, &activity.id, annotation())
        .unwrap();
    let one = ObjectStateDraft {
        supersedes: Some(original.id.clone()),
        ..annotation()
    };
    let two = ObjectStateDraft {
        supersedes: Some(original.id.clone()),
        ..annotation()
    };
    let barrier = Arc::new(Barrier::new(2));
    let other_barrier = Arc::clone(&barrier);
    let id = activity.id.clone();
    let worker = std::thread::spawn(move || {
        other_barrier.wait();
        second.record_object_state(7, &id, two)
    });
    barrier.wait();
    let results = [
        first.record_object_state(7, &activity.id, one),
        worker.join().unwrap(),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(ActivityError::Conflict(_))))
            .count(),
        1
    );
    let history = first.object_state(7, &activity.id, None, 100).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[1].superseded_by.as_deref(),
        Some(history[0].id.as_str())
    );
    let duplicate_edge = ObjectStateDraft {
        supersedes: Some(original.id),
        ..annotation()
    };
    assert!(insert_raw(&first.lock().unwrap(), &activity, &duplicate_edge).is_err());
    assert_eq!(first.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn concurrent_exact_retries_share_the_original_immutable_entry() {
    let directory = TestDirectory::new();
    let first = SqliteActivityService::open(directory.database()).unwrap();
    let second = SqliteActivityService::open(directory.database()).unwrap();
    let activity = activity(&first, 7);
    let input = annotation();
    let other_input = input.clone();
    let id = activity.id.clone();
    let barrier = Arc::new(Barrier::new(2));
    let other_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        other_barrier.wait();
        second.record_object_state(7, &id, other_input).unwrap()
    });
    barrier.wait();
    let original = first.record_object_state(7, &activity.id, input).unwrap();
    assert_eq!(worker.join().unwrap(), original);
    assert_eq!(
        first.object_state(7, &activity.id, None, 100).unwrap(),
        vec![original]
    );
}

#[test]
fn all_history_counts_toward_the_activity_quota_but_exact_retries_still_work() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let owned = activity(&service, 7);
    let first = service
        .record_object_state(7, &owned.id, annotation())
        .unwrap();
    for _ in 1..MAX_ENTRIES_PER_ACTIVITY - 1 {
        service
            .record_object_state(7, &owned.id, annotation())
            .unwrap();
    }
    let last = service
        .record_object_state(
            7,
            &owned.id,
            ObjectStateDraft {
                content: ObjectStateContent::Retracted {
                    reason: "Withdrawn".into(),
                },
                supersedes: Some(first.id.clone()),
                ..annotation()
            },
        )
        .unwrap();
    assert!(matches!(
        service.record_object_state(7, &owned.id, annotation()),
        Err(ActivityError::LimitReached)
    ));
    let mut expected = first.clone();
    expected.superseded_by = Some(last.id.clone());
    assert_eq!(
        service
            .record_object_state(7, &owned.id, first.draft)
            .unwrap(),
        expected
    );
    assert_eq!(
        service
            .record_object_state(7, &owned.id, last.draft.clone())
            .unwrap(),
        last
    );
    let mut conflict = last.draft.clone();
    conflict.content = ObjectStateContent::Retracted {
        reason: "Changed".into(),
    };
    assert!(matches!(
        service.record_object_state(7, &owned.id, conflict),
        Err(ActivityError::Conflict(_))
    ));
    assert_eq!(
        service.object_state(7, &owned.id, None, 0).unwrap().len(),
        DEFAULT_LIST_LIMIT
    );
    assert_eq!(
        service
            .object_state(7, &owned.id, None, usize::MAX)
            .unwrap()
            .len(),
        MAX_LIST_LIMIT
    );
    assert_eq!(
        service
            .object_state(7, &owned.id, Some(SUBJECT), 1)
            .unwrap(),
        vec![last]
    );
    let another = activity(&service, 7);
    service
        .record_object_state(7, &another.id, annotation())
        .unwrap();
    assert_eq!(service.get(7, &owned.id).unwrap(), owned);
}

#[test]
fn lists_filter_exact_references_and_use_sequence_instead_of_caller_or_server_times() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 7);
    let old = service
        .record_object_state(7, &activity.id, annotation())
        .unwrap();
    let newer = service
        .record_object_state(
            7,
            &activity.id,
            ObjectStateDraft {
                reference: ALTERNATE.into(),
                ..annotation()
            },
        )
        .unwrap();
    service.lock().unwrap().execute(
        "UPDATE activity_object_state SET recorded_at = '2099-01-01T00:00:00.000000000Z' WHERE id = ?1",
        [&old.id],
    ).unwrap();
    service.lock().unwrap().execute(
        "UPDATE activity_object_state SET recorded_at = '2000-01-01T00:00:00.000000000Z' WHERE id = ?1",
        [&newer.id],
    ).unwrap();
    let history = service.object_state(7, &activity.id, None, 100).unwrap();
    assert_eq!(
        history
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec![newer.id.as_str(), old.id.as_str()]
    );
    assert_eq!(
        service
            .object_state(7, &activity.id, Some(SUBJECT), 1)
            .unwrap()[0]
            .id,
        old.id
    );
    assert_eq!(
        service
            .object_state(7, &activity.id, Some(ALTERNATE), 1)
            .unwrap()[0]
            .id,
        newer.id
    );
    assert!(service
        .object_state(
            7,
            &activity.id,
            Some("app://sample/record?id=never-attached"),
            1
        )
        .unwrap()
        .is_empty());
}

#[test]
fn validity_is_derived_again_on_read_and_retry_without_persisting_truth_or_clock_state() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 7);
    let now = Utc::now();
    for (start, end, expected) in [
        (
            now - chrono::Duration::days(2),
            now - chrono::Duration::days(1),
            ObjectStateValidity::Expired,
        ),
        (
            now - chrono::Duration::days(1),
            now + chrono::Duration::days(1),
            ObjectStateValidity::WithinReportedWindow,
        ),
        (
            now + chrono::Duration::days(1),
            now + chrono::Duration::days(2),
            ObjectStateValidity::NotYetApplicable,
        ),
    ] {
        let input = ObjectStateDraft {
            observed_at: Some(start.to_rfc3339_opts(SecondsFormat::Nanos, true)),
            valid_until: Some(end.to_rfc3339_opts(SecondsFormat::Nanos, true)),
            ..annotation()
        };
        let entry = service
            .record_object_state(7, &activity.id, input.clone())
            .unwrap();
        assert_eq!(entry.validity, expected);
        assert_eq!(
            service.record_object_state(7, &activity.id, input).unwrap(),
            entry
        );
        assert_eq!(
            service.object_state(7, &activity.id, None, 1).unwrap()[0].validity,
            expected
        );
        let before = load_history(
            &service.lock().unwrap(),
            &activity,
            start - chrono::Duration::nanoseconds(1),
        )
        .unwrap();
        let during = load_history(&service.lock().unwrap(), &activity, start).unwrap();
        let after = load_history(&service.lock().unwrap(), &activity, end).unwrap();
        for (history, validity) in [
            (before, ObjectStateValidity::NotYetApplicable),
            (during, ObjectStateValidity::WithinReportedWindow),
            (after, ObjectStateValidity::Expired),
        ] {
            let projected = history.iter().find(|stored| stored.id == entry.id).unwrap();
            assert_eq!(projected.validity, validity);
            assert_eq!(projected.recorded_at, entry.recorded_at);
            assert_eq!(projected.draft, entry.draft);
        }
    }
    let json: String = service
        .lock()
        .unwrap()
        .query_row(
            "SELECT draft_json FROM activity_object_state LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!json.contains("validity"));
    assert!(!json.contains("verified"));
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn late_metadata_and_corrections_never_reopen_or_complete_any_activity_state() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    for state in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let mut owned = activity(&service, 7);
        if state != ActivityState::Active {
            owned = service
                .transition(
                    7,
                    &owned.id,
                    state,
                    (state == ActivityState::Completed)
                        .then(|| "Explicit user confirmation".into()),
                )
                .unwrap();
        }
        let first = service
            .record_object_state(7, &owned.id, annotation())
            .unwrap();
        let receipt = unverified_receipt(&service, 7, &owned.id, receipt_report()).unwrap();
        service
            .record_object_state(7, &owned.id, app_report(&receipt.id))
            .unwrap();
        service
            .record_object_state(7, &owned.id, relation(TARGET))
            .unwrap();
        service
            .record_object_state(
                7,
                &owned.id,
                ObjectStateDraft {
                    content: ObjectStateContent::Retracted {
                        reason: "Late correction".into(),
                    },
                    supersedes: Some(first.id),
                    ..annotation()
                },
            )
            .unwrap();
        assert_eq!(
            service.object_state(7, &owned.id, None, 100).unwrap().len(),
            4
        );
        assert_eq!(service.get(7, &owned.id).unwrap(), owned);
    }
}

#[test]
fn detaching_resources_preserves_history_and_idempotency_but_not_new_attachment_eligibility() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 7);
    let receipt = unverified_receipt(&service, 7, &activity.id, receipt_report()).unwrap();
    let original = service
        .record_object_state(7, &activity.id, annotation())
        .unwrap();
    let related = service
        .record_object_state(7, &activity.id, relation(TARGET))
        .unwrap();
    let reported = service
        .record_object_state(7, &activity.id, app_report(&receipt.id))
        .unwrap();
    let detached = service
        .update(
            7,
            &activity.id,
            ActivityPatch {
                resources: Some(Vec::new()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        service
            .object_state(7, &activity.id, Some(SUBJECT), 100)
            .unwrap(),
        vec![reported, related.clone(), original.clone()]
    );
    assert_eq!(
        service
            .record_object_state(7, &activity.id, original.draft.clone())
            .unwrap(),
        original
    );
    assert!(matches!(
        service.record_object_state(7, &activity.id, annotation()),
        Err(ActivityError::Invalid(_))
    ));
    assert!(matches!(
        service.record_object_state(
            7,
            &activity.id,
            ObjectStateDraft {
                supersedes: Some(original.id),
                ..annotation()
            }
        ),
        Err(ActivityError::Invalid(_))
    ));
    assert_eq!(service.get(7, &activity.id).unwrap(), detached);
    service
        .add_resource(
            7,
            &activity.id,
            ActivityResource {
                label: "Subject again".into(),
                reference: SUBJECT.into(),
            },
        )
        .unwrap();
    assert!(matches!(
        service.record_object_state(7, &activity.id, relation(TARGET)),
        Err(ActivityError::Invalid(_))
    ));
    service
        .record_object_state(
            7,
            &activity.id,
            ObjectStateDraft {
                content: ObjectStateContent::Retracted {
                    reason: "Retract a link to a detached target".into(),
                },
                supersedes: Some(related.id),
                ..annotation()
            },
        )
        .unwrap();
}

#[test]
fn corrupt_object_state_fields_are_errors_even_when_a_view_would_filter_them_out() {
    for (column, value) in [
        ("source", "os_confirmed".to_string()),
        ("id", "not-a-uuid".to_string()),
        ("reference", ALTERNATE.to_string()),
        ("recorded_at", "not a timestamp".to_string()),
        ("recorded_at", "2026-01-01T00:00:00+00:00".to_string()),
        ("draft_json", "not JSON".to_string()),
        ("draft_json", "{}".to_string()),
        ("receipt_id", uuid::Uuid::new_v4().to_string()),
        ("supersedes", uuid::Uuid::new_v4().to_string()),
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let activity = activity(&service, 7);
        let original = service
            .record_object_state(7, &activity.id, annotation())
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON;")
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute(
                &format!("UPDATE activity_object_state SET {column} = ?1"),
                [&value],
            )
            .unwrap();
        assert!(
            matches!(
                service.object_state(7, &activity.id, None, 100),
                Err(ActivityError::Corrupt(_))
            ),
            "{column}"
        );
        assert!(
            matches!(
                service.object_state(7, &activity.id, Some(TARGET), 1),
                Err(ActivityError::Corrupt(_))
            ),
            "filtered {column}"
        );
        assert!(
            matches!(
                service.record_object_state(7, &activity.id, original.draft),
                Err(ActivityError::Corrupt(_))
            ),
            "retry {column}"
        );
        let stored: String = service
            .lock()
            .unwrap()
            .query_row(
                &format!("SELECT {column} FROM activity_object_state"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, value);
    }
}

#[test]
fn persisted_drafts_must_remain_canonical_and_cannot_forge_owner_or_source() {
    for change in [
        "id",
        "text",
        "window",
        "owner_uid",
        "source",
        "self",
        "retraction",
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let activity = activity(&service, 7);
        let original = service
            .record_object_state(7, &activity.id, annotation())
            .unwrap();
        let mut value = serde_json::to_value(&original.draft).unwrap();
        match change {
            "id" => {
                value["id"] = serde_json::json!(uuid::Uuid::parse_str(&original.id)
                    .unwrap()
                    .simple()
                    .to_string())
            }
            "text" => value["content"]["text"] = serde_json::json!(" untrimmed "),
            "window" => {
                value["observed_at"] = serde_json::json!("2026-01-01T00:00:00+00:00");
                value["valid_until"] = serde_json::json!("2026-01-02T00:00:00+00:00");
            }
            "owner_uid" => value["owner_uid"] = serde_json::json!(0),
            "source" => value["source"] = serde_json::json!("os_confirmed"),
            "self" => value["supersedes"] = serde_json::json!(original.id),
            "retraction" => {
                value["content"] = serde_json::json!({"kind":"retracted","reason":"No predecessor"})
            }
            _ => unreachable!(),
        }
        service
            .lock()
            .unwrap()
            .execute(
                "UPDATE activity_object_state SET draft_json = ?1",
                [value.to_string()],
            )
            .unwrap();
        assert!(
            matches!(
                service.object_state(7, &activity.id, None, 100),
                Err(ActivityError::Corrupt(_))
            ),
            "{change}"
        );
    }
}

#[test]
fn corrupt_supersession_edges_fail_instead_of_being_skipped_or_repaired() {
    for change in [
        "future",
        "duplicate",
        "missing",
        "foreign",
        "reference",
        "retracted",
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let owned = activity(&service, 7);
        let foreign = activity(&service, 8);
        let foreign_entry = service
            .record_object_state(8, &foreign.id, annotation())
            .unwrap();
        let first = service
            .record_object_state(7, &owned.id, annotation())
            .unwrap();
        let second = service
            .record_object_state(
                7,
                &owned.id,
                ObjectStateDraft {
                    supersedes: Some(first.id.clone()),
                    ..annotation()
                },
            )
            .unwrap();
        let third = service
            .record_object_state(
                7,
                &owned.id,
                ObjectStateDraft {
                    supersedes: Some(second.id.clone()),
                    ..annotation()
                },
            )
            .unwrap();
        let mut changed = third.draft.clone();
        match change {
            "future" => {
                changed = first.draft.clone();
                changed.supersedes = Some(third.id);
            }
            "duplicate" => changed.supersedes = Some(first.id),
            "missing" => changed.supersedes = Some(uuid::Uuid::new_v4().to_string()),
            "foreign" => changed.supersedes = Some(foreign_entry.id),
            "reference" => changed.reference = ALTERNATE.into(),
            "retracted" => {
                changed = second.draft;
                changed.content = ObjectStateContent::Retracted {
                    reason: "This entry is withdrawn".into(),
                };
            }
            _ => unreachable!(),
        }
        service
            .lock()
            .unwrap()
            .execute_batch(
                "PRAGMA foreign_keys = OFF; DROP INDEX activity_object_state_supersession;",
            )
            .unwrap();
        service.lock().unwrap().execute(
            "UPDATE activity_object_state SET draft_json = ?1, reference = ?2, supersedes = ?3 WHERE id = ?4",
            params![serde_json::to_string(&changed).unwrap(), changed.reference, changed.supersedes, changed.id],
        ).unwrap();
        assert!(
            matches!(
                service.object_state(7, &owned.id, None, 1),
                Err(ActivityError::Corrupt(_))
            ),
            "{change}"
        );
        assert!(
            matches!(
                service.record_object_state(7, &owned.id, annotation()),
                Err(ActivityError::Corrupt(_))
            ),
            "{change}"
        );
    }
}

#[test]
fn invalid_linked_receipt_data_is_never_copied_or_promoted_to_an_app_report() {
    for change in ["json", "app", "source", "activity", "owner", "missing"] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let owned = activity(&service, 7);
        let another = activity(&service, 7);
        let receipt = unverified_receipt(&service, 7, &owned.id, receipt_report()).unwrap();
        let entry = service
            .record_object_state(7, &owned.id, app_report(&receipt.id))
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON;")
            .unwrap();
        match change {
            "json" => {
                service
                    .lock()
                    .unwrap()
                    .execute("UPDATE activity_receipts SET report_json = '{}'", [])
                    .unwrap();
            }
            "app" => {
                let mut report = receipt.report;
                report.app_id = "kv".into();
                service
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE activity_receipts SET report_json = ?1",
                        [serde_json::to_string(&report).unwrap()],
                    )
                    .unwrap();
            }
            "source" => {
                service
                    .lock()
                    .unwrap()
                    .execute("UPDATE activity_receipts SET source = 'os_confirmed'", [])
                    .unwrap();
            }
            "activity" => {
                service
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE activity_receipts SET activity_id = ?1",
                        [&another.id],
                    )
                    .unwrap();
            }
            "owner" => {
                service
                    .lock()
                    .unwrap()
                    .execute("UPDATE activity_receipts SET owner_uid = 0", [])
                    .unwrap();
            }
            "missing" => {
                service
                    .lock()
                    .unwrap()
                    .execute("DELETE FROM activity_receipts", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            matches!(
                service.object_state(7, &owned.id, None, 100),
                Err(ActivityError::Corrupt(_))
            ),
            "{change}"
        );
        assert!(
            matches!(
                service.record_object_state(7, &owned.id, entry.draft),
                Err(ActivityError::Corrupt(_))
            ),
            "{change}"
        );
    }
}

#[test]
fn ignored_rewritten_or_lifecycle_changing_inserts_roll_back_instead_of_reporting_success() {
    for trigger in [
        "CREATE TRIGGER ignore_state BEFORE INSERT ON activity_object_state
         BEGIN SELECT RAISE(IGNORE); END;",
        "CREATE TRIGGER rewrite_state AFTER INSERT ON activity_object_state
         BEGIN UPDATE activity_object_state SET recorded_at = '2000-01-01T00:00:00.000000000Z'
         WHERE sequence = NEW.sequence; END;",
        "CREATE TRIGGER rewrite_draft AFTER INSERT ON activity_object_state
         BEGIN UPDATE activity_object_state SET draft_json = replace(NEW.draft_json, 'Caller statement', 'Changed')
         WHERE sequence = NEW.sequence; END;",
        "CREATE TRIGGER change_goal AFTER INSERT ON activity_object_state
         BEGIN UPDATE activities SET goal = 'Unrelated mutation' WHERE id = NEW.activity_id; END;",
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let activity = activity(&service, 7);
        service.lock().unwrap().execute_batch(trigger).unwrap();
        assert!(matches!(
            service.record_object_state(7, &activity.id, annotation()),
            Err(ActivityError::Corrupt(_))
        ));
        assert!(service.object_state(7, &activity.id, None, 100).unwrap().is_empty());
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
    }
}

#[test]
fn object_state_storage_failures_and_poisoned_locks_are_explicit() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 7);
    service
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_state BEFORE INSERT ON activity_object_state
         BEGIN SELECT RAISE(ABORT, 'write failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        service.record_object_state(7, &activity.id, annotation()),
        Err(ActivityError::Database(_))
    ));
    assert!(service
        .object_state(7, &activity.id, None, 100)
        .unwrap()
        .is_empty());
    let clone = service.clone();
    assert!(std::thread::spawn(move || {
        let _guard = clone.conn.lock().unwrap();
        panic!("poison object-state database lock");
    })
    .join()
    .is_err());
    assert!(matches!(
        service.record_object_state(7, &activity.id, annotation()),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.object_state(7, &activity.id, None, 100),
        Err(ActivityError::Poisoned)
    ));
}

fn schema_two_database(path: &Path) -> (Vec<Activity>, Vec<ActivityReceipt>) {
    let activities = schema_one_database(path);
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(MIGRATE_TO_V2).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    let mut receipts = Vec::new();
    for (index, activity) in activities.iter().enumerate() {
        let mut report = receipt_report();
        if index % 2 != 0 {
            report.outcome = ReceiptOutcome::Indeterminate;
            report.result = None;
            report.error = Some("Legacy caller reports unavailable execution results".into());
        }
        let receipt = ActivityReceipt {
            id: report.id.clone(),
            activity_id: activity.id.clone(),
            owner_uid: activity.owner_uid,
            received_at: "2026-01-03T00:00:00.000000000Z".into(),
            source: ReceiptSource::CallerReported,
            report,
            declaration: (index % 2 == 0).then(receipt_declaration),
            declaration_error: (index % 2 != 0).then(|| "Legacy unverified declaration".into()),
        };
        conn.execute(
            "INSERT INTO activity_receipts (
                id, activity_id, owner_uid, received_at, source,
                report_json, declaration_json, declaration_error
             ) VALUES (?1, ?2, ?3, ?4, 'caller_reported', ?5, ?6, ?7)",
            params![
                receipt.id,
                receipt.activity_id,
                receipt.owner_uid,
                receipt.received_at,
                serde_json::to_string(&receipt.report).unwrap(),
                receipt
                    .declaration
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .unwrap(),
                receipt.declaration_error,
            ],
        )
        .unwrap();
        receipts.push(receipt);
    }
    (activities, receipts)
}

fn table_rows(conn: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    let count = statement.column_count();
    let rows = statement
        .query_map([], |row| (0..count).map(|index| row.get(index)).collect())
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)",
        [name],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn schema_one_and_two_migrate_sequentially_to_three_without_rewriting_any_legacy_fields() {
    assert_eq!(SCHEMA_VERSION, 1);
    assert_eq!(DATABASE_SCHEMA_VERSION, 3);
    for version in [1, 2] {
        let directory = TestDirectory::new();
        let path = directory.database();
        let (activities, receipts) = if version == 1 {
            (schema_one_database(&path), Vec::new())
        } else {
            schema_two_database(&path)
        };
        let (activity_rows, receipt_rows) = {
            let conn = Connection::open(&path).unwrap();
            (
                table_rows(&conn, "activities"),
                (version == 2).then(|| table_rows(&conn, "activity_receipts")),
            )
        };
        let service = SqliteActivityService::open(&path).unwrap();
        {
            let conn = service.lock().unwrap();
            assert_eq!(
                conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                    .unwrap(),
                3
            );
            assert_eq!(table_rows(&conn, "activities"), activity_rows);
            if let Some(receipt_rows) = receipt_rows {
                assert_eq!(table_rows(&conn, "activity_receipts"), receipt_rows);
            }
            assert!(table_exists(&conn, "activity_object_state"));
            validate_schema(&conn).unwrap();
        }
        for activity in activities {
            assert_eq!(
                service.get(activity.owner_uid, &activity.id).unwrap(),
                activity
            );
            let expected: Vec<_> = receipts
                .iter()
                .filter(|receipt| receipt.activity_id == activity.id)
                .cloned()
                .collect();
            assert_eq!(
                service
                    .receipts(activity.owner_uid, &activity.id, 100)
                    .unwrap(),
                expected
            );
            assert!(service
                .object_state(activity.owner_uid, &activity.id, None, 100)
                .unwrap()
                .is_empty());
        }
        let new = activity(&service, 7);
        service
            .record_object_state(7, &new.id, annotation())
            .unwrap();
        drop(service);
        let reopened = SqliteActivityService::open(&path).unwrap();
        assert_eq!(
            reopened.object_state(7, &new.id, None, 100).unwrap().len(),
            1
        );
    }
}

#[test]
fn corrupt_schema_one_metadata_rolls_back_both_new_ledgers_and_the_version() {
    for (column, value) in [
        ("resources_json", "not JSON"),
        ("title", " not normalized "),
        ("updated_at", "not a timestamp"),
        ("id", "not-a-uuid"),
        ("state", "invalid"),
    ] {
        let directory = TestDirectory::new();
        let path = directory.database();
        schema_one_database(&path);
        let before = {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
            conn.execute(
                &format!("UPDATE activities SET {column} = ?1 WHERE rowid = 1"),
                [value],
            )
            .unwrap();
            table_rows(&conn, "activities")
        };
        assert!(
            matches!(
                SqliteActivityService::open(&path),
                Err(ActivityError::Corrupt(_))
            ),
            "{column}"
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(table_rows(&conn, "activities"), before);
        for name in [
            "activity_receipts",
            "activities_owner_id",
            "activity_object_state",
            "activity_receipts_owner_activity_id",
        ] {
            assert!(!table_exists(&conn, name), "{column}: {name}");
        }
    }
}

#[test]
fn corrupt_schema_two_receipts_roll_back_object_state_migration_without_repairs() {
    for (column, value) in [
        ("report_json", "{}".to_string()),
        ("declaration_json", "{}".to_string()),
        ("received_at", "not a timestamp".to_string()),
        ("source", "os_confirmed".to_string()),
        ("activity_id", uuid::Uuid::new_v4().to_string()),
        ("id", "not-a-uuid".to_string()),
    ] {
        let directory = TestDirectory::new();
        let path = directory.database();
        schema_two_database(&path);
        let (activities, receipts) = {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON;")
                .unwrap();
            conn.execute(
                &format!("UPDATE activity_receipts SET {column} = ?1 WHERE sequence = 1"),
                [&value],
            )
            .unwrap();
            (
                table_rows(&conn, "activities"),
                table_rows(&conn, "activity_receipts"),
            )
        };
        assert!(
            matches!(
                SqliteActivityService::open(&path),
                Err(ActivityError::Corrupt(_))
            ),
            "{column}"
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            2
        );
        assert_eq!(table_rows(&conn, "activities"), activities);
        assert_eq!(table_rows(&conn, "activity_receipts"), receipts);
        assert!(!table_exists(&conn, "activity_object_state"));
        assert!(!table_exists(&conn, "activity_receipts_owner_activity_id"));
    }
}

#[test]
fn schema_three_creation_failure_rolls_back_the_added_receipt_index() {
    let directory = TestDirectory::new();
    let path = directory.database();
    schema_two_database(&path);
    let (activities, receipts) = {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE activity_object_state (sentinel TEXT);
             INSERT INTO activity_object_state VALUES ('preserve');",
        )
        .unwrap();
        (
            table_rows(&conn, "activities"),
            table_rows(&conn, "activity_receipts"),
        )
    };
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Database(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert!(!table_exists(&conn, "activity_receipts_owner_activity_id"));
    assert_eq!(table_rows(&conn, "activities"), activities);
    assert_eq!(table_rows(&conn, "activity_receipts"), receipts);
    assert_eq!(
        conn.query_row("SELECT sentinel FROM activity_object_state", [], |row| {
            row.get::<_, String>(0)
        },)
            .unwrap(),
        "preserve"
    );
}

#[test]
fn missing_ownership_constraints_prevent_migration_even_when_foreign_key_check_has_no_rows() {
    let directory = TestDirectory::new();
    let path = directory.database();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let incomplete = MIGRATE_TO_V2.replace(
            "FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id),",
            "",
        );
        assert_ne!(incomplete, MIGRATE_TO_V2);
        conn.execute_batch(&incomplete).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Corrupt(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert!(!table_exists(&conn, "activity_object_state"));
    assert!(!table_exists(&conn, "activity_receipts_owner_activity_id"));
}

#[test]
fn missing_history_indexes_are_reported_not_recreated_when_opening_schema_three() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let expected = {
        let service = SqliteActivityService::open(&path).unwrap();
        let owned = activity(&service, 7);
        service
            .record_object_state(7, &owned.id, annotation())
            .unwrap();
        owned
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP INDEX activity_object_state_supersession")
            .unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Corrupt(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        3
    );
    assert!(!table_exists(&conn, "activity_object_state_supersession"));
    assert_eq!(load_activity(&conn, 7, &expected.id).unwrap(), expected);
    assert_eq!(table_rows(&conn, "activity_object_state").len(), 1);
}

#[test]
fn object_state_foreign_keys_enforce_owner_and_activity_binding_and_detect_damaged_storage() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let owned = {
        let service = SqliteActivityService::open(&path).unwrap();
        let owned = activity(&service, 7);
        service
            .record_object_state(7, &owned.id, annotation())
            .unwrap();
        assert!(service
            .lock()
            .unwrap()
            .execute("UPDATE activity_object_state SET owner_uid = 0", [],)
            .is_err());
        owned
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute("UPDATE activity_object_state SET owner_uid = 0", [])
            .unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Corrupt(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(load_activity(&conn, 7, &owned.id).unwrap(), owned);
    assert_eq!(
        conn.query_row("SELECT owner_uid FROM activity_object_state", [], |row| row
            .get::<_, u32>(0),)
            .unwrap(),
        0
    );
}

#[test]
fn corrupt_sequence_or_oversize_history_is_not_silently_capped_or_pruned() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let owned = activity(&service, 7);
    service
        .record_object_state(7, &owned.id, annotation())
        .unwrap();
    service
        .lock()
        .unwrap()
        .execute_batch(
            "PRAGMA ignore_check_constraints = ON; UPDATE activity_object_state SET sequence = 0;",
        )
        .unwrap();
    assert!(matches!(
        service.object_state(7, &owned.id, None, 1),
        Err(ActivityError::Corrupt(_))
    ));
    let service = SqliteActivityService::open_in_memory().unwrap();
    let owned = activity(&service, 7);
    {
        let mut conn = service.lock().unwrap();
        let tx = conn.transaction().unwrap();
        for _ in 0..=MAX_ENTRIES_PER_ACTIVITY {
            insert_raw(&tx, &owned, &annotation()).unwrap();
        }
        tx.commit().unwrap();
    }
    assert!(matches!(
        service.object_state(7, &owned.id, None, 1),
        Err(ActivityError::Corrupt(_))
    ));
    assert!(matches!(
        service.record_object_state(7, &owned.id, annotation()),
        Err(ActivityError::Corrupt(_))
    ));
    assert_eq!(
        table_rows(&service.lock().unwrap(), "activity_object_state").len(),
        MAX_ENTRIES_PER_ACTIVITY + 1
    );
}

#[test]
fn object_state_migration_crash_worker() {
    let Some(path) = std::env::var_os("COS_TEST_OBJECT_STATE_MIGRATION_DB") else {
        return;
    };
    let mut conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "synchronous", "FULL").unwrap();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    tx.execute_batch(MIGRATE_TO_V3).unwrap();
    validate_schema(&tx).unwrap();
    tx.pragma_update(None, "user_version", 3).unwrap();
    tx.cache_flush().unwrap();
    std::process::exit(92);
}

#[test]
fn interrupted_schema_three_migration_recovers_without_losing_legacy_receipts() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let (activities, receipts) = schema_two_database(&path);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "activities::sqlite::object_state::tests::object_state_migration_crash_worker",
            "--test-threads=1",
        ])
        .env("COS_TEST_OBJECT_STATE_MIGRATION_DB", &path)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(92),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    {
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            2
        );
        assert!(!table_exists(&conn, "activity_object_state"));
        assert!(!table_exists(&conn, "activity_receipts_owner_activity_id"));
    }
    let service = SqliteActivityService::open(&path).unwrap();
    for activity in activities {
        assert_eq!(
            service.get(activity.owner_uid, &activity.id).unwrap(),
            activity
        );
    }
    for receipt in receipts {
        assert_eq!(
            service
                .receipts(receipt.owner_uid, &receipt.activity_id, 100)
                .unwrap(),
            vec![receipt]
        );
    }
}
