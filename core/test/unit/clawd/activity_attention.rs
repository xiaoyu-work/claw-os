use super::*;

use crate::activities::{Activity, ActivityDraft, ActivityState};
use crate::agent::service::FinishOutcome;
use crate::clawd::protocol::BrokerErrorKind;
use crate::notifications::{NotificationDraft, NotificationState, Severity};
use crate::test_env::TestEnvVarGuard;
use std::time::{SystemTime, UNIX_EPOCH};

const OWNER: u32 = 1000;

struct Sandbox {
    _data: TestEnvVarGuard,
    _caps: TestEnvVarGuard,
    _dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn sandbox() -> Sandbox {
    let lock = crate::test_env::lock_env();
    let dir = tempfile::tempdir().unwrap();
    Sandbox {
        _data: TestEnvVarGuard::set("COS_DATA_DIR", dir.path().join("data")),
        _caps: TestEnvVarGuard::set("COS_CAPS_DATA_DIR", dir.path().join("caps")),
        _dir: dir,
        _lock: lock,
    }
}

fn peer(uid: u32) -> ClientIdentity {
    ClientIdentity {
        uid: Some(uid),
        ..ClientIdentity::unknown()
    }
}

fn activity(owner_uid: u32) -> Activity {
    crate::activities::open_default()
        .unwrap()
        .create(
            owner_uid,
            ActivityDraft {
                title: "Release".into(),
                goal: "Prepare the release".into(),
                completion_criteria: String::new(),
                boundaries: String::new(),
                resources: Vec::new(),
            },
        )
        .unwrap()
}

fn claimed_activity_job(activity: &Activity) -> (Store, Job) {
    let store = Store::open_default().unwrap();
    let session = crate::session::create("Activity attention test")
        .unwrap()
        .to_string();
    store
        .submit_with_activity(
            "Prepare the release".into(),
            None,
            None,
            Some(session),
            None,
            false,
            Some(activity.owner_uid),
            None,
            Some(activity.id.clone()),
        )
        .unwrap();
    let job = store.claim_one().unwrap().unwrap();
    (store, job)
}

fn approval(job: &Job, reason: &str) -> String {
    let session = job.session_id.as_deref().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let execution = approvals::ApprovalExecutionBinding::for_worker(
        job.id.clone(),
        job.worker_pid.unwrap(),
        job.worker_start_time_ticks,
        "activity-attention-test-nonce",
        now + 60,
        Some(OWNER),
        session,
    )
    .unwrap();
    approvals::submit_owned_with_execution(
        Verb::FS_WRITE,
        Scope::path("/tmp/release"),
        session,
        reason,
        Some("Activity agent".into()),
        Some(OWNER),
        Some(crate::caps::ConsentContext::Attended),
        Some(execution),
    )
    .unwrap()
}

fn wait_for_approvals(store: &Store, job: Job, ids: Vec<String>) -> Job {
    store
        .finish(job, FinishOutcome::WaitingApproval { request_ids: ids })
        .unwrap()
}

#[test]
fn attention_is_one_owner_scoped_read_projection_with_explicit_empty_counts() {
    let _sandbox = sandbox();
    let activity = activity(OWNER);
    let view = get(json!({"id": activity.id}), &peer(OWNER)).unwrap();
    assert_eq!(view["schema"], 1);
    assert_eq!(view["activity_id"], activity.id);
    assert_eq!(view["activity_state"], "active");
    assert_eq!(view["limit"], 50);
    assert_eq!(
        view["counts"],
        json!({
            "queued":0, "running":0, "waiting":0, "completed":0,
            "failed":0, "cancelled":0, "indeterminate":0,
            "pending_decisions":0, "unavailable_decisions":0, "unread_notifications":0,
        })
    );
    assert_eq!(view["decisions"], json!([]));
    assert_eq!(view["issues"], json!([]));
    assert_eq!(view["notifications"], json!([]));
    assert_eq!(
        view["totals"],
        json!({"decisions":0,"issues":0,"notifications":0})
    );
    assert_eq!(
        view["has_more"],
        json!({"decisions":false,"issues":false,"notifications":false})
    );
    assert!(!crate::paths::notifications_db_path().exists());
}

#[test]
fn attention_projects_pending_and_recorded_decisions_without_deciding_them() {
    let _sandbox = sandbox();
    let activity = activity(OWNER);
    let (store, claimed) = claimed_activity_job(&activity);
    let id = approval(&claimed, "Write the reviewed release notes");
    let waiting = wait_for_approvals(&store, claimed, vec![id.clone()]);

    let pending = get(json!({"id":activity.id}), &peer(OWNER)).unwrap();
    assert_eq!(pending["counts"]["waiting"], 1);
    assert_eq!(pending["counts"]["pending_decisions"], 1);
    assert_eq!(pending["decisions"][0]["id"], id);
    assert_eq!(pending["decisions"][0]["status"], "pending");
    assert_eq!(pending["decisions"][0]["verb"], "fs.write");
    assert_eq!(
        pending["decisions"][0]["reason"],
        "Write the reviewed release notes"
    );
    assert_eq!(pending["decisions"][0]["review_id"], id);
    assert_eq!(pending["issues"][0]["kind"], "waiting_approval");
    assert_eq!(
        approvals::status_for_owner(&id, Some(OWNER)),
        approvals::RequestStatus::Pending
    );
    assert_eq!(
        store.locate(&waiting.id).unwrap().unwrap().1.status,
        JobStatus::WaitingApproval
    );

    approvals::approve_for_owner(
        &id,
        approvals::GrantDuration::Once,
        Some("owner".into()),
        None,
        Some(OWNER),
    )
    .unwrap();
    assert_eq!(store.reconcile_waiting_approvals().unwrap(), (1, 0));
    let recorded = get(json!({"id":activity.id}), &peer(OWNER)).unwrap();
    assert_eq!(recorded["counts"]["queued"], 1);
    assert_eq!(recorded["counts"]["pending_decisions"], 0);
    assert_eq!(recorded["decisions"][0]["status"], "approved");
    assert_eq!(recorded["decisions"][0]["review_id"], id);
}

#[test]
fn attention_keeps_denials_as_history_and_mismatches_closed() {
    let _sandbox = sandbox();
    let activity = activity(OWNER);
    let (store, claimed) = claimed_activity_job(&activity);
    let id = approval(&claimed, "Write release notes");
    wait_for_approvals(&store, claimed, vec![id.clone()]);
    approvals::deny_for_owner(&id, Some("owner".into()), None, Some(OWNER)).unwrap();
    assert_eq!(store.reconcile_waiting_approvals().unwrap(), (0, 1));

    let view = get(json!({"id":activity.id}), &peer(OWNER)).unwrap();
    assert_eq!(view["counts"]["failed"], 1);
    assert_eq!(view["decisions"][0]["status"], "denied");
    assert_eq!(view["issues"][0]["kind"], "failed");

    let mut mismatched = store
        .locate(&view["issues"][0]["job_id"].as_str().unwrap())
        .unwrap()
        .unwrap()
        .1;
    mismatched.id = uuid::Uuid::new_v4().to_string();
    let hidden = serde_json::to_value(read_decision(OWNER, &mismatched, &id)).unwrap();
    assert_eq!(hidden["status"], "unavailable");
    for field in [
        "requested_at",
        "verb",
        "scope",
        "risk",
        "reason",
        "review_id",
    ] {
        assert!(hidden[field].is_null(), "{field} must not leak");
    }
    assert_eq!(
        hidden["error"],
        "The request is not linked to this Activity task."
    );
}

#[test]
fn attention_limits_details_after_counts_and_is_read_only() {
    let _sandbox = sandbox();
    let activity = activity(OWNER);
    let (store, claimed) = claimed_activity_job(&activity);
    let first = approval(&claimed, "First decision");
    let second = approval(&claimed, "Second decision");
    let waiting = wait_for_approvals(&store, claimed, vec![first.clone(), second.clone()]);
    let notifications = crate::notifications::open_default().unwrap();
    let mut associated =
        NotificationDraft::new("test", "attention", Severity::Warning, "Check", "Review");
    associated.task_id = Some(waiting.id.clone());
    let associated = notifications.publish(OWNER, associated).unwrap();
    let mut unrelated =
        NotificationDraft::new("test", "unrelated", Severity::Info, "Other", "Other");
    unrelated.task_id = Some(uuid::Uuid::new_v4().to_string());
    notifications.publish(OWNER, unrelated).unwrap();
    let cursor = notifications.cursor(OWNER).unwrap();
    let before = notifications.get(OWNER, &associated.id).unwrap();

    let view = get(json!({"id":activity.id,"limit":1}), &peer(OWNER)).unwrap();
    assert_eq!(view["counts"]["waiting"], 1);
    assert_eq!(view["counts"]["pending_decisions"], 2);
    assert_eq!(view["totals"]["decisions"], 2);
    assert_eq!(view["decisions"].as_array().unwrap().len(), 1);
    assert_eq!(view["has_more"]["decisions"], true);
    assert!(view["totals"]["notifications"].as_u64().unwrap() >= 2);
    assert_eq!(view["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(view["has_more"]["notifications"], true);
    assert_eq!(
        approvals::status_for_owner(&first, Some(OWNER)),
        approvals::RequestStatus::Pending
    );
    assert_eq!(
        approvals::status_for_owner(&second, Some(OWNER)),
        approvals::RequestStatus::Pending
    );
    assert_eq!(
        store.locate(&waiting.id).unwrap().unwrap().1.status,
        JobStatus::WaitingApproval
    );
    assert_eq!(notifications.cursor(OWNER).unwrap(), cursor);
    assert_eq!(notifications.get(OWNER, &associated.id).unwrap(), before);
    assert_eq!(before.state, NotificationState::Unread);
}

#[test]
fn indeterminate_is_an_overlapping_count_and_the_highest_priority_issue() {
    let _sandbox = sandbox();
    let activity = activity(OWNER);
    let (store, mut job) = claimed_activity_job(&activity);
    job.status = JobStatus::Error;
    job.execution_phase = ExecutionPhase::Indeterminate;
    job.error = Some("Execution outcome is unknown".into());
    let mut waiting = job.clone();
    waiting.id = uuid::Uuid::new_v4().to_string();
    waiting.status = JobStatus::WaitingApproval;
    waiting.execution_phase = ExecutionPhase::Unprepared;
    waiting.waiting_on = vec!["ap-missing".into()];
    let (counts, _, mut issues) = project_jobs(OWNER, &[waiting, job]);
    issues.sort_by_key(|issue| issue.kind.priority());
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.waiting, 1);
    assert_eq!(counts.indeterminate, 1);
    assert_eq!(issues[0].kind, IssueKind::Indeterminate);
    assert_eq!(issues[1].kind, IssueKind::WaitingApproval);
    drop(store);
}

#[test]
fn attention_validates_identity_and_shape_before_opening_state() {
    let _sandbox = sandbox();
    assert_eq!(
        get(json!({}), &ClientIdentity::unknown()).unwrap_err().kind,
        BrokerErrorKind::Unauthorized
    );
    let id = uuid::Uuid::new_v4().to_string();
    for params in [
        json!({"id":"invalid"}),
        json!({"id":id,"limit":0}),
        json!({"id":id,"limit":101}),
        json!({"id":id,"owner_uid":OWNER}),
        json!({"id":id,"caps":[]}),
    ] {
        assert!(get(params, &peer(OWNER)).is_err());
    }
    assert!(!crate::paths::data_dir().exists());
}

#[test]
fn foreign_activity_is_not_readable_even_by_root_and_never_opens_its_work() {
    let _sandbox = sandbox();
    let activity = activity(OWNER);
    for uid in [0, OWNER + 1] {
        let foreign = get(json!({"id":activity.id}), &peer(uid)).unwrap_err();
        let missing = get(json!({"id":uuid::Uuid::new_v4()}), &peer(uid)).unwrap_err();
        assert_eq!(foreign.message, missing.message);
    }
    assert!(!crate::paths::agent_jobs_dir().exists());
    assert!(!crate::paths::notifications_db_path().exists());
}

#[test]
fn attention_remains_readable_after_the_goal_ends_without_reopening_it() {
    let _sandbox = sandbox();
    let activity = activity(OWNER);
    let service = crate::activities::open_default().unwrap();
    service
        .transition(
            OWNER,
            &activity.id,
            ActivityState::Completed,
            Some("Owner confirmed".into()),
        )
        .unwrap();
    let view = get(json!({"id":activity.id.to_uppercase()}), &peer(OWNER)).unwrap();
    assert_eq!(view["activity_id"], activity.id);
    assert_eq!(view["activity_state"], "completed");
    assert_eq!(
        service.get(OWNER, &activity.id).unwrap().state,
        ActivityState::Completed
    );
}

#[test]
fn attention_previews_keep_exact_character_limits_and_utf8_boundaries() {
    for limit in [160, MAX_PREVIEW_CHARS] {
        let full = "\u{00e9}".repeat(limit);
        assert_eq!(excerpt(&full, limit), full);
        let clipped = excerpt(&"\u{00e9}".repeat(limit + 1), limit);
        assert_eq!(clipped.chars().count(), limit);
        assert!(clipped.ends_with('\u{2026}'));
    }
}
