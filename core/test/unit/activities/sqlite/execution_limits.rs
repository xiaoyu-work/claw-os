use super::super::tests::{
    draft as activity_draft, receipt_report, schema_one_database, TestDirectory,
};
use super::super::{object_state::MIGRATE_TO_V3, MIGRATE_TO_V2};
use super::*;
use crate::activities::{
    ActivityResource, ActivityService, ObjectStateContent, ObjectStateDraft,
    DATABASE_SCHEMA_VERSION, SCHEMA_VERSION,
};
use chrono::SecondsFormat;
use std::path::Path;
use std::sync::{Arc, Barrier};

fn limits(attempts: u32, turns: u32) -> ExecutionLimitsDraft {
    ExecutionLimitsDraft {
        max_attempts: attempts,
        max_turns_per_attempt: turns,
        expires_at: (Utc::now() + chrono::Duration::days(1))
            .to_rfc3339_opts(SecondsFormat::Nanos, true),
    }
}

fn configured(
    service: &dyn ActivityService,
    owner: u32,
    attempts: u32,
    turns: u32,
) -> (Activity, ActivityExecutionLimits) {
    let activity = service.create(owner, activity_draft()).unwrap();
    let policy = service
        .set_execution_limits(owner, &activity.id, None, limits(attempts, turns))
        .unwrap();
    (activity, policy)
}

fn attempt() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn assert_blocked<T: std::fmt::Debug>(
    result: Result<T, ActivityError>,
    expected: ExecutionBlockedReason,
) {
    match result {
        Err(ActivityError::ExecutionBlocked(actual)) => assert_eq!(actual, expected),
        other => panic!("expected {expected:?}, got {other:?}"),
    }
}

fn stored_rows(conn: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    let columns = statement.column_count();
    let rows = statement
        .query_map([], |row| {
            (0..columns).map(|column| row.get(column)).collect()
        })
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

fn exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)",
        [name],
        |row| row.get(0),
    )
    .unwrap()
}

fn make_expired(service: &SqliteActivityService, activity: &Activity) {
    let mut conn = service.lock().unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "UPDATE activity_execution_limits SET expires_at = '2000-01-02T00:00:00.000000000Z',
            created_at = '2000-01-01T00:00:00.000000000Z', updated_at = '2000-01-01T00:00:00.000000000Z'
         WHERE owner_uid = ?1 AND activity_id = ?2",
        params![i64::from(activity.owner_uid), activity.id],
    ).unwrap();
    tx.execute(
        "UPDATE activity_execution_reservations SET expires_at = '2000-01-02T00:00:00.000000000Z',
            reserved_at = '2000-01-01T00:00:00.000000000Z'
         WHERE owner_uid = ?1 AND activity_id = ?2",
        params![i64::from(activity.owner_uid), activity.id],
    )
    .unwrap();
    tx.commit().unwrap();
}

#[test]
fn absent_policies_preserve_legacy_behavior_without_creating_accounting_or_authority() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    for state in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let mut activity = service.create(7, activity_draft()).unwrap();
        if state != ActivityState::Active {
            activity = service
                .transition(
                    7,
                    &activity.id,
                    state,
                    (state == ActivityState::Completed).then(|| "Confirmed".into()),
                )
                .unwrap();
        }
        assert!(service.execution_limits(7, &activity.id).unwrap().is_none());
        assert!(service
            .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
            .unwrap()
            .is_none());
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
    }
    let conn = service.lock().unwrap();
    assert!(stored_rows(&conn, "activity_execution_limits").is_empty());
    assert!(stored_rows(&conn, "activity_execution_reservations").is_empty());
}

