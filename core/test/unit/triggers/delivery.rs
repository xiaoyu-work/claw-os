use super::*;
use crate::activities::{ActivityService, ActivityState};
use crate::triggers::tests::TestState;

fn staged_delivery(
    state: &TestState,
    rule: &TriggerRule,
    automatic: bool,
) -> (ActivityDelivery, Job) {
    let raw = automatic.then(|| state.event());
    let prompt = raw
        .as_deref()
        .map(|raw| fired_prompt(rule, &serde_json::from_str(raw).unwrap()))
        .unwrap_or_else(|| rule.prompt.clone());
    let event = raw.as_deref().map(|raw| (0, raw));
    let mut delivery = ActivityDelivery::new(rule, &prompt, event).unwrap();
    let mut cursor = TriggerCursor {
        in_flight: Some(delivery.clone()),
        ..TriggerCursor::default()
    };
    write_cursor(&cursor).unwrap();
    let mut job = prepare_job(rule, prompt).unwrap();
    job.id = delivery.id.clone();
    delivery.session_id = job.session_id.clone();
    cursor.in_flight = Some(delivery.clone());
    write_cursor(&cursor).unwrap();
    (delivery, job)
}

#[test]
fn idle_nonmatching_and_foreign_events_do_not_create_jobs_or_sessions() {
    let state = TestState::new();
    let activity = state.activity(true);
    state.add("quiet", Some(&activity.id));
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    state.append_event(
        &json!({ "source": "unmatched", "event_type": "changed", "client": { "uid": state.uid } }),
    );
    state.append_event(
        &json!({ "source": "fixture", "event_type": "other", "client": { "uid": state.uid } }),
    );
    state.append_event(&json!({ "source": "fixture", "event_type": "changed", "client": { "uid": state.uid + 1 } }));
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["processed"], 3);
    assert_eq!(tick["fired"], json!([]));
    assert_eq!(tick["pending"], 0);
    assert!(read_cursor().unwrap().in_flight.is_none());
    state.assert_no_work();
}

