use super::*;
use crate::activities::{ActivityState, CapabilityPolicyDraft};
use crate::triggers::tests::TestState;

#[test]
fn invalid_activity_id_is_refused_without_opening_storage() {
    let state = TestState::new();
    assert!(validate_activity_trigger(state.uid, "../not-an-activity")
        .unwrap_err()
        .contains("UUID"));
    assert!(!crate::paths::data_dir().join("activities.db").exists());
    state.assert_no_work();
}

#[test]
fn only_the_owner_of_an_active_bounded_activity_passes_preflight() {
    let state = TestState::new();
    let activity = state.activity(false);
    assert!(validate_activity_trigger(state.uid, &activity.id)
        .unwrap_err()
        .contains("configured finite"));
    assert!(validate_activity_trigger(state.uid + 1, &activity.id)
        .unwrap_err()
        .contains("not found"));
    if state.uid != 0 {
        assert!(validate_activity_trigger(0, &activity.id)
            .unwrap_err()
            .contains("not found"));
    }
    assert!(
        validate_activity_trigger(state.uid, &uuid::Uuid::new_v4().to_string())
            .unwrap_err()
            .contains("not found")
    );
    state.set_limits(&activity.id, 2);
    assert_eq!(
        validate_activity_trigger(state.uid, &activity.id.to_uppercase()).unwrap(),
        activity.id
    );
    state.assert_no_work();
}

#[test]
fn activity_refusals_precede_add_authorization_and_rule_creation() {
    let state = TestState::new();
    let service = crate::activities::open_default().unwrap();
    let unbounded = state.activity(false);
    let foreign = state.activity(true);
    let mut cases = vec![
        (unbounded.id, "configured finite"),
        (foreign.id, "not found"),
    ];
    for activity_state in [
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let activity = state.activity(true);
        service
            .transition(
                state.uid,
                &activity.id,
                activity_state,
                (activity_state == ActivityState::Completed)
                    .then(|| "Confirmed by fixture owner".to_string()),
            )
            .unwrap();
        cases.push((activity.id, activity_state.as_str()));
    }
    for (index, (id, expected)) in cases.into_iter().enumerate() {
        let uid = if expected == "not found" {
            state.uid + 1
        } else {
            state.uid
        };
        let error = if uid != state.uid {
            validate_activity_trigger(uid, &id).unwrap_err()
        } else {
            let args = [
                "--id",
                &format!("invalid-{index}"),
                "--prompt",
                "Inspect",
                "--activity",
                &id,
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
            state
                .with_caps(CapSet::new(), || run("add", &args))
                .unwrap_err()
        };
        assert!(error.contains(expected), "{error}");
    }
    assert!(!triggers_dir().exists());
    assert!(!crate::paths::caps_audit_log_path().exists());
    state.assert_no_work();
}

#[test]
fn missing_capability_policy_is_normal_but_disabling_one_blocks_work() {
    let state = TestState::new();
    let activity = state.activity(true);
    let service = crate::activities::open_default().unwrap();
    assert!(service
        .capability_policy(state.uid, &activity.id)
        .unwrap()
        .is_none());
    assert!(validate_activity_trigger(state.uid, &activity.id).is_ok());
    let policy = service
        .set_capability_policy(
            state.uid,
            &activity.id,
            None,
            CapabilityPolicyDraft { rules: Vec::new() },
        )
        .unwrap();
    assert!(validate_activity_trigger(state.uid, &activity.id).is_ok());
    let rule = state.add("policy-bound", Some(&activity.id));
    service
        .set_capability_policy_enabled(state.uid, &activity.id, policy.revision, false)
        .unwrap();
    state.event();
    let tick = state.command("tick", &[]).unwrap();
    assert_eq!(tick["fired"], json!([]));
    assert_eq!(tick["skipped"][0]["status"], "blocked");
    assert!(tick["skipped"][0]["error"]
        .as_str()
        .unwrap()
        .contains("capability policy is disabled"));
    assert!(
        load_rule(&rule.id).unwrap().enabled,
        "policy revocation must not quarantine the rule"
    );
    assert!(state
        .command("run", &[&rule.id])
        .unwrap_err()
        .contains("capability policy is disabled"));
    state.assert_no_work();
}

#[test]
fn execution_limits_disabled_expired_and_exhausted_are_distinct_refusals() {
    let state = TestState::new();
    let activity = state.activity(true);
    let service = crate::activities::open_default().unwrap();
    let limits = service
        .execution_limits(state.uid, &activity.id)
        .unwrap()
        .unwrap();
    let disabled = service
        .set_execution_limits_enabled(state.uid, &activity.id, limits.revision, false)
        .unwrap();
    assert!(validate_activity_trigger(state.uid, &activity.id)
        .unwrap_err()
        .contains("limits are disabled"));
    service
        .set_execution_limits_enabled(state.uid, &activity.id, disabled.revision, true)
        .unwrap();
    let connection =
        rusqlite::Connection::open(crate::paths::data_dir().join("activities.db")).unwrap();
    connection
        .execute(
            "UPDATE activity_execution_limits SET expires_at = ?1 WHERE activity_id = ?2",
            rusqlite::params![
                (chrono::Utc::now() - chrono::Duration::seconds(1))
                    .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                activity.id
            ],
        )
        .unwrap();
    let error = validate_activity_trigger(state.uid, &activity.id).unwrap_err();
    assert!(error.contains("expired"), "{error}");
    drop(connection);
    state.set_limits(&activity.id, 2);
    for index in 0..2 {
        service
            .reserve_execution(
                state.uid,
                &activity.id,
                &uuid::Uuid::new_v4().to_string(),
                &format!("synthetic-job-{index}"),
                Some(7),
            )
            .unwrap();
    }
    assert!(validate_activity_trigger(state.uid, &activity.id)
        .unwrap_err()
        .contains("attempt limit reached"));
    state.assert_no_work();
}

#[test]
fn association_uses_shared_job_session_contract_and_never_mints_caps_or_reservations() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("associated", Some(&activity.id.to_uppercase()));
    assert_eq!(rule.activity_id.as_deref(), Some(activity.id.as_str()));
    state.event();
    let tick = state.command("tick", &[]).unwrap();
    let job_id = tick["fired"][0]["job_id"].as_str().unwrap();
    let job = Store::open_default()
        .unwrap()
        .locate_for_owner(job_id, Some(state.uid))
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(job.activity_id.as_deref(), Some(activity.id.as_str()));
    assert_eq!(job.owner_uid, Some(state.uid));
    assert_eq!(job.client, trigger_client());
    assert_eq!(
        job.max_turns,
        Some(7),
        "the Root reservation, not this trigger, owns the effective ceiling"
    );
    assert!(job.execution_reservation.is_none());
    assert_eq!(
        job.execution_phase,
        crate::agent::service::ExecutionPhase::Unprepared
    );
    assert!(
        job.context.is_none(),
        "Activity planning context is still supplied by ordinary Root claim"
    );
    let sid = job.session_id.as_ref().unwrap().parse().unwrap();
    let meta = crate::session::get_meta(&sid).unwrap();
    assert_eq!(meta.activity_id, job.activity_id);
    assert_eq!(meta.owner_uid, Some(state.uid));
    assert_eq!(
        meta.origin,
        Some(crate::session::SessionOrigin::TriggerDelegation)
    );
    assert_eq!(meta.client, trigger_client());
    assert_eq!(
        meta.credential_tier,
        Some(Role::AgentHost.credential_tier())
    );
    let caps = crate::session::get_caps(&sid).unwrap();
    assert!(caps.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)));
    assert!(!caps.covers(&Cap::new(Verb::SYS_KERNEL, Scope::Wild)));
    assert!(!caps.covers(&Cap::new(Verb::FS_WRITE, Scope::path("/etc/trigger-test"))));
    let limits = crate::activities::open_default()
        .unwrap()
        .execution_limits(state.uid, &activity.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        limits.used_attempts, 0,
        "submission must not charge or impersonate Root claim"
    );
    let jobs = Store::open_default()
        .unwrap()
        .list_for_activity(state.uid, &activity.id, 10)
        .unwrap();
    assert_eq!(
        jobs.len(),
        1,
        "existing Activity result views must find the same Job"
    );
}