#[test]
fn policies_and_reservations_persist_without_changing_the_activity_or_opening_jobs() {
    let directory = TestDirectory::new();
    let (activity, expected, reservation) = {
        let provider = SqliteActivityService::open(directory.database()).unwrap();
        let service: &dyn ActivityService = &provider;
        let (activity, policy) = configured(service, 7, 3, 10);
        assert_eq!(policy.revision, 1);
        assert!(policy.enabled);
        assert_eq!(policy.used_attempts, 0);
        assert_eq!(policy.created_at, policy.updated_at);
        let reservation = service
            .reserve_execution(
                7,
                &activity.id,
                &attempt(),
                "no-job-file-is-opened",
                Some(50),
            )
            .unwrap()
            .unwrap();
        assert_eq!(reservation.max_turns, 10);
        assert_eq!(reservation.expires_at, policy.limits.expires_at);
        assert_eq!(reservation.policy_revision, 1);
        assert_eq!(reservation.owner_uid, 7);
        assert_eq!(reservation.activity_id, activity.id);
        let expected = service.execution_limits(7, &activity.id).unwrap().unwrap();
        assert_eq!(expected.used_attempts, 1);
        assert_eq!(expected.revision, 1);
        assert_eq!(expected.created_at, policy.created_at);
        assert!(expected.updated_at >= policy.updated_at);
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
        (activity, expected, reservation)
    };
    let reopened = SqliteActivityService::open(directory.database()).unwrap();
    assert_eq!(
        reopened.execution_limits(7, &activity.id).unwrap(),
        Some(expected)
    );
    assert_eq!(
        reopened
            .reserve_execution(
                7,
                &activity.id,
                &reservation.id,
                &reservation.job_id,
                Some(50)
            )
            .unwrap(),
        Some(reservation)
    );
    assert_eq!(reopened.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn execution_constraints_isolate_every_owner_including_root_and_share_no_attempt_ids() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let owners = [0, 7, 8, u32::MAX];
    let records: Vec<_> = owners
        .iter()
        .map(|owner| configured(&service, *owner, 2, 10).0)
        .collect();
    let id = attempt();
    for activity in &records {
        let reservation = service
            .reserve_execution(activity.owner_uid, &activity.id, &id, "job-1", None)
            .unwrap()
            .unwrap();
        assert_eq!(reservation.id, id);
        assert_eq!(reservation.owner_uid, activity.owner_uid);
        for owner in owners
            .into_iter()
            .filter(|owner| *owner != activity.owner_uid)
        {
            for activity_id in [&activity.id, &attempt()] {
                assert!(matches!(
                    service.execution_limits(owner, activity_id),
                    Err(ActivityError::NotFound)
                ));
                assert!(matches!(
                    service.set_execution_limits(owner, activity_id, None, limits(2, 10)),
                    Err(ActivityError::NotFound)
                ));
                assert!(matches!(
                    service.set_execution_limits_enabled(owner, activity_id, 1, false),
                    Err(ActivityError::NotFound)
                ));
                assert!(matches!(
                    service.reserve_execution(owner, activity_id, &id, "job-1", None),
                    Err(ActivityError::NotFound)
                ));
            }
        }
    }
}

#[test]
fn policy_configuration_uses_cas_and_preserves_counters_when_limits_are_lowered() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, first) = configured(&service, 7, 5, 20);
    service
        .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
        .unwrap();
    service
        .reserve_execution(7, &activity.id, &attempt(), "job-2", None)
        .unwrap();
    assert!(matches!(
        service.set_execution_limits(7, &activity.id, None, limits(5, 20)),
        Err(ActivityError::Conflict(_))
    ));
    let lowered = service
        .set_execution_limits(7, &activity.id, Some(first.revision), limits(1, 3))
        .unwrap();
    assert_eq!(lowered.revision, 2);
    assert_eq!(lowered.used_attempts, 2);
    assert_eq!(lowered.limits.max_attempts, 1);
    assert_eq!(lowered.created_at, first.created_at);
    assert!(lowered.enabled);
    assert_blocked(
        service.reserve_execution(7, &activity.id, &attempt(), "job-3", None),
        ExecutionBlockedReason::AttemptLimit,
    );
    assert_blocked(
        service.set_execution_limits(7, &activity.id, Some(1), limits(10, 10)),
        ExecutionBlockedReason::StaleRevision,
    );
    assert_blocked(
        service.set_execution_limits_enabled(7, &activity.id, 1, false),
        ExecutionBlockedReason::StaleRevision,
    );
    assert_eq!(
        service.execution_limits(7, &activity.id).unwrap(),
        Some(lowered)
    );
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
    let absent = service.create(7, activity_draft()).unwrap();
    assert!(matches!(
        service.set_execution_limits(7, &absent.id, Some(1), limits(5, 5)),
        Err(ActivityError::Conflict(_))
    ));
    assert!(matches!(
        service.set_execution_limits_enabled(7, &absent.id, 1, false),
        Err(ActivityError::NotFound)
    ));
    assert!(service.execution_limits(7, &absent.id).unwrap().is_none());
}

#[test]
fn edits_never_reset_usage_or_reenable_and_every_explicit_enable_change_advances_revision() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, _) = configured(&service, 7, 5, 10);
    service
        .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
        .unwrap();
    let disabled = service
        .set_execution_limits_enabled(7, &activity.id, 1, false)
        .unwrap();
    assert_eq!(disabled.revision, 2);
    assert!(!disabled.enabled);
    assert_eq!(disabled.used_attempts, 1);
    let still_disabled = service
        .set_execution_limits_enabled(7, &activity.id, 2, false)
        .unwrap();
    assert_eq!(still_disabled.revision, 3);
    let edited = service
        .set_execution_limits(7, &activity.id, Some(3), limits(10, 20))
        .unwrap();
    assert!(!edited.enabled);
    assert_eq!(edited.used_attempts, 1);
    assert_eq!(edited.revision, 4);
    assert_blocked(
        service.reserve_execution(7, &activity.id, &attempt(), "job-2", None),
        ExecutionBlockedReason::Disabled,
    );
    let enabled = service
        .set_execution_limits_enabled(7, &activity.id, 4, true)
        .unwrap();
    assert_eq!(enabled.revision, 5);
    assert_eq!(enabled.used_attempts, 1);
    assert!(enabled.enabled);
    let enabled_again = service
        .set_execution_limits_enabled(7, &activity.id, 5, true)
        .unwrap();
    assert_eq!(enabled_again.revision, 6);
    assert_eq!(enabled_again.used_attempts, 1);
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn reservation_retries_canonicalize_uuid_spellings_and_conflict_on_any_changed_request_identity() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, _) = configured(&service, 7, 5, 10);
    let id = uuid::Uuid::new_v4();
    let original = service
        .reserve_execution(7, &activity.id, &id.to_string(), "job-1", None)
        .unwrap()
        .unwrap();
    for spelling in [
        id.to_string().to_uppercase(),
        id.simple().to_string(),
        id.urn().to_string(),
    ] {
        assert_eq!(
            service
                .reserve_execution(7, &activity.id.to_uppercase(), &spelling, "job-1", None)
                .unwrap(),
            Some(original.clone())
        );
    }
    for (job, turns) in [("job-2", None), ("job-1", Some(10)), ("job-1", Some(100))] {
        assert!(matches!(
            service.reserve_execution(7, &activity.id, &original.id, job, turns),
            Err(ActivityError::Conflict(_))
        ));
    }
    let requested_id = attempt();
    let requested = service
        .reserve_execution(7, &activity.id, &requested_id, "job-1", Some(100))
        .unwrap();
    assert!(matches!(
        service.reserve_execution(7, &activity.id, &requested_id, "job-1", Some(101)),
        Err(ActivityError::Conflict(_))
    ));
    assert_eq!(
        service
            .reserve_execution(7, &activity.id, &requested_id, "job-1", Some(100))
            .unwrap(),
        requested
    );
    let other = service.create(7, activity_draft()).unwrap();
    assert!(matches!(
        service.reserve_execution(7, &other.id, &original.id, "job-1", None),
        Err(ActivityError::Conflict(_))
    ));
    assert_eq!(
        service
            .execution_limits(7, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        2
    );
    assert!(service.execution_limits(7, &other.id).unwrap().is_none());
}

