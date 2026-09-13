use super::*;
use crate::activities::ExecutionLimitsDraft;

fn limits(attempts: u32) -> ExecutionLimitsDraft {
    ExecutionLimitsDraft {
        max_attempts: attempts,
        max_turns_per_attempt: 3,
        expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
    }
}

#[test]
fn claim_charges_one_bounded_attempt_and_does_not_block_unrelated_work() {
    let dir = fresh_activity_root();
    let _env = EnvGuard::set(&dir.path().canonicalize().unwrap());
    let service = crate::activities::open_default().unwrap();
    let activity = activity_fixture(1001);
    service
        .set_execution_limits(1001, &activity.id, None, limits(1))
        .unwrap();
    let store = Store::with_root(dir.path().join("jobs")).unwrap();
    let first = submit_activity_fixture(&store, &activity);
    let claimed = store.claim_one().unwrap().unwrap();
    assert_eq!(claimed.id, first.id);
    assert_eq!(claimed.effective_max_turns(), Some(3));
    let reservation = claimed.execution_reservation.as_ref().unwrap();
    assert_eq!(reservation.job_id, first.id);
    assert_eq!(reservation.activity_id, activity.id);
    let (_, events) = store.read_stream_events(&first.id, 0).unwrap();
    assert!(events.iter().any(|event| {
        event["progress"]["kind"] == "activity_execution_reserved"
            && event["progress"]["reservation_id"] == reservation.id
            && event["progress"]["max_turns"] == 3
    }));
    let blocked = submit_activity_fixture(&store, &activity);
    assert!(store.claim_one().unwrap().is_none());
    let ordinary = store
        .submit("unrelated".into(), None, None, None, None)
        .unwrap();
    let next = store.claim_one().unwrap().unwrap();
    assert_eq!(next.id, ordinary.id);
    assert!(next.execution_reservation.is_none());
    assert_eq!(
        store.locate(&blocked.id).unwrap().unwrap().1.status,
        JobStatus::Error
    );
    assert!(store
        .locate(&blocked.id)
        .unwrap()
        .unwrap()
        .1
        .error
        .is_some());
    assert_eq!(
        service
            .execution_limits(1001, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        1
    );
    assert_eq!(
        service.get(1001, &activity.id).unwrap().state,
        ActivityState::Active
    );
}

#[test]
fn recovery_is_a_new_attempt_and_never_refunds_prior_reservations() {
    let dir = fresh_activity_root();
    let _env = EnvGuard::set(&dir.path().canonicalize().unwrap());
    let service = crate::activities::open_default().unwrap();
    let activity = activity_fixture(1001);
    service
        .set_execution_limits(1001, &activity.id, None, limits(2))
        .unwrap();
    let store = Store::with_root(dir.path().join("jobs")).unwrap();
    let job = submit_activity_fixture(&store, &activity);
    let first = store.claim_one().unwrap().unwrap();
    let first_id = first.execution_reservation.unwrap().id;
    store
        .release_for_retry(&job.id, "test recovery")
        .unwrap()
        .unwrap();
    let second = store.claim_one().unwrap().unwrap();
    assert_ne!(second.execution_reservation.unwrap().id, first_id);
    assert_eq!(
        service
            .execution_limits(1001, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        2
    );
    store
        .release_for_retry(&job.id, "second recovery")
        .unwrap()
        .unwrap();
    assert!(store.claim_one().unwrap().is_none());
    assert_eq!(
        store.locate(&job.id).unwrap().unwrap().1.status,
        JobStatus::Error
    );
}

#[test]
fn paused_activity_stays_pending_and_a_disabled_policy_never_means_unlimited() {
    let dir = fresh_activity_root();
    let _env = EnvGuard::set(&dir.path().canonicalize().unwrap());
    let service = crate::activities::open_default().unwrap();
    let activity = activity_fixture(1001);
    let policy = service
        .set_execution_limits(1001, &activity.id, None, limits(2))
        .unwrap();
    let store = Store::with_root(dir.path().join("jobs")).unwrap();
    let job = submit_activity_fixture(&store, &activity);
    service
        .transition(1001, &activity.id, ActivityState::Paused, None)
        .unwrap();
    assert!(store.claim_one().unwrap().is_none());
    assert_eq!(
        store.locate(&job.id).unwrap().unwrap().1.status,
        JobStatus::Pending
    );
    assert_eq!(
        service
            .execution_limits(1001, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        0
    );
    service
        .set_execution_limits_enabled(1001, &activity.id, policy.revision, false)
        .unwrap();
    service
        .transition(1001, &activity.id, ActivityState::Active, None)
        .unwrap();
    assert!(store.claim_one().unwrap().is_none());
    assert_eq!(
        store.locate(&job.id).unwrap().unwrap().1.status,
        JobStatus::Error
    );
    assert_eq!(
        service
            .execution_limits(1001, &activity.id)
            .unwrap()
            .unwrap()
            .used_attempts,
        0
    );
}