#[test]
fn observed_inactive_events_are_consumed_and_fresh_events_resume_normally() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("lifecycle", Some(&activity.id));
    let service = crate::activities::open_default().unwrap();
    for activity_state in [
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        service
            .transition(
                state.uid,
                &activity.id,
                activity_state,
                (activity_state == ActivityState::Completed)
                    .then(|| "Owner confirmed completion".to_string()),
            )
            .unwrap();
        state.event();
        let tick = state.command("tick", &[]).unwrap();
        assert_eq!(tick["fired"], json!([]));
        assert_eq!(tick["skipped"][0]["status"], "blocked");
        assert!(tick["skipped"][0]["error"]
            .as_str()
            .unwrap()
            .contains(activity_state.as_str()));
        assert_eq!(tick["pending"], 0);
        assert!(load_rule(&rule.id).unwrap().enabled);
        assert!(state
            .command("run", &[&rule.id])
            .unwrap_err()
            .contains(activity_state.as_str()));
        service
            .transition(state.uid, &activity.id, ActivityState::Active, None)
            .unwrap();
        assert_eq!(
            state.command("tick", &[]).unwrap()["fired"],
            json!([]),
            "resume must not replay blocked events"
        );
        state.assert_no_work();
    }
    state.event();
    assert_eq!(
        state.command("tick", &[]).unwrap()["fired"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(state.jobs().len(), 1);
    assert_eq!(
        load_rule(&rule.id).unwrap().last_delivery.unwrap().status,
        TriggerDeliveryStatus::Submitted
    );
}

#[test]
fn exhausted_limits_block_without_holding_and_an_explicit_increase_allows_only_fresh_events() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("exhausted", Some(&activity.id));
    let service = crate::activities::open_default().unwrap();
    for index in 0..2 {
        service
            .reserve_execution(
                state.uid,
                &activity.id,
                &uuid::Uuid::new_v4().to_string(),
                &format!("charged-{index}"),
                Some(1),
            )
            .unwrap();
    }
    for _ in 0..5 {
        state.event();
    }
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["skipped"].as_array().unwrap().len(), 5);
    assert_eq!(tick["pending"], 0);
    assert!(load_rule(&rule.id).unwrap().enabled);
    assert!(state
        .command("run", &[&rule.id])
        .unwrap_err()
        .contains("attempt limit reached"));
    state.assert_no_work();
    state.set_limits(&activity.id, 3);
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    state.event();
    assert_eq!(
        state.command("tick", &[]).unwrap()["fired"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn simultaneous_ticks_share_one_cursor_and_publish_one_job() {
    let state = TestState::new();
    let activity = state.activity(true);
    state.add("concurrent", Some(&activity.id));
    state.event();
    let barrier = std::sync::Barrier::new(4);
    let outcomes = std::thread::scope(|scope| {
        let handles = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    state.command("tick", &[]).unwrap()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        outcomes
            .iter()
            .map(|value| value["fired"].as_array().unwrap().len())
            .sum::<usize>(),
        1
    );
    assert_eq!(state.jobs().len(), 1);
    assert_eq!(read_cursor().unwrap().next_line, 1);
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
}

#[test]
fn crash_after_delivery_identity_before_session_is_indeterminate_not_retryable() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("crash-gap", Some(&activity.id));
    let raw = state.event();
    let delivery = ActivityDelivery::new(
        &rule,
        &fired_prompt(&rule, &serde_json::from_str(&raw).unwrap()),
        Some((0, &raw)),
    )
    .unwrap();
    write_cursor(&TriggerCursor {
        in_flight: Some(delivery.clone()),
        ..TriggerCursor::default()
    })
    .unwrap();
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["fired"], json!([]));
    assert_eq!(tick["skipped"][0]["status"], "indeterminate");
    assert_eq!(tick["skipped"][0]["job_id"], delivery.id);
    let current = load_rule(&rule.id).unwrap();
    assert!(!current.enabled);
    assert_eq!(
        current.last_delivery.unwrap().status,
        TriggerDeliveryStatus::Indeterminate
    );
    assert!(state
        .command("run", &[&rule.id])
        .unwrap_err()
        .contains("held"));
    state.event();
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    state.assert_no_work();
    state.command("enable", &[&rule.id]).unwrap();
    assert_ne!(load_rule(&rule.id).unwrap().generation, rule.generation);
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    state.event();
    assert_eq!(
        state.command("tick", &[]).unwrap()["fired"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn crash_after_session_correlation_without_a_job_never_replays_submission() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("prepared-gap", Some(&activity.id));
    let (delivery, _) = staged_delivery(&state, &rule, true);
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["fired"], json!([]));
    assert_eq!(tick["skipped"][0]["status"], "indeterminate");
    assert_eq!(
        tick["skipped"][0]["session_id"],
        delivery.session_id.unwrap()
    );
    assert!(state.jobs().is_empty());
    assert_eq!(
        crate::session::list().unwrap().len(),
        1,
        "the evidence Session remains available to inspect"
    );
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
}

#[test]
fn crash_after_job_publication_recovers_the_existing_owned_activity_job() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("published-gap", Some(&activity.id));
    let (delivery, job) = staged_delivery(&state, &rule, true);
    Store::open_default().unwrap().publish(job).unwrap();
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["fired"].as_array().unwrap().len(), 1);
    assert_eq!(tick["fired"][0]["job_id"], delivery.id);
    assert_eq!(tick["fired"][0]["recovered"], true);
    assert_eq!(tick["fired"][0]["status"], "recovered");
    assert_eq!(state.jobs().len(), 1);
    assert_eq!(crate::session::list().unwrap().len(), 1);
    assert!(read_cursor().unwrap().in_flight.is_none());
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    assert_eq!(
        load_rule(&rule.id).unwrap().last_delivery.unwrap().status,
        TriggerDeliveryStatus::Recovered
    );
}