#[test]
fn activity_configuration_cannot_supply_missing_scheduler_authority() {
    let state = TestState::new();
    let activity = state.activity(true);
    let args = [
        "--id",
        "no-authority",
        "--prompt",
        "Inspect",
        "--activity",
        &activity.id,
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let error = state
        .with_caps(
            CapSet::from_caps([Cap::new(Verb::TIME_CRON, Scope::Wild)]),
            || run("add", &args),
        )
        .unwrap_err();
    assert!(error.contains("lacks agent.spawn"));
    assert!(!rule_path("no-authority").exists());
    state.assert_no_work();
}

#[test]
fn manual_refusal_records_diagnostic_before_spending_capability_authorization() {
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("manual-blocked", Some(&activity.id));
    crate::activities::open_default()
        .unwrap()
        .transition(state.uid, &activity.id, ActivityState::Paused, None)
        .unwrap();
    let audit = fs::read(crate::paths::caps_audit_log_path()).unwrap();
    let error = state
        .with_caps(CapSet::new(), || run("run", std::slice::from_ref(&rule.id)))
        .unwrap_err();
    assert!(error.contains("paused"));
    assert_eq!(
        fs::read(crate::paths::caps_audit_log_path()).unwrap(),
        audit
    );
    let current = load_rule(&rule.id).unwrap();
    assert!(current.enabled);
    assert_eq!(
        current.last_delivery.unwrap().status,
        TriggerDeliveryStatus::Blocked
    );
    let recorded =
        fs::read_to_string(crate::paths::data_dir().join("clawd").join("audit.jsonl")).unwrap();
    assert!(recorded.contains("clawd.trigger.delivery"));
    assert!(!recorded.contains("Inspect the reported change."));
    if state.uid != 0 {
        use crate::notifications::NotificationService;
        assert!(state
            .command("run", &[&rule.id])
            .unwrap_err()
            .contains("paused"));
        let notifications = crate::notifications::open_default().unwrap();
        let visible = notifications.list(state.uid, false, 10).unwrap();
        assert_eq!(
            visible.len(),
            1,
            "repeated blocked events reuse notification deduplication"
        );
        assert_eq!(visible[0].kind, "trigger.blocked");
        assert!(visible[0].body.contains("paused"));
        assert!(notifications
            .list(state.uid + 1, false, 10)
            .unwrap()
            .is_empty());
    }
    state.assert_no_work();
}
