use super::*;
use crate::activities::{
    ActivityDraft, ActivityService, ActivityWorkCheckpoint, ActivityWorkDraft, ExecutionLimitsDraft,
};

fn configured(service: &SqliteActivityService) -> (Activity, ActivityWork) {
    let activity = service
        .create(
            1000,
            ActivityDraft {
                title: "Research".into(),
                goal: "Prepare a researched report".into(),
                completion_criteria: "Owner reviews the report".into(),
                boundaries: String::new(),
                resources: Vec::new(),
            },
        )
        .unwrap();
    service
        .set_execution_limits(
            1000,
            &activity.id,
            None,
            ExecutionLimitsDraft {
                max_attempts: 10,
                max_turns_per_attempt: 5,
                expires_at: "2099-01-01T00:00:00Z".into(),
            },
        )
        .unwrap();
    let work = service
        .change_work(
            1000,
            &activity.id,
            None,
            ActivityWorkChange::Configure(ActivityWorkDraft {
                task: "Find the first source".into(),
                check_interval_secs: 3600,
                wake_event: None,
            }),
        )
        .unwrap();
    (activity, work)
}

fn enable(service: &SqliteActivityService, work: &ActivityWork) -> ActivityWork {
    service
        .change_work(
            1000,
            &work.activity_id,
            Some(work.revision),
            ActivityWorkChange::Enable {
                generation: uuid::Uuid::new_v4().to_string(),
                next_task: None,
                review_note: None,
            },
        )
        .unwrap()
}

fn segment(service: &SqliteActivityService, work: &ActivityWork, progress: bool) -> ActivityWork {
    let job_id = uuid::Uuid::new_v4().to_string();
    let running = service
        .change_work(
            1000,
            &work.activity_id,
            Some(work.revision),
            ActivityWorkChange::Reserve {
                generation: work.generation.clone().unwrap(),
                job_id: job_id.clone(),
                session_id: "ses_work".into(),
            },
        )
        .unwrap();
    let staged = service
        .change_work(
            1000,
            &work.activity_id,
            Some(running.revision),
            ActivityWorkChange::Stage {
                job_id: job_id.clone(),
                checkpoint: ActivityWorkCheckpoint {
                    summary: "A caller-reported handoff".into(),
                    made_progress: progress,
                    completed_steps: Vec::new(),
                    artifacts: Vec::new(),
                    next: ActivityWorkNext::Continue {
                        task: "Read the next source".into(),
                    },
                },
            },
        )
        .unwrap();
    assert_eq!(staged.phase, ActivityWorkPhase::Running);
    assert!(!staged.due_at(Utc::now()).unwrap());
    service
        .change_work(
            1000,
            &work.activity_id,
            Some(staged.revision),
            ActivityWorkChange::Finish {
                job_id,
                outcome: ActivityWorkOutcome::Reported,
                event_after: 10,
            },
        )
        .unwrap()
}

#[test]
fn automatic_work_is_explicit_owner_scoped_and_revision_checked() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, work) = configured(&service);
    assert!(!work.enabled);
    assert!(!work.due_at(Utc::now()).unwrap());
    for owner in [0, 2000] {
        assert!(matches!(
            service.work(owner, &activity.id),
            Err(ActivityError::NotFound)
        ));
    }
    assert!(service
        .change_work(1000, &activity.id, Some(99), ActivityWorkChange::Disable)
        .is_err());
    let enabled = enable(&service, &work);
    assert!(enabled.due_at(Utc::now()).unwrap());
    let next = segment(&service, &enabled, true);
    assert_eq!(next.phase, ActivityWorkPhase::Ready);
    assert_eq!(service.get(1000, &activity.id).unwrap(), activity);
    assert_eq!(
        service.work_history(1000, &activity.id, 10).unwrap().len(),
        1
    );
    assert!(service
        .change_work(
            1000,
            &activity.id,
            Some(next.revision),
            ActivityWorkChange::Stage {
                job_id: "another-job".into(),
                checkpoint: next.last_checkpoint.unwrap().checkpoint.unwrap(),
            }
        )
        .is_err());
}