#[test]
fn repeated_manual_run_recovers_its_unacknowledged_job_instead_of_creating_another() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("manual-gap", Some(&activity.id));
    let (delivery, job) = staged_delivery(&state, &rule, false);
    Store::open_default().unwrap().publish(job).unwrap();
    let response = state.command("run", &[&rule.id]).unwrap();
    assert_eq!(response["job_id"], delivery.id);
    assert_eq!(response["activity_id"], activity.id);
    assert_eq!(response["recovered"], true);
    assert_eq!(response["ok"], true);
    assert_eq!(state.jobs().len(), 1);
    assert_eq!(read_cursor().unwrap().next_line, 0);
}

#[test]
fn recovery_is_observation_not_new_admission_after_activity_pause() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("paused-recovery", Some(&activity.id));
    let (delivery, job) = staged_delivery(&state, &rule, true);
    let store = Store::open_default().unwrap();
    store.publish(job).unwrap();
    store
        .cancel_pending_for_owner(&delivery.id, Some(state.uid))
        .unwrap()
        .unwrap();
    crate::activities::open_default()
        .unwrap()
        .transition(state.uid, &activity.id, ActivityState::Paused, None)
        .unwrap();
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["fired"][0]["job_id"], delivery.id);
    assert_eq!(tick["fired"][0]["recovered"], true);
    assert_eq!(state.jobs().len(), 1);
    assert_eq!(
        state.jobs()[0].status,
        crate::agent::service::JobStatus::Cancelled
    );
    assert!(load_rule(&rule.id).unwrap().enabled);
}

#[test]
fn conflicting_job_evidence_is_indeterminate_and_never_false_success() {
    for field in ["owner_uid", "activity_id", "prompt", "session_id", "client"] {
        let state = TestState::new();
        let activity = state.activity(true);
        let rule = state.add("conflicting", Some(&activity.id));
        let (delivery, job) = staged_delivery(&state, &rule, true);
        Store::open_default().unwrap().publish(job).unwrap();
        let path = crate::paths::agent_jobs_dir()
            .join("pending")
            .join(format!("{}.json", delivery.id));
        let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value[field] = match field {
            "owner_uid" => json!(state.uid + 1),
            "activity_id" => json!(uuid::Uuid::new_v4().to_string()),
            "prompt" => json!("a different prompt"),
            "session_id" => json!(crate::session::SessionId::generate()),
            "client" => json!({ "source": "local-cli", "attended": true, "local": true }),
            _ => unreachable!(),
        };
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let tick = state.command("tick", &[]).unwrap();
        assert_eq!(tick["fired"], json!([]), "{field}");
        assert_eq!(tick["skipped"][0]["status"], "indeterminate", "{field}");
        assert!(!load_rule(&rule.id).unwrap().enabled);
        assert_eq!(state.jobs().len(), 1, "{field}");
        assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    }
}

#[test]
fn pending_recovery_rechecks_event_visibility_even_if_a_job_with_the_id_exists() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("visibility", Some(&activity.id));
    let (_, job) = staged_delivery(&state, &rule, true);
    Store::open_default().unwrap().publish(job).unwrap();
    let mut cursor = read_cursor().unwrap();
    let pending = cursor.in_flight.as_mut().unwrap();
    let mut event: Value = serde_json::from_str(pending.raw_event.as_ref().unwrap()).unwrap();
    event["client"]["uid"] = json!(state.uid + 1);
    pending.raw_event = Some(event.to_string());
    write_cursor(&cursor).unwrap();
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["fired"], json!([]));
    assert_eq!(tick["skipped"][0]["status"], "indeterminate");
    assert!(tick["skipped"][0]["error"]
        .as_str()
        .unwrap()
        .contains("visible"));
    assert_eq!(state.jobs().len(), 1);
}