#[test]
fn turn_limits_default_or_clamp_every_positive_request_without_overflow() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, _) = configured(&service, 7, 10, 10);
    for (requested, effective) in [
        (None, 10),
        (Some(1), 1),
        (Some(10), 10),
        (Some(11), 10),
        (Some(100), 10),
        (Some(u32::MAX), 10),
    ] {
        let reservation = service
            .reserve_execution(7, &activity.id, &attempt(), "job-1", requested)
            .unwrap()
            .unwrap();
        assert_eq!(reservation.max_turns, effective);
    }
    assert!(matches!(
        service.reserve_execution(7, &activity.id, &attempt(), "job-1", Some(0)),
        Err(ActivityError::Invalid(_))
    ));
    assert_eq!(
        service
            .execution_limits(7, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        6
    );
}

#[test]
fn lifetime_attempt_quota_is_exact_and_exhausted_current_retries_are_not_charged_again() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, _) = configured(&service, 7, MAX_ATTEMPTS, 1);
    let first = service
        .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
        .unwrap()
        .unwrap();
    let mut last = first.clone();
    for _ in 1..MAX_ATTEMPTS {
        last = service
            .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        service
            .execution_limits(7, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        1000
    );
    assert_blocked(
        service.reserve_execution(7, &activity.id, &attempt(), "job-1", None),
        ExecutionBlockedReason::AttemptLimit,
    );
    for entry in [first, last] {
        assert_eq!(
            service
                .reserve_execution(7, &activity.id, &entry.id, "job-1", None)
                .unwrap(),
            Some(entry)
        );
    }
    service
        .set_execution_limits(7, &activity.id, Some(1), limits(1000, 100))
        .unwrap();
    assert_blocked(
        service.reserve_execution(7, &activity.id, &attempt(), "job-1", None),
        ExecutionBlockedReason::AttemptLimit,
    );
    assert_eq!(
        service
            .execution_limits(7, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        1000
    );
    let (other, _) = configured(&service, 7, 1, 1);
    service
        .reserve_execution(7, &other.id, &attempt(), "job-1", None)
        .unwrap()
        .unwrap();
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn independent_connections_atomically_reserve_the_last_attempt_for_exactly_one_winner() {
    let directory = TestDirectory::new();
    let first = SqliteActivityService::open(directory.database()).unwrap();
    let second = SqliteActivityService::open(directory.database()).unwrap();
    let (activity, _) = configured(&first, 7, 1, 10);
    let id = activity.id.clone();
    let barrier = Arc::new(Barrier::new(2));
    let other_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        other_barrier.wait();
        second.reserve_execution(7, &id, &attempt(), "job-2", None)
    });
    barrier.wait();
    let results = [
        first.reserve_execution(7, &activity.id, &attempt(), "job-1", None),
        worker.join().unwrap(),
    ];
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(Some(_))))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(
                result,
                Err(ActivityError::ExecutionBlocked(
                    ExecutionBlockedReason::AttemptLimit
                ))
            ))
            .count(),
        1
    );
    assert_eq!(
        first
            .execution_limits(7, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        1
    );
}

#[test]
fn concurrent_exact_retries_share_one_charge_and_concurrent_policy_edits_use_cas() {
    let directory = TestDirectory::new();
    let first = SqliteActivityService::open(directory.database()).unwrap();
    let second = SqliteActivityService::open(directory.database()).unwrap();
    let (activity, _) = configured(&first, 7, 1, 10);
    let id = activity.id.clone();
    let attempt_id = attempt();
    let copy = attempt_id.clone();
    let barrier = Arc::new(Barrier::new(2));
    let other_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        other_barrier.wait();
        second
            .reserve_execution(7, &id, &copy, "job-1", None)
            .unwrap()
    });
    barrier.wait();
    let original = first
        .reserve_execution(7, &activity.id, &attempt_id, "job-1", None)
        .unwrap();
    assert_eq!(worker.join().unwrap(), original);
    assert_eq!(
        first
            .execution_limits(7, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        1
    );

    let second = SqliteActivityService::open(directory.database()).unwrap();
    let id = activity.id.clone();
    let other_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        other_barrier.wait();
        second.set_execution_limits(7, &id, Some(1), limits(5, 5))
    });
    barrier.wait();
    let results = [
        first.set_execution_limits(7, &activity.id, Some(1), limits(10, 10)),
        worker.join().unwrap(),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(
                result,
                Err(ActivityError::ExecutionBlocked(
                    ExecutionBlockedReason::StaleRevision
                ))
            ))
            .count(),
        1
    );
    let policy = first.execution_limits(7, &activity.id).unwrap().unwrap();
    assert_eq!(policy.revision, 2);
    assert_eq!(policy.used_attempts, 1);
}

