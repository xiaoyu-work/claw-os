use super::*;
use crate::activities::{ActivityWorkDraft, ActivityWorkNext};

fn setup(state: &super::super::tests::TestState) -> (crate::activities::Activity, TriggerRule) {
    let activity = state.activity(true);
    state.set_limits(&activity.id, 10);
    let service = crate::activities::open_default().unwrap();
    service
        .change_work(
            state.uid,
            &activity.id,
            None,
            ActivityWorkChange::Configure(ActivityWorkDraft {
                task: "Inspect one source and prepare a report for review.".into(),
                check_interval_secs: 3600,
                wake_event: None,
            }),
        )
        .unwrap();
    let result = state
        .command(
            "add",
            &[
                "--id",
                &rule_id(&activity.id),
                "--activity",
                &activity.id,
                "--activity-work",
                "true",
                "--work-revision",
                "1",
                "--prompt",
                "Inspect one source and prepare a report for review.",
                "--source",
                "activity-work-disabled-events",
                "--event-type",
                "none",
            ],
        )
        .unwrap();
    assert_eq!(result["work"]["enabled"], true);
    let rule = binding(state.uid, &activity.id).unwrap().unwrap();
    (activity, rule)
}

fn commit(store: &Store, job: &Job) {
    let prepare = "0123456789abcdef0123456789abcdef";
    let commit = "fedcba9876543210fedcba9876543210";
    let generation = "a".repeat(64);
    store
        .record_execution_prepared(
            &job.id,
            job.worker_pid.unwrap(),
            job.worker_start_time_ticks,
            prepare,
            commit,
            &generation,
        )
        .unwrap();
    store
        .commit_execution(
            &job.id,
            job.worker_pid.unwrap(),
            job.worker_start_time_ticks,
            prepare,
            commit,
            &generation,
        )
        .unwrap();
}