#[test]
fn disable_rearm_and_remove_recreate_do_not_inherit_old_delivery() {
    for remove in [false, true] {
        let state = TestState::new();
        let activity = state.activity(true);
        let rule = state.add("replacement", Some(&activity.id));
        staged_delivery(&state, &rule, true);
        if remove {
            state.command("remove", &[&rule.id]).unwrap();
            state.add(&rule.id, Some(&activity.id));
        } else {
            state.command("disable", &[&rule.id]).unwrap();
            state.command("enable", &[&rule.id]).unwrap();
        }
        assert_ne!(load_rule(&rule.id).unwrap().generation, rule.generation);
        let tick = state.command("tick", &[]).unwrap();
        assert_eq!(tick["fired"], json!([]));
        assert_eq!(tick["skipped"][0]["status"], "skipped");
        assert!(load_rule(&rule.id).unwrap().enabled);
        assert!(state.jobs().is_empty());
        state.event();
        assert_eq!(
            state.command("tick", &[]).unwrap()["fired"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(state.jobs().len(), 1);
    }
}

#[test]
fn unreadable_or_corrupt_cursor_refuses_work_instead_of_resetting_progress() {
    for directory in [false, true] {
        let state = TestState::new();
        let activity = state.activity(true);
        state.add("bad-cursor", Some(&activity.id));
        state.event();
        if directory {
            fs::remove_file(cursor_path()).unwrap();
            fs::create_dir(cursor_path()).unwrap();
        } else {
            fs::write(cursor_path(), "{corrupt").unwrap();
        }
        assert!(state.command("tick", &[]).unwrap_err().contains("cursor"));
        state.assert_no_work();
    }
}

#[test]
fn storage_error_during_recovery_holds_the_rule_without_republishing() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("unreadable-job", Some(&activity.id));
    let (delivery, _) = staged_delivery(&state, &rule, true);
    let store = Store::open_default().unwrap();
    fs::create_dir(
        store
            .root()
            .join("pending")
            .join(format!("{}.json", delivery.id)),
    )
    .unwrap();
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["fired"], json!([]));
    assert_eq!(tick["skipped"][0]["status"], "indeterminate");
    assert!(!load_rule(&rule.id).unwrap().enabled);
    assert!(state.jobs().is_empty());
}

#[test]
fn legacy_numeric_cursor_and_pending_json_remain_readable_without_activity_storage() {
    let state = TestState::new();
    let rule = state.add("legacy", None);
    let raw = state.event();
    crate::filelock::write_locked(&cursor_path(), "1").unwrap();
    assert_eq!(read_cursor().unwrap().next_line, 1);
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    crate::filelock::write_locked(&cursor_path(), &json!({
        "next_line": 1,
        "pending": [{ "line_index": 0, "rule_id": rule.id, "raw_event": raw, "attempts": 1, "last_error": "fixture failure" }]
    }).to_string()).unwrap();
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["fired"][0]["retried"], true);
    assert_eq!(tick["pending"], 0);
    assert!(!crate::paths::data_dir().join("activities.db").exists());
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
}

#[test]
fn legacy_pending_activity_without_correlation_is_held_not_automatically_replayed() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("uncorrelated", Some(&activity.id));
    let raw = state.event();
    crate::filelock::write_locked(
        &cursor_path(),
        &json!({
            "next_line": 1,
            "pending": [{ "line_index": 0, "rule_id": rule.id, "raw_event": raw }]
        })
        .to_string(),
    )
    .unwrap();
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["fired"], json!([]));
    assert_eq!(tick["skipped"][0]["status"], "indeterminate");
    assert_eq!(tick["pending"], 0);
    assert!(!load_rule(&rule.id).unwrap().enabled);
    state.assert_no_work();
}

#[test]
fn legacy_retry_rechecks_generation_owner_visibility_and_current_match() {
    for mismatch in ["generation", "owner", "visibility", "match", "disabled"] {
        let state = TestState::new();
        let rule = state.add("legacy-retry", None);
        let mut raw: Value = serde_json::from_str(&state.event()).unwrap();
        if mismatch == "visibility" {
            raw["client"]["uid"] = json!(state.uid + 1);
        } else if mismatch == "match" {
            raw["source"] = json!("other");
        }
        let pending = PendingDelivery {
            line_index: 0,
            rule_id: rule.id.clone(),
            raw_event: raw.to_string(),
            attempts: 1,
            last_error: None,
            owner_uid: Some(if mismatch == "owner" {
                state.uid + 1
            } else {
                state.uid
            }),
            generation: Some(if mismatch == "generation" {
                uuid::Uuid::new_v4().to_string()
            } else {
                rule.generation.unwrap()
            }),
        };
        write_cursor(&TriggerCursor {
            next_line: 1,
            pending: vec![pending],
            ..TriggerCursor::default()
        })
        .unwrap();
        if mismatch == "disabled" {
            state.command("disable", &[&rule.id]).unwrap();
        }
        let tick = state.command("tick", &[]).unwrap();
        assert_eq!(tick["fired"], json!([]), "{mismatch}");
        assert_eq!(tick["pending"], 0, "{mismatch}");
        state.assert_no_work();
    }
}