#[test]
fn old_reservations_cannot_bypass_pause_revocation_reenable_or_policy_edits() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, _) = configured(&service, 7, 5, 10);
    let original = service
        .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
        .unwrap()
        .unwrap();
    service
        .transition(7, &activity.id, ActivityState::Paused, None)
        .unwrap();
    assert_blocked(
        service.reserve_execution(7, &activity.id, &original.id, "job-1", None),
        ExecutionBlockedReason::Inactive,
    );
    service
        .transition(7, &activity.id, ActivityState::Active, None)
        .unwrap();
    assert_eq!(
        service
            .reserve_execution(7, &activity.id, &original.id, "job-1", None)
            .unwrap(),
        Some(original.clone())
    );
    service
        .set_execution_limits_enabled(7, &activity.id, 1, false)
        .unwrap();
    assert_blocked(
        service.reserve_execution(7, &activity.id, &original.id, "job-1", None),
        ExecutionBlockedReason::Disabled,
    );
    service
        .set_execution_limits_enabled(7, &activity.id, 2, true)
        .unwrap();
    assert_blocked(
        service.reserve_execution(7, &activity.id, &original.id, "job-1", None),
        ExecutionBlockedReason::StaleRevision,
    );
    let current = service
        .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
        .unwrap()
        .unwrap();
    service
        .set_execution_limits(7, &activity.id, Some(3), limits(5, 5))
        .unwrap();
    assert_blocked(
        service.reserve_execution(7, &activity.id, &current.id, "job-1", None),
        ExecutionBlockedReason::StaleRevision,
    );
    assert_eq!(
        service
            .execution_limits(7, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        2
    );
}

#[test]
fn expiry_is_strict_at_the_transaction_clock_and_expired_retries_never_bypass_it() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, policy) = configured(&service, 7, 1, 10);
    let expiry = policy.limits.expiry().unwrap();
    require_future(&policy.limits, expiry - chrono::Duration::nanoseconds(1)).unwrap();
    assert_blocked(
        require_future(&policy.limits, expiry),
        ExecutionBlockedReason::Expired,
    );
    assert_blocked(
        require_future(&policy.limits, expiry + chrono::Duration::nanoseconds(1)),
        ExecutionBlockedReason::Expired,
    );
    let original = service
        .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
        .unwrap()
        .unwrap();
    make_expired(&service, &activity);
    assert_eq!(
        service
            .execution_limits(7, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        1
    );
    for id in [&original.id, &attempt()] {
        assert_blocked(
            service.reserve_execution(7, &activity.id, id, "job-1", None),
            ExecutionBlockedReason::Expired,
        );
    }
    assert_blocked(
        service.set_execution_limits_enabled(7, &activity.id, 1, true),
        ExecutionBlockedReason::Expired,
    );
    let past = ExecutionLimitsDraft {
        expires_at: "2000-01-01T00:00:00Z".into(),
        ..limits(5, 10)
    };
    past.validate().unwrap();
    assert_blocked(
        service.set_execution_limits(7, &activity.id, Some(1), past.clone()),
        ExecutionBlockedReason::Expired,
    );
    let absent = service.create(7, activity_draft()).unwrap();
    assert_blocked(
        service.set_execution_limits(7, &absent.id, None, past),
        ExecutionBlockedReason::Expired,
    );
    assert!(service.execution_limits(7, &absent.id).unwrap().is_none());
    let disabled = service
        .set_execution_limits_enabled(7, &activity.id, 1, false)
        .unwrap();
    assert_eq!(disabled.revision, 2);
    let edited = service
        .set_execution_limits(7, &activity.id, Some(2), limits(5, 10))
        .unwrap();
    assert!(!edited.enabled);
    assert_eq!(edited.used_attempts, 1);
    service
        .set_execution_limits_enabled(7, &activity.id, 3, true)
        .unwrap();
    service
        .reserve_execution(7, &activity.id, &attempt(), "job-2", None)
        .unwrap()
        .unwrap();
}