#[test]
fn automatic_activity_work_advances_exactly_once_from_a_durable_handoff() {
    let state = super::super::tests::TestState::new();
    let (activity, _) = setup(&state);
    let service = crate::activities::open_default().unwrap();
    state.command("tick", &[]).unwrap();
    let store = Store::open_default().unwrap();
    let first = store.claim_one().unwrap().unwrap();
    commit(&store, &first);
    let report = crate::activities::ActivityWorkCheckpoint {
        summary: "Inspected the source.".into(),
        made_progress: true,
        completed_steps: vec!["Read source".into()],
        artifacts: Vec::new(),
        next: ActivityWorkNext::Continue {
            task: "Compare the second source without repeating the first.".into(),
        },
    };
    stage_checkpoint(&first, report.clone()).unwrap();
    state.command("tick", &[]).unwrap();
    assert!(store.claim_one().unwrap().is_none());
    store
        .finish(
            first.clone(),
            crate::agent::service::FinishOutcome::Ok {
                response: report.summary,
                turns_used: 1,
                provider: "mock".into(),
                model: "mock".into(),
                evidence: Box::new(None),
                fallback: Box::new(None),
            },
        )
        .unwrap();
    state.command("tick", &[]).unwrap();
    let second = store.claim_one().unwrap().unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(second.prompt, first.prompt);
    let context = second.context.as_deref().unwrap();
    assert!(context.contains("activity_work_checkpoint"));
    assert!(context.contains("Compare the second source"));
    assert!(!second.prompt.contains("Compare the second source"));
    state.command("tick", &[]).unwrap();
    assert!(store.claim_one().unwrap().is_none());
    assert_eq!(
        service
            .work_history(state.uid, &activity.id, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        service.get(state.uid, &activity.id).unwrap().state,
        crate::activities::ActivityState::Active
    );
}

#[test]
fn missing_reserved_job_is_held_instead_of_republished() {
    let state = super::super::tests::TestState::new();
    let (activity, rule) = setup(&state);
    let service = crate::activities::open_default().unwrap();
    let work = service.work(state.uid, &activity.id).unwrap().unwrap();
    service
        .change_work(
            state.uid,
            &activity.id,
            Some(work.revision),
            ActivityWorkChange::Reserve {
                generation: rule.generation.unwrap(),
                job_id: "missing-original".into(),
                session_id: "ses_original".into(),
            },
        )
        .unwrap();
    state.command("tick", &[]).unwrap();
    let work = service.work(state.uid, &activity.id).unwrap().unwrap();
    assert_eq!(work.phase, ActivityWorkPhase::Held);
    assert!(!work.enabled);
    assert!(Store::open_default()
        .unwrap()
        .claim_one()
        .unwrap()
        .is_none());
    assert_eq!(work.last_checkpoint.unwrap().outcome, "indeterminate");
}

#[test]
fn an_existing_owner_event_wakes_a_wait_but_old_events_do_not_replay() {
    let state = super::super::tests::TestState::new();
    let (activity, _) = setup(&state);
    let service = crate::activities::open_default().unwrap();
    let work = service.work(state.uid, &activity.id).unwrap().unwrap();
    let work = service
        .change_work(
            state.uid,
            &activity.id,
            Some(work.revision),
            ActivityWorkChange::Configure(ActivityWorkDraft {
                task: work.config.task.clone(),
                check_interval_secs: 3600,
                wake_event: Some(crate::activities::ActivityWorkEvent {
                    source: "fixture".into(),
                    event_type: "changed".into(),
                }),
            }),
        )
        .unwrap();
    state
        .command(
            "add",
            &[
                "--id",
                &rule_id(&activity.id),
                "--activity",
                &activity.id,
                "--activity-work",
                "true",
                "--work-revision",
                &work.revision.to_string(),
                "--prompt",
                &work.config.task,
                "--source",
                "fixture",
                "--event-type",
                "changed",
            ],
        )
        .unwrap();
    state.event();
    state.command("tick", &[]).unwrap();
    let store = Store::open_default().unwrap();
    let first = store.claim_one().unwrap().unwrap();
    commit(&store, &first);
    stage_checkpoint(
        &first,
        crate::activities::ActivityWorkCheckpoint {
            summary: "Waiting for a new owner event".into(),
            made_progress: false,
            completed_steps: Vec::new(),
            artifacts: Vec::new(),
            next: ActivityWorkNext::Wait {
                task: "Inspect the new change".into(),
                reason: "No newer data yet".into(),
            },
        },
    )
    .unwrap();
    store
        .finish(
            first.clone(),
            crate::agent::service::FinishOutcome::Ok {
                response: "Waiting".into(),
                turns_used: 1,
                provider: "mock".into(),
                model: "mock".into(),
                evidence: Box::new(None),
                fallback: Box::new(None),
            },
        )
        .unwrap();
    state.command("tick", &[]).unwrap();
    assert!(store.claim_one().unwrap().is_none());
    assert_eq!(
        service
            .work(state.uid, &activity.id)
            .unwrap()
            .unwrap()
            .phase,
        ActivityWorkPhase::Waiting
    );
    assert_eq!(service.work(state.uid,&activity.id).unwrap().unwrap().stagnant_segments,0);
    service.transition(state.uid,&activity.id,crate::activities::ActivityState::Paused,None).unwrap();
    state.event();
    state.command("tick",&[]).unwrap();
    assert!(store.claim_one().unwrap().is_none());
    service.transition(state.uid,&activity.id,crate::activities::ActivityState::Active,None).unwrap();
    state.command("tick",&[]).unwrap();
    assert!(store.claim_one().unwrap().is_none(),"paused events must not replay on resume");
    state.append_event(&json!({"source":"fixture","event_type":"changed","client":{"uid":state.uid.wrapping_add(1)}}));
    state.command("tick", &[]).unwrap();
    assert!(store.claim_one().unwrap().is_none());
    state.event();
    state.command("tick", &[]).unwrap();
    assert_ne!(store.claim_one().unwrap().unwrap().id, first.id);
}