#[test]
fn oversized_activity_event_is_skipped_without_persisting_an_unbounded_backlog() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("bounded-event", Some(&activity.id));
    state.append_event(&json!({
        "source": "fixture", "event_type": "changed", "client": { "uid": state.uid },
        "payload": { "oversized": "x".repeat(64 * 1024) },
    }));
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["fired"], json!([]));
    assert_eq!(tick["skipped"][0]["status"], "blocked");
    assert!(tick["skipped"][0]["error"]
        .as_str()
        .unwrap()
        .contains("64 KiB"));
    assert_eq!(tick["pending"], 0);
    assert!(read_cursor().unwrap().in_flight.is_none());
    assert!(load_rule(&rule.id).unwrap().enabled);
    state.assert_no_work();
}

#[test]
fn publication_errors_hold_unknown_outcomes_and_recover_only_matching_visible_jobs() {
    for (failpoint, published) in [("rename", false), ("after_rename", true)] {
        let state = TestState::new();
        let activity = state.activity(true);
        let rule = state.add("publish-error", Some(&activity.id));
        Store::open_default().unwrap();
        state.event();
        std::env::set_var("COS_TEST_PERSISTENCE_FAILPOINT", failpoint);
        let result = state.command("tick", &[]);
        std::env::remove_var("COS_TEST_PERSISTENCE_FAILPOINT");
        let tick = result.unwrap();
        if published {
            assert_eq!(tick["fired"][0]["recovered"], true);
            assert_eq!(tick["fired"][0]["status"], "recovered");
            assert_eq!(state.jobs().len(), 1);
            assert!(load_rule(&rule.id).unwrap().enabled);
        } else {
            assert_eq!(tick["fired"], json!([]));
            assert_eq!(tick["skipped"][0]["status"], "indeterminate");
            assert!(tick["skipped"][0]["error"]
                .as_str()
                .unwrap()
                .contains("rename"));
            assert!(state.jobs().is_empty());
            assert!(!load_rule(&rule.id).unwrap().enabled);
        }
        assert_eq!(tick["pending"], 0);
        assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    }
}

fn assert_missing_activity_progress_holds(state: &TestState, rule: &TriggerRule) {
    let job_ids = state
        .jobs()
        .into_iter()
        .map(|job| job.id)
        .collect::<Vec<_>>();
    let session_count = crate::session::list().unwrap().len();
    let stored_rule = fs::read(rule_path(&rule.id)).unwrap();
    let activity_id = rule.activity_id.as_deref().unwrap();
    let add_args = [
        "--id",
        "held-copy",
        "--prompt",
        "Inspect",
        "--activity",
        activity_id,
    ];
    let audit = fs::read(crate::paths::caps_audit_log_path()).unwrap();
    for (command, args) in [
        (
            "add",
            add_args
                .iter()
                .map(|arg| arg.to_string())
                .collect::<Vec<_>>(),
        ),
        ("run", vec![rule.id.clone()]),
        ("enable", vec![rule.id.clone()]),
    ] {
        let error = preflight_activity_command(state.uid, command, &args).unwrap_err();
        assert!(
            error.contains("cursor is missing") && error.contains("indeterminate"),
            "{error}"
        );
    }
    assert_eq!(
        fs::read(crate::paths::caps_audit_log_path()).unwrap(),
        audit
    );
    for _ in 0..2 {
        for (command, args) in [
            ("tick", Vec::new()),
            ("run", vec![rule.id.as_str()]),
            ("enable", vec![rule.id.as_str()]),
            ("add", add_args.to_vec()),
        ] {
            let error = state.command(command, &args).unwrap_err();
            assert!(
                error.contains("cursor is missing") && error.contains("indeterminate"),
                "{error}"
            );
            assert!(
                error.contains("Restore the original durable cursor"),
                "{error}"
            );
            assert!(
                !cursor_path().exists(),
                "{command} must not recreate ambiguous progress"
            );
        }
    }
    assert!(!rule_path("held-copy").exists());
    assert_eq!(fs::read(rule_path(&rule.id)).unwrap(), stored_rule);
    assert_eq!(
        state
            .jobs()
            .into_iter()
            .map(|job| job.id)
            .collect::<Vec<_>>(),
        job_ids
    );
    assert_eq!(crate::session::list().unwrap().len(), session_count);
    assert_eq!(
        state.command("list", &["--activity", activity_id]).unwrap()["count"],
        1
    );
}