#[test]
fn configuration_and_revocation_respect_lifecycle_without_mutating_goal_or_activity_timestamps() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    for state in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let (mut activity, _) = configured(&service, 7, 5, 10);
        if state != ActivityState::Active {
            activity = service
                .transition(
                    7,
                    &activity.id,
                    state,
                    (state == ActivityState::Completed).then(|| "Explicit confirmation".into()),
                )
                .unwrap();
        }
        let expected = service.execution_limits(7, &activity.id).unwrap().unwrap();
        if matches!(state, ActivityState::Completed | ActivityState::Cancelled) {
            assert_blocked(
                service.set_execution_limits(7, &activity.id, Some(1), limits(10, 10)),
                ExecutionBlockedReason::Inactive,
            );
            assert_blocked(
                service.set_execution_limits_enabled(7, &activity.id, 1, true),
                ExecutionBlockedReason::Inactive,
            );
            assert_eq!(
                service.execution_limits(7, &activity.id).unwrap(),
                Some(expected)
            );
        } else {
            let revised = service
                .set_execution_limits(7, &activity.id, Some(1), limits(10, 10))
                .unwrap();
            assert_eq!(revised.revision, 2);
        }
        if state != ActivityState::Active {
            assert_blocked(
                service.reserve_execution(7, &activity.id, &attempt(), "job-1", None),
                ExecutionBlockedReason::Inactive,
            );
        }
        let current = service.execution_limits(7, &activity.id).unwrap().unwrap();
        let disabled = service
            .set_execution_limits_enabled(7, &activity.id, current.revision, false)
            .unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.used_attempts, current.used_attempts);
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
    }
}

#[test]
fn invalid_identifiers_requests_and_revisions_fail_before_any_charge_or_policy_mutation() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, policy) = configured(&service, 7, 5, 10);
    for invalid in ["", "../activity", "not-a-uuid", "uuid\0"] {
        assert!(matches!(
            service.execution_limits(7, invalid),
            Err(ActivityError::Invalid(_))
        ));
        assert!(matches!(
            service.reserve_execution(7, &activity.id, invalid, "job-1", None),
            Err(ActivityError::Invalid(_))
        ));
    }
    for invalid in ["", "../job", "job/child", "job\\child", &"x".repeat(129)] {
        assert!(matches!(
            service.reserve_execution(7, &activity.id, &attempt(), invalid, None),
            Err(ActivityError::Invalid(_))
        ));
    }
    for revision in [0, u64::MAX] {
        assert!(matches!(
            service.set_execution_limits(7, &activity.id, Some(revision), limits(5, 10)),
            Err(ActivityError::Invalid(_))
        ));
        assert!(matches!(
            service.set_execution_limits_enabled(7, &activity.id, revision, false),
            Err(ActivityError::Invalid(_))
        ));
    }
    for draft in [
        ExecutionLimitsDraft {
            max_attempts: 0,
            ..limits(5, 10)
        },
        ExecutionLimitsDraft {
            max_turns_per_attempt: 101,
            ..limits(5, 10)
        },
        ExecutionLimitsDraft {
            expires_at: "invalid".into(),
            ..limits(5, 10)
        },
    ] {
        assert!(matches!(
            service.set_execution_limits(7, &activity.id, Some(1), draft),
            Err(ActivityError::Invalid(_))
        ));
    }
    assert_eq!(
        service.execution_limits(7, &activity.id).unwrap(),
        Some(policy)
    );
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn revision_exhaustion_is_an_error_without_wraparound_counter_reset_or_hidden_reenable() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, _) = configured(&service, 7, 5, 10);
    service
        .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
        .unwrap();
    service
        .lock()
        .unwrap()
        .execute(
            "UPDATE activity_execution_limits SET revision = ?1, enabled = 0",
            [i64::MAX],
        )
        .unwrap();
    let expected = service.execution_limits(7, &activity.id).unwrap().unwrap();
    assert_eq!(expected.revision, u64::try_from(i64::MAX).unwrap());
    assert!(matches!(
        service.set_execution_limits(7, &activity.id, Some(expected.revision), limits(10, 10)),
        Err(ActivityError::Conflict(_))
    ));
    assert!(matches!(
        service.set_execution_limits_enabled(7, &activity.id, expected.revision, true),
        Err(ActivityError::Conflict(_))
    ));
    assert_eq!(
        service.execution_limits(7, &activity.id).unwrap(),
        Some(expected)
    );
}

#[test]
fn corrupt_counters_and_policy_fields_are_never_reset_or_silently_repaired() {
    for update in [
        "used_attempts = -1",
        "used_attempts = 0",
        "used_attempts = 2",
        "used_attempts = 1001",
        "used_attempts = 4294967296",
        "revision = 0",
        "revision = -1",
        "enabled = 0",
        "enabled = 2",
        "max_attempts = 0",
        "max_attempts = 1001",
        "max_turns_per_attempt = 0",
        "max_turns_per_attempt = 101",
        "expires_at = 'not a timestamp'",
        "expires_at = '2099-01-01T00:00:00+00:00'",
        "updated_at = '1900-01-01T00:00:00.000000000Z'",
        "owner_uid = 0",
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let (activity, _) = configured(&service, 7, 5, 10);
        let reservation = service
            .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
            .unwrap()
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON;")
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute_batch(&format!("UPDATE activity_execution_limits SET {update}"))
            .unwrap();
        let before = stored_rows(&service.lock().unwrap(), "activity_execution_limits");
        assert!(
            matches!(
                service.execution_limits(7, &activity.id),
                Err(ActivityError::Corrupt(_))
            ),
            "{update}"
        );
        assert!(
            matches!(
                service.reserve_execution(7, &activity.id, &reservation.id, "job-1", None),
                Err(ActivityError::Corrupt(_))
            ),
            "{update}"
        );
        assert!(
            matches!(
                service.set_execution_limits(7, &activity.id, Some(1), limits(5, 10)),
                Err(ActivityError::Corrupt(_))
            ),
            "{update}"
        );
        assert!(
            matches!(
                service.set_execution_limits_enabled(7, &activity.id, 1, false),
                Err(ActivityError::Corrupt(_))
            ),
            "{update}"
        );
        assert_eq!(
            stored_rows(&service.lock().unwrap(), "activity_execution_limits"),
            before
        );
    }
}

