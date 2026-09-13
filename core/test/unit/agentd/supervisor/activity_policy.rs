use super::*;
use crate::activities::{ActivityService, CapabilityBoundaryDecision as Boundary};
use crate::caps::activity_boundary::{scope as policy_scope, ActivityBoundary};
use serde_json::json;

fn boundary(lease: &Lease, mode: &str) -> (String, Arc<ActivityBoundary>) {
    let service = crate::activities::open_default().unwrap();
    let activity = service
        .create(
            lease.owner_uid,
            serde_json::from_value(json!({
                "title":"Bounded task","goal":"Prepare, do not send"
            }))
            .unwrap(),
        )
        .unwrap();
    let scopes = if mode == "deny" {
        json!([])
    } else {
        json!([{"kind":"path","value":"/home/user/**"}])
    };
    service
        .set_capability_policy(
            lease.owner_uid,
            &activity.id,
            None,
            serde_json::from_value(
                json!({"rules":[{"verb":"fs.read","mode":mode,"scopes":scopes}]}),
            )
            .unwrap(),
        )
        .unwrap();
    let job: Job = serde_json::from_value(json!({
        "schema_version":2,"execution_phase":"preparing",
        "id":lease.task_id,"prompt":"test","status":"running","created_at":"2026-01-01T00:00:00Z",
        "owner_uid":lease.owner_uid,"session_id":lease.session_id,"activity_id":activity.id,
    }))
    .unwrap();
    (
        activity.id,
        ActivityBoundary::for_job(&job).unwrap().unwrap(),
    )
}

#[tokio::test]
async fn denied_boundaries_refuse_direct_consent_requests_without_spending_existing_approvals() {
    let _store = ConsentStore::new();
    let lease = new_lease();
    let resource = crate::caps::Scope::path("/home/user/notes.txt");
    approve_once(&lease, crate::caps::Verb::FS_READ, &resource);
    let (_, boundary) = boundary(&lease, "deny");
    policy_scope(Some(boundary), async {
        let mut used = 0;
        let ask = ApprovalAsk::Boundary {
            verb: "fs.read".into(),
            scope: resource.clone(),
        };
        assert_eq!(
            mediate_approval(&mut used, &lease, &ask),
            ApprovalReply::Boundary {
                decision: Boundary::Deny
            }
        );
        assert!(matches!(
            mediate_approval(&mut used, &lease, &consume_ask(&resource)),
            ApprovalReply::Refused { .. }
        ));
        let ask = ApprovalAsk::Request {
            verb: "fs.read".into(),
            scope: resource.clone(),
            operation_digest: None,
        };
        assert!(matches!(
            mediate_approval(&mut used, &lease, &ask),
            ApprovalReply::Refused { .. }
        ));
        assert!(crate::approvals::list_pending_for_owner(Some(lease.owner_uid)).is_empty());
    })
    .await;
    assert_eq!(
        mediate_approval(&mut 0, &lease, &consume_ask(&resource)),
        ApprovalReply::Granted,
        "denied requests must not have consumed the execution-bound approval",
    );
}

#[tokio::test]
async fn activity_consent_is_exact_single_use_and_cannot_select_another_lease() {
    let _store = ConsentStore::new();
    let lease = new_lease();
    let resource = crate::caps::Scope::path("/home/user/notes.txt");
    approve_once(
        &lease,
        crate::caps::Verb::FS_READ,
        &crate::caps::Scope::path("/home/user/**"),
    );
    let (_, boundary) = boundary(&lease, "require_approval");
    policy_scope(Some(boundary), async {
        let mut used = 0;
        assert_eq!(
            mediate_approval(&mut used, &lease, &consume_ask(&resource)),
            ApprovalReply::Pending { request_id: None }
        );
        let ask = ApprovalAsk::Request {
            verb: "fs.read".into(),
            scope: resource.clone(),
            operation_digest: None,
        };
        let ApprovalReply::Pending {
            request_id: Some(id),
        } = mediate_approval(&mut used, &lease, &ask)
        else {
            panic!("an exact confirmation must be requested");
        };
        let pending = crate::approvals::lookup_pending(&id).unwrap();
        assert_eq!(pending.scope, resource);
        crate::approvals::approve_for_owner(
            &id,
            crate::approvals::GrantDuration::Session,
            Some("test".into()),
            None,
            Some(lease.owner_uid),
        )
        .unwrap();
        for owner in [0, lease.owner_uid + 1] {
            let mut foreign = new_lease();
            foreign.owner_uid = owner;
            assert!(matches!(
                mediate_approval(&mut used, &foreign, &consume_ask(&resource)),
                ApprovalReply::Refused { .. }
            ));
        }
        let mut foreign = new_lease();
        foreign.session_id = Some("another-session".into());
        assert!(matches!(
            mediate_approval(&mut used, &foreign, &consume_ask(&resource)),
            ApprovalReply::Refused { .. }
        ));
        assert_eq!(
            mediate_approval(&mut used, &lease, &consume_ask(&resource)),
            ApprovalReply::Granted
        );
        assert_eq!(
            mediate_approval(&mut used, &lease, &consume_ask(&resource)),
            ApprovalReply::Pending { request_id: None }
        );
    })
    .await;
}