#[test]
fn initial_activity_rule_creation_persists_progress_before_manual_dispatch() {
    let state = TestState::new();
    let activity = state.activity(true);
    assert!(!cursor_path().exists());
    let rule = state.add("initial-progress", Some(&activity.id));
    assert!(cursor_path().is_file());
    assert!(activity_cursor_marker_path().is_file());
    let cursor = read_cursor().unwrap();
    assert_eq!(cursor.next_line, 0);
    assert!(cursor.pending.is_empty() && cursor.in_flight.is_none());
    state.assert_no_work();
    let response = state.command("run", &[&rule.id]).unwrap();
    assert_eq!(response["status"], "submitted");
    assert_eq!(state.jobs().len(), 1);
    assert_eq!(crate::session::list().unwrap().len(), 1);
    assert_eq!(read_cursor().unwrap().next_line, 0);
}

#[test]
fn first_activity_rule_preserves_existing_legacy_cursor_position() {
    let state = TestState::new();
    let activity = state.activity(true);
    state.add("legacy-progress", None);
    assert!(!activity_cursor_marker_path().exists());
    state.event();
    crate::filelock::write_locked(&cursor_path(), "1").unwrap();
    state.add("associated-progress", Some(&activity.id));
    assert_eq!(read_cursor().unwrap().next_line, 1);
    assert!(activity_cursor_marker_path().is_file());
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    state.assert_no_work();
}

#[test]
fn initial_activity_rule_is_not_published_with_invalid_cursor_storage() {
    let state = TestState::new();
    let activity = state.activity(true);
    fs::create_dir_all(cursor_path()).unwrap();
    let error = state
        .command(
            "add",
            &[
                "--id",
                "unpublished-progress",
                "--prompt",
                "Inspect",
                "--activity",
                &activity.id,
            ],
        )
        .unwrap_err();
    assert!(error.contains("cursor"), "{error}");
    assert!(!rule_path("unpublished-progress").exists());
    assert!(!activity_cursor_marker_path().exists());
    state.assert_no_work();
}

#[test]
fn cursor_initialization_requires_directory_sync_before_recording_the_marker() {
    let state = TestState::new();
    let activity = state.activity(true);
    let result = with_trigger_lock(|| {
        std::env::set_var("COS_TEST_PERSISTENCE_FAILPOINT", "dir_fsync");
        let result = initialize_activity_progress();
        std::env::remove_var("COS_TEST_PERSISTENCE_FAILPOINT");
        result
    });
    assert!(result
        .unwrap_err()
        .contains("sync trigger progress directory"));
    assert!(
        cursor_path().is_file(),
        "visibility alone is not the initialization commit"
    );
    assert!(!activity_cursor_marker_path().exists());
    assert!(!has_activity_rules());
    state.assert_no_work();
    state.add("synced-progress", Some(&activity.id));
    assert!(activity_cursor_marker_path().is_file());
    assert_eq!(read_cursor().unwrap().next_line, 0);
    state.assert_no_work();
}

