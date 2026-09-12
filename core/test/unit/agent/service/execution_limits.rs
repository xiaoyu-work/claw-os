use super::*;
use crate::activities::{ActivityDraft, ExecutionLimitsDraft, SqliteActivityService};
use serde_json::json;

fn fixture() -> (Arc<SqliteActivityService>, Job) {
    let service = Arc::new(SqliteActivityService::open_in_memory().unwrap());
    let activity = service
        .create(
            1000,
            ActivityDraft {
                title: "Bounded goal".into(),
                goal: "Prepare".into(),
                completion_criteria: String::new(),
                boundaries: String::new(),
                resources: Vec::new(),
            },
        )
        .unwrap();
    let expires_at = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    service
        .set_execution_limits(
            1000,
            &activity.id,
            None,
            ExecutionLimitsDraft {
                max_attempts: 3,
                max_turns_per_attempt: 4,
                expires_at,
            },
        )
        .unwrap();
    let mut job = Job::new_pending(
        "Prepare".into(),
        None,
        None,
        None,
        Some(20),
        false,
        Some(1000),
        None,
    );
    job.activity_id = Some(activity.id.clone());
    job.execution_reservation = service
        .reserve_execution(
            1000,
            &activity.id,
            &uuid::Uuid::new_v4().to_string(),
            &job.id,
            job.max_turns,
        )
        .unwrap();
    (service, job)
}

#[test]
fn reserved_turns_are_an_actual_job_ceiling_not_prompt_text() {
    let (_service, mut job) = fixture();
    assert_eq!(job.effective_max_turns(), Some(4));
    job.max_turns = Some(2);
    assert_eq!(job.effective_max_turns(), Some(2));
    job.max_turns = None;
    assert_eq!(job.effective_max_turns(), Some(4));
    job.execution_reservation = None;
    assert_eq!(job.effective_max_turns(), None);
}

#[test]
fn live_policy_revision_disable_and_new_policy_stop_old_attempts() {
    let (service, job) = fixture();
    let activity = job.activity_id.as_deref().unwrap();
    let mut guard = ExecutionLimitsGuard::with_service(&job, Some(service.clone())).unwrap();
    assert!(guard.check_now(&job).unwrap().is_none());
    let policy = service.execution_limits(1000, activity).unwrap().unwrap();
    service
        .set_execution_limits_enabled(1000, activity, policy.revision, false)
        .unwrap();
    assert!(guard.check_now(&job).unwrap().unwrap().contains("disabled"));
    let policy = service.execution_limits(1000, activity).unwrap().unwrap();
    service
        .set_execution_limits_enabled(1000, activity, policy.revision, true)
        .unwrap();
    assert!(guard.check_now(&job).unwrap().unwrap().contains("changed"));
    let mut unreserved = job.clone();
    unreserved.execution_reservation = None;
    assert!(ExecutionLimitsGuard::with_service(&unreserved, Some(service)).is_err());
}

#[test]
fn monotonic_expiry_cannot_be_extended_by_heartbeat_or_wall_clock_checks() {
    let (service, job) = fixture();
    let mut guard = ExecutionLimitsGuard::with_service(&job, Some(service)).unwrap();
    guard.deadline = Some(Instant::now() - Duration::from_secs(1));
    guard.next_policy_check = Instant::now() + Duration::from_secs(60);
    assert!(guard.check(&job).unwrap().unwrap().contains("expired"));
}

#[test]
fn a_reservation_cannot_be_rebound_to_another_job_or_owner() {
    let (service, job) = fixture();
    let mut changed = job.clone();
    changed.id = uuid::Uuid::new_v4().to_string();
    assert!(ExecutionLimitsGuard::with_service(&changed, Some(service.clone())).is_err());
    changed = job;
    changed.owner_uid = Some(2000);
    assert!(ExecutionLimitsGuard::with_service(&changed, Some(service)).is_err());
}

#[test]
fn old_unassociated_job_json_retains_its_existing_behavior() {
    let job: Job = serde_json::from_value(json!({
        "id":uuid::Uuid::new_v4().to_string(),"prompt":"Old job",
        "status":"pending","created_at":"2026-09-11T00:00:00Z","max_turns":7,
    }))
    .unwrap();
    assert!(job.execution_reservation.is_none());
    assert_eq!(job.effective_max_turns(), Some(7));
    let mut guard = ExecutionLimitsGuard::with_service(&job, None).unwrap();
    assert!(guard.check_now(&job).unwrap().is_none());
}