#[test]
fn repeated_no_progress_requires_explicit_review_and_a_new_task() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, configured) = configured(&service);
    let mut work = enable(&service, &configured);
    for _ in 0..3 {
        work = segment(&service, &work, false);
    }
    assert!(!work.enabled);
    assert_eq!(work.phase, ActivityWorkPhase::Review);
    assert!(service
        .change_work(
            1000,
            &activity.id,
            Some(work.revision),
            ActivityWorkChange::Enable {
                generation: uuid::Uuid::new_v4().to_string(),
                next_task: None,
                review_note: None,
            }
        )
        .is_err());
    let resumed = service
        .change_work(
            1000,
            &activity.id,
            Some(work.revision),
            ActivityWorkChange::Enable {
                generation: uuid::Uuid::new_v4().to_string(),
                next_task: Some("Inspect a different source".into()),
                review_note: Some("Reviewed the stalled attempts".into()),
            },
        )
        .unwrap();
    assert!(resumed.enabled);
    assert_eq!(
        service.get(1000, &activity.id).unwrap().state,
        ActivityState::Active
    );
}

#[test]
fn restart_retains_the_exact_inflight_job_and_indeterminate_work_never_replays() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("activities.db");
    let (activity, running) = {
        let service = SqliteActivityService::open(&path).unwrap();
        let (activity, work) = configured(&service);
        let enabled = enable(&service, &work);
        let running = service
            .change_work(
                1000,
                &activity.id,
                Some(enabled.revision),
                ActivityWorkChange::Reserve {
                    generation: enabled.generation.unwrap(),
                    job_id: "job-original".into(),
                    session_id: "ses_original".into(),
                },
            )
            .unwrap();
        (activity, running)
    };
    let service = SqliteActivityService::open(&path).unwrap();
    assert_eq!(service.work(1000, &activity.id).unwrap().unwrap(), running);
    let held = service
        .change_work(
            1000,
            &activity.id,
            Some(running.revision),
            ActivityWorkChange::Finish {
                job_id: "job-original".into(),
                outcome: ActivityWorkOutcome::Indeterminate(
                    "Execution may have produced effects".into(),
                ),
                event_after: 0,
            },
        )
        .unwrap();
    assert_eq!(held.phase, ActivityWorkPhase::Held);
    assert!(!held.enabled);
    assert!(!held.due_at(Utc::now()).unwrap());
    assert_eq!(
        service.work_history(1000, &activity.id, 10).unwrap()[0].outcome,
        "indeterminate"
    );
}

#[test]
fn schema_eight_upgrade_and_portable_import_never_create_automatic_execution() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("activities.db");
    let (activity, document) = {
        let service = SqliteActivityService::open(&path).unwrap();
        let (activity, work) = configured(&service);
        let before = service.export_continuity(1000, &activity.id).unwrap();
        enable(&service, &work);
        assert_eq!(
            service.export_continuity(1000, &activity.id).unwrap(),
            before
        );
        let imported = service
            .import_continuity(
                2000,
                crate::activities::ActivityExecutionPlacement::Local,
                before.clone(),
            )
            .unwrap();
        assert_eq!(imported.activity.state, ActivityState::Paused);
        assert!(service.work(2000, &imported.activity.id).unwrap().is_none());
        (activity, before)
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE activity_work_history; DROP TABLE activity_work; PRAGMA user_version=8;",
        )
        .unwrap();
    }
    let service = SqliteActivityService::open(&path).unwrap();
    assert_eq!(service.get(1000, &activity.id).unwrap(), activity);
    assert_eq!(
        service.export_continuity(1000, &activity.id).unwrap(),
        document
    );
    assert!(service.work(1000, &activity.id).unwrap().is_none());
}