#[test]
fn missing_cursor_after_publication_holds_until_original_progress_is_restored() {
    for acknowledged in [false, true] {
        let state = TestState::new();
        let activity = state.activity(true);
        let rule = state.add("lost-progress", Some(&activity.id));
        let job_id = if acknowledged {
            state.event();
            state.command("tick", &[]).unwrap()["fired"][0]["job_id"]
                .as_str()
                .unwrap()
                .to_string()
        } else {
            let (delivery, job) = staged_delivery(&state, &rule, true);
            Store::open_default().unwrap().publish(job).unwrap();
            delivery.id
        };
        let original = fs::read_to_string(cursor_path()).unwrap();
        fs::remove_file(cursor_path()).unwrap();
        assert_missing_activity_progress_holds(&state, &rule);
        assert_eq!(state.jobs().len(), 1);
        assert_eq!(crate::session::list().unwrap().len(), 1);

        crate::filelock::write_locked(&cursor_path(), &original).unwrap();
        sync_progress_directory().unwrap();
        let tick = state.command("tick", &[]).unwrap();
        if acknowledged {
            assert_eq!(tick["fired"], json!([]));
        } else {
            assert_eq!(tick["fired"][0]["job_id"], job_id);
            assert_eq!(tick["fired"][0]["recovered"], true);
        }
        assert_eq!(state.jobs().len(), 1);
        assert_eq!(crate::session::list().unwrap().len(), 1);
        assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    }
}

#[test]
fn removal_and_replacement_cannot_erase_missing_activity_progress_history() {
    for remove_before_loss in [false, true] {
        let state = TestState::new();
        let activity = state.activity(true);
        let rule = state.add("replace-progress", Some(&activity.id));
        state.event();
        state.command("tick", &[]).unwrap();
        let original = fs::read_to_string(cursor_path()).unwrap();
        if remove_before_loss {
            state.command("remove", &[&rule.id]).unwrap();
            fs::remove_file(cursor_path()).unwrap();
        } else {
            fs::remove_file(cursor_path()).unwrap();
            state.command("disable", &[&rule.id]).unwrap();
            state.command("remove", &[&rule.id]).unwrap();
        }
        assert!(!has_activity_rules());
        assert!(activity_cursor_marker_path().is_file());
        for _ in 0..2 {
            let error = state
                .command(
                    "add",
                    &[
                        "--id",
                        &rule.id,
                        "--prompt",
                        "Inspect",
                        "--activity",
                        &activity.id,
                    ],
                )
                .unwrap_err();
            assert!(error.contains("cursor is missing"), "{error}");
            assert!(state
                .command("tick", &[])
                .unwrap_err()
                .contains("cursor is missing"));
            assert!(!cursor_path().exists());
            assert!(!rule_path(&rule.id).exists());
        }
        assert_eq!(state.jobs().len(), 1);
        assert_eq!(crate::session::list().unwrap().len(), 1);
        crate::filelock::write_locked(&cursor_path(), &original).unwrap();
        sync_progress_directory().unwrap();
        let replacement = state.add(&rule.id, Some(&activity.id));
        assert_ne!(replacement.generation, rule.generation);
        assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
        assert_eq!(state.jobs().len(), 1);
        state.event();
        assert_eq!(
            state.command("tick", &[]).unwrap()["fired"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(state.jobs().len(), 2);
    }
}

#[test]
fn legacy_associated_rule_without_cursor_is_held_and_retirement_keeps_the_witness() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("legacy-associated-progress", None);
    let rule = update_rule(&rule.id, |mut rule| {
        rule.activity_id = Some(activity.id.clone());
        Ok(rule)
    })
    .unwrap();
    assert!(!activity_cursor_marker_path().exists());
    assert!(!cursor_path().exists());
    state.event();
    assert_missing_activity_progress_holds(&state, &rule);
    state.assert_no_work();
    state.command("disable", &[&rule.id]).unwrap();
    state.command("remove", &[&rule.id]).unwrap();
    assert!(activity_cursor_marker_path().is_file());
    assert!(!cursor_path().exists());
    assert!(state
        .command("tick", &[])
        .unwrap_err()
        .contains("cursor is missing"));
    assert!(!cursor_path().exists());
    state.assert_no_work();
}