#[test]
fn corrupt_reservation_rows_fail_reads_and_retries_instead_of_losing_accounting() {
    for update in [
        "sequence = 0",
        "id = 'not-a-uuid'",
        "job_id = '../job'",
        "policy_revision = 0",
        "policy_revision = -1",
        "policy_revision = 2",
        "policy_max_turns = 0",
        "policy_max_turns = 101",
        "policy_max_turns = 9",
        "requested_max_turns = 0",
        "requested_max_turns = -1",
        "requested_max_turns = 4294967296",
        "max_turns = 0",
        "max_turns = 11",
        "expires_at = '2000-01-01T00:00:00.000000000Z'",
        "reserved_at = 'invalid'",
        "reserved_at = '2099-01-01T00:00:00.000000000Z'",
        "owner_uid = 0",
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let (activity, _) = configured(&service, 7, 5, 10);
        let reservation = service
            .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
            .unwrap()
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON;")
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute_batch(&format!(
                "UPDATE activity_execution_reservations SET {update}"
            ))
            .unwrap();
        let before = stored_rows(&service.lock().unwrap(), "activity_execution_reservations");
        assert!(
            matches!(
                service.execution_limits(7, &activity.id),
                Err(ActivityError::Corrupt(_))
            ),
            "{update}"
        );
        assert!(
            matches!(
                service.reserve_execution(7, &activity.id, &reservation.id, "job-1", None),
                Err(ActivityError::Corrupt(_))
            ),
            "{update}"
        );
        assert_eq!(
            stored_rows(&service.lock().unwrap(), "activity_execution_reservations"),
            before
        );
    }
}

#[test]
fn reservations_without_policies_or_moved_across_activities_are_corruption_not_legacy_absence() {
    for remove_policy in [true, false] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let (owned, _) = configured(&service, 7, 5, 10);
        let other = service.create(7, activity_draft()).unwrap();
        service
            .reserve_execution(7, &owned.id, &attempt(), "job-1", None)
            .unwrap();
        service
            .lock()
            .unwrap()
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        if remove_policy {
            service
                .lock()
                .unwrap()
                .execute("DELETE FROM activity_execution_limits", [])
                .unwrap();
        } else {
            service
                .lock()
                .unwrap()
                .execute(
                    "UPDATE activity_execution_reservations SET activity_id = ?1",
                    [&other.id],
                )
                .unwrap();
            assert!(matches!(
                service.execution_limits(7, &other.id),
                Err(ActivityError::Corrupt(_))
            ));
            assert!(matches!(
                service.reserve_execution(7, &other.id, &attempt(), "job-2", None),
                Err(ActivityError::Corrupt(_))
            ));
        }
        assert!(matches!(
            service.execution_limits(7, &owned.id),
            Err(ActivityError::Corrupt(_))
        ));
        assert!(matches!(
            service.reserve_execution(7, &owned.id, &attempt(), "job-2", None),
            Err(ActivityError::Corrupt(_))
        ));
    }
}

#[test]
fn historical_reservations_retain_their_original_turn_snapshot_after_policy_edits() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, _) = configured(&service, 7, 5, 10);
    let old = service
        .reserve_execution(7, &activity.id, &attempt(), "job-1", Some(100))
        .unwrap()
        .unwrap();
    service
        .set_execution_limits(7, &activity.id, Some(1), limits(5, 3))
        .unwrap();
    let newer = service
        .reserve_execution(7, &activity.id, &attempt(), "job-2", Some(100))
        .unwrap()
        .unwrap();
    assert_eq!(newer.max_turns, 3);
    let history = load_state(&service.lock().unwrap(), &activity)
        .unwrap()
        .unwrap();
    assert_eq!(history.reservations[0].entry, old);
    assert_eq!(history.reservations[0].policy_max_turns, 10);
    assert_eq!(history.reservations[1].entry, newer);
    service
        .lock()
        .unwrap()
        .execute(
            "UPDATE activity_execution_reservations SET max_turns = 9 WHERE id = ?1",
            [&old.id],
        )
        .unwrap();
    assert!(matches!(
        service.execution_limits(7, &activity.id),
        Err(ActivityError::Corrupt(_))
    ));
}