#[tokio::test]
async fn changed_policy_refuses_even_a_direct_consumption_message_and_has_a_separate_bound() {
    let _store = ConsentStore::new();
    let lease = new_lease();
    let resource = crate::caps::Scope::path("/home/user/notes.txt");
    let (id, boundary) = boundary(&lease, "normal");
    policy_scope(Some(boundary), async {
        let ask = ApprovalAsk::Boundary {
            verb: "fs.read".into(),
            scope: resource.clone(),
        };
        let mut used = protocol::MAX_BOUNDARY_CHECKS - 1;
        assert_eq!(
            mediate_approval(&mut used, &lease, &ask),
            ApprovalReply::Boundary {
                decision: Boundary::Normal
            }
        );
        assert!(matches!(
            mediate_approval(&mut used, &lease, &ask),
            ApprovalReply::Refused { .. }
        ));
        crate::activities::open_default()
            .unwrap()
            .set_capability_policy_enabled(lease.owner_uid, &id, 1, false)
            .unwrap();
        assert!(matches!(
            mediate_approval(&mut 0, &lease, &consume_ask(&resource)),
            ApprovalReply::Refused { .. }
        ));
        assert!(crate::approvals::list_pending_for_owner(Some(lease.owner_uid)).is_empty());
    })
    .await;
}

#[test]
fn policy_and_limit_stops_preserve_charged_attempts_and_respect_commit_phase() {
    use crate::agent::service::{
        execution_limits::ExecutionLimitsGuard, ExecutionPhase, JobStatus,
    };
    for committed in [false, true] {
        for stop_policy in [false, true] {
            let fixture = ConsentStore::new();
            let service = crate::activities::open_default().unwrap();
            let activity = service.create(1000, serde_json::from_value(json!({
                "title":"Retained attempt", "goal":"Never refund or complete on a constraint stop"
            })).unwrap()).unwrap();
            service
                .set_capability_policy(
                    1000,
                    &activity.id,
                    None,
                    serde_json::from_value(json!({"rules":[]})).unwrap(),
                )
                .unwrap();
            service
                .set_execution_limits(
                    1000,
                    &activity.id,
                    None,
                    crate::activities::ExecutionLimitsDraft {
                        max_attempts: 2,
                        max_turns_per_attempt: 3,
                        expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                    },
                )
                .unwrap();
            let store = Store::with_root(fixture._dir.path().join("jobs")).unwrap();
            let submitted = store
                .submit_with_activity(
                    "bounded attempt".into(),
                    None,
                    None,
                    Some("session-a".into()),
                    Some(20),
                    false,
                    Some(1000),
                    None,
                    Some(activity.id.clone()),
                )
                .unwrap();
            let claimed = store.claim_one().unwrap().unwrap();
            assert_eq!(claimed.id, submitted.id);
            assert_eq!(claimed.schema_version, 2);
            assert_eq!(claimed.effective_max_turns(), Some(3));
            assert!(claimed.execution_reservation.is_some());
            let boundary = ActivityBoundary::for_job(&claimed).unwrap().unwrap();
            let mut limits = ExecutionLimitsGuard::new(&claimed).unwrap();
            let pid = claimed.worker_pid.unwrap();
            let start = claimed.worker_start_time_ticks;
            let prepare = "0123456789abcdef0123456789abcdef";
            let commit = "fedcba9876543210fedcba9876543210";
            let generation = "aaaaaaaaaaaaaaaa";
            let mut job = store
                .record_execution_prepared(&claimed.id, pid, start, prepare, commit, generation)
                .unwrap();
            if committed {
                job = store
                    .commit_execution(&job.id, pid, start, prepare, commit, generation)
                    .unwrap();
            }
            let reason = if stop_policy {
                service
                    .set_capability_policy_enabled(1000, &activity.id, 1, false)
                    .unwrap();
                boundary.check().unwrap_err()
            } else {
                service
                    .set_execution_limits_enabled(1000, &activity.id, 1, false)
                    .unwrap();
                limits
                    .check_now(&job)
                    .unwrap()
                    .expect("disabled limit must stop the attempt")
            };
            let outcome = if committed {
                post_assignment_interruption(reason, false)
            } else {
                TaskOutcome::Failed(reason)
            };
            finish_task_outcome(&store, &job, outcome);
            let finished = store.locate(&job.id).unwrap().unwrap().1;
            assert_eq!(finished.status, JobStatus::Error);
            assert_eq!(
                finished.execution_phase,
                if committed {
                    ExecutionPhase::Indeterminate
                } else {
                    ExecutionPhase::Prepared
                }
            );
            assert_eq!(finished.recovery_count, 0);
            assert!(store.claim_one().unwrap().is_none());
            assert_eq!(
                service
                    .execution_limits(1000, &activity.id)
                    .unwrap()
                    .unwrap()
                    .used_attempts,
                1
            );
            assert_eq!(
                service.get(1000, &activity.id).unwrap().state,
                crate::activities::ActivityState::Active
            );
            assert!(service
                .receipts(1000, &activity.id, 100)
                .unwrap()
                .is_empty());
        }
    }
}