#[test]
fn ignored_or_rewritten_writes_roll_back_policy_accounting_and_activity_changes() {
    for trigger in [
        "CREATE TRIGGER ignore_policy BEFORE INSERT ON activity_execution_limits
         BEGIN SELECT RAISE(IGNORE); END;",
        "CREATE TRIGGER rewrite_policy AFTER INSERT ON activity_execution_limits
         BEGIN UPDATE activity_execution_limits SET enabled = 0; END;",
        "CREATE TRIGGER mutate_goal AFTER INSERT ON activity_execution_limits
         BEGIN UPDATE activities SET goal = 'unexpected mutation' WHERE id = NEW.activity_id; END;",
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let activity = service.create(7, activity_draft()).unwrap();
        service.lock().unwrap().execute_batch(trigger).unwrap();
        assert!(matches!(
            service.set_execution_limits(7, &activity.id, None, limits(5, 10)),
            Err(ActivityError::Corrupt(_))
        ));
        assert!(service.execution_limits(7, &activity.id).unwrap().is_none());
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
    }
    for trigger in [
        "CREATE TRIGGER ignore_reservation BEFORE INSERT ON activity_execution_reservations
         BEGIN SELECT RAISE(IGNORE); END;",
        "CREATE TRIGGER rewrite_reservation AFTER INSERT ON activity_execution_reservations
         BEGIN UPDATE activity_execution_reservations SET job_id = 'rewritten' WHERE id = NEW.id; END;",
        "CREATE TRIGGER ignore_counter BEFORE UPDATE ON activity_execution_limits
         BEGIN SELECT RAISE(IGNORE); END;",
        "CREATE TRIGGER mutate_goal AFTER INSERT ON activity_execution_reservations
         BEGIN UPDATE activities SET goal = 'unexpected mutation' WHERE id = NEW.activity_id; END;",
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let (activity, policy) = configured(&service, 7, 5, 10);
        service.lock().unwrap().execute_batch(trigger).unwrap();
        assert!(matches!(
            service.reserve_execution(7, &activity.id, &attempt(), "job-1", None),
            Err(ActivityError::Corrupt(_))
        ));
        assert_eq!(service.execution_limits(7, &activity.id).unwrap(), Some(policy));
        assert!(stored_rows(&service.lock().unwrap(), "activity_execution_reservations").is_empty());
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
    }
}

#[test]
fn write_failures_and_poisoned_locks_never_return_success_or_legacy_defaults() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, policy) = configured(&service, 7, 5, 10);
    service
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_reservation BEFORE INSERT ON activity_execution_reservations
         BEGIN SELECT RAISE(ABORT, 'write failed'); END;",
        )
        .unwrap();
    assert!(matches!(
        service.reserve_execution(7, &activity.id, &attempt(), "job-1", None),
        Err(ActivityError::Database(_))
    ));
    assert_eq!(
        service.execution_limits(7, &activity.id).unwrap(),
        Some(policy)
    );
    let clone = service.clone();
    assert!(std::thread::spawn(move || {
        let _guard = clone.conn.lock().unwrap();
        panic!("poison execution limits connection");
    })
    .join()
    .is_err());
    assert!(matches!(
        service.execution_limits(7, &activity.id),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.set_execution_limits(7, &activity.id, Some(1), limits(5, 10)),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.set_execution_limits_enabled(7, &activity.id, 1, false),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.reserve_execution(7, &activity.id, &attempt(), "job-1", None),
        Err(ActivityError::Poisoned)
    ));
}

fn schema_three_database(path: &Path) -> Vec<Activity> {
    let mut activities = schema_one_database(path);
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(MIGRATE_TO_V2).unwrap();
    conn.execute_batch(MIGRATE_TO_V3).unwrap();
    conn.pragma_update(None, "user_version", 3).unwrap();
    for activity in &mut activities {
        let reference = "app://sample/record?id=legacy";
        activity.resources.push(ActivityResource {
            label: "Legacy object".into(),
            reference: reference.into(),
        });
        conn.execute(
            "UPDATE activities SET resources_json = ?1 WHERE id = ?2",
            params![
                serde_json::to_string(&activity.resources).unwrap(),
                activity.id
            ],
        )
        .unwrap();
        let report = receipt_report();
        conn.execute(
            "INSERT INTO activity_receipts (
                id, activity_id, owner_uid, received_at, source, report_json, declaration_error
             ) VALUES (?1, ?2, ?3, '2026-01-03T00:00:00.000000000Z', 'caller_reported', ?4, 'Legacy unverified report')",
            params![report.id, activity.id, i64::from(activity.owner_uid), serde_json::to_string(&report).unwrap()],
        ).unwrap();
        let draft = ObjectStateDraft {
            id: attempt(),
            reference: reference.into(),
            content: ObjectStateContent::AppReport {
                receipt_id: report.id.clone(),
            },
            observed_at: None,
            valid_until: None,
            supersedes: None,
        };
        conn.execute(
            "INSERT INTO activity_object_state (
                id, activity_id, owner_uid, reference, recorded_at, source, draft_json, receipt_id
             ) VALUES (?1, ?2, ?3, ?4, '2026-01-03T00:00:00.000000000Z', 'caller_reported', ?5, ?6)",
            params![
                draft.id, activity.id, i64::from(activity.owner_uid), reference,
                serde_json::to_string(&draft).unwrap(), report.id,
            ],
        ).unwrap();
    }
    activities
}

#[test]
fn migration_from_schema_three_preserves_every_goal_receipt_and_object_state_field() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let activities = schema_three_database(&path);
    let tables = ["activities", "activity_receipts", "activity_object_state"];
    let before: Vec<_> = {
        let conn = Connection::open(&path).unwrap();
        tables
            .iter()
            .map(|table| stored_rows(&conn, table))
            .collect()
    };
    let service = SqliteActivityService::open(&path).unwrap();
    assert_eq!(SCHEMA_VERSION, 1);
    assert_eq!(DATABASE_SCHEMA_VERSION, 4);
    {
        let conn = service.lock().unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            4
        );
        for (table, original) in tables.iter().zip(&before) {
            assert_eq!(stored_rows(&conn, table), *original);
        }
        validate_schema(&conn).unwrap();
    }
    for activity in activities {
        assert_eq!(
            service.get(activity.owner_uid, &activity.id).unwrap(),
            activity
        );
        assert_eq!(
            service
                .receipts(activity.owner_uid, &activity.id, 100)
                .unwrap()
                .len(),
            1
        );
        let objects = service
            .object_state(activity.owner_uid, &activity.id, None, 100)
            .unwrap();
        assert_eq!(objects.len(), 1);
        assert!(objects[0].receipt.is_some());
        assert!(service
            .execution_limits(activity.owner_uid, &activity.id)
            .unwrap()
            .is_none());
        assert!(service
            .reserve_execution(activity.owner_uid, &activity.id, &attempt(), "job-1", None)
            .unwrap()
            .is_none());
        assert_eq!(
            service.get(activity.owner_uid, &activity.id).unwrap(),
            activity
        );
    }
    let (new, _) = configured(&service, 7, 1, 10);
    service
        .reserve_execution(7, &new.id, &attempt(), "job-1", None)
        .unwrap();
    drop(service);
    let reopened = SqliteActivityService::open(&path).unwrap();
    assert_eq!(
        reopened
            .execution_limits(7, &new.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        1
    );
}

#[test]
fn schema_four_creation_failure_rolls_back_policy_schema_and_keeps_schema_three_unchanged() {
    let directory = TestDirectory::new();
    let path = directory.database();
    schema_three_database(&path);
    let tables = ["activities", "activity_receipts", "activity_object_state"];
    let before: Vec<_> = {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE activity_execution_reservations (sentinel TEXT);
             INSERT INTO activity_execution_reservations VALUES ('preserve');",
        )
        .unwrap();
        tables
            .iter()
            .map(|table| stored_rows(&conn, table))
            .collect()
    };
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Database(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert!(!exists(&conn, "activity_execution_limits"));
    assert!(!exists(&conn, "activity_execution_reservations_activity"));
    assert_eq!(
        conn.query_row(
            "SELECT sentinel FROM activity_execution_reservations",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "preserve"
    );
    for (table, original) in tables.iter().zip(&before) {
        assert_eq!(stored_rows(&conn, table), *original);
    }
}

#[test]
fn migration_retains_legacy_integrity_foreign_key_and_metadata_checks_before_advancing_version() {
    for corrupt_sql in [
        "UPDATE activities SET title = ' untrimmed '",
        "UPDATE activity_receipts SET report_json = '{}'",
        "UPDATE activity_object_state SET owner_uid = 999",
    ] {
        let directory = TestDirectory::new();
        let path = directory.database();
        schema_three_database(&path);
        let tables = ["activities", "activity_receipts", "activity_object_state"];
        let before: Vec<_> = {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", false).unwrap();
            conn.execute_batch(corrupt_sql).unwrap();
            tables
                .iter()
                .map(|table| stored_rows(&conn, table))
                .collect()
        };
        assert!(
            matches!(
                SqliteActivityService::open(&path),
                Err(ActivityError::Corrupt(_))
            ),
            "{corrupt_sql}"
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert!(!exists(&conn, "activity_execution_limits"));
        assert!(!exists(&conn, "activity_execution_reservations"));
        for (table, original) in tables.iter().zip(&before) {
            assert_eq!(stored_rows(&conn, table), *original);
        }
    }
}

#[test]
fn foreign_keys_enforce_activity_policy_ownership_and_reopen_detects_corrupt_edges() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let activity = {
        let service = SqliteActivityService::open(&path).unwrap();
        let (activity, _) = configured(&service, 7, 5, 10);
        service
            .reserve_execution(7, &activity.id, &attempt(), "job-1", None)
            .unwrap();
        let conn = service.lock().unwrap();
        assert!(conn
            .execute("UPDATE activity_execution_limits SET owner_uid = 0", [])
            .is_err());
        assert!(conn
            .execute(
                "UPDATE activity_execution_reservations SET owner_uid = 0",
                []
            )
            .is_err());
        assert!(conn
            .execute("DELETE FROM activity_execution_limits", [])
            .is_err());
        activity
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute(
            "UPDATE activity_execution_reservations SET owner_uid = 0",
            [],
        )
        .unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Corrupt(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(load_activity(&conn, 7, &activity.id).unwrap(), activity);
    assert_eq!(
        conn.query_row(
            "SELECT owner_uid FROM activity_execution_reservations",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
}

#[test]
fn schema_four_requires_owner_scoped_unique_ids_and_composite_foreign_key_definitions() {
    for ddl in [
        MIGRATE_TO_V4.replace("UNIQUE(owner_uid, id),", ""),
        MIGRATE_TO_V4.replace(
            "FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id),",
            "",
        ),
    ] {
        let directory = TestDirectory::new();
        let path = directory.database();
        schema_three_database(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&ddl).unwrap();
            conn.pragma_update(None, "user_version", 4).unwrap();
        }
        assert!(matches!(
            SqliteActivityService::open(&path),
            Err(ActivityError::Corrupt(_))
        ));
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            4
        );
    }
}
