use super::*;
use crate::activities::Response;
use cos_agent_protocol::ActivityState;
use serde_json::json;
use std::time::{Duration, UNIX_EPOCH};

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OTHER: &str = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";

fn ready(lifecycle: ActivityState) -> Activities {
    crate::localize::localize();
    Activities {
        visible: true,
        selected: Some(ACTIVITY.into()),
        detail: Some(serde_json::from_value(json!({
            "activity": {"id": ACTIVITY, "title": "Release", "goal": "Prepare the release", "state": lifecycle},
            "jobs": [{"id": "existing-job", "status": "ok"}]
        })).unwrap()),
        ..Activities::default()
    }
}

fn policy(enabled: bool) -> ActivityExecutionLimits {
    ActivityExecutionLimits {
        activity_id: ACTIVITY.into(),
        owner_uid: 1000,
        revision: 5,
        enabled,
        limits: ExecutionLimitsDraft {
            max_attempts: 10,
            max_turns_per_attempt: 20,
            expires_at: "2099-01-01T12:00:00Z".into(),
        },
        used_attempts: 8,
        created_at: "2026-09-11T12:00:00Z".into(),
        updated_at: "2026-09-11T13:00:00Z".into(),
    }
}

fn response(current: Option<ActivityExecutionLimits>) -> Response {
    Response::ExecutionLimits(ActivityExecutionLimitsResponse {
        schema: 1,
        activity_id: ACTIVITY.into(),
        execution_limits: current,
    })
}

fn update(state: &mut Activities, message: Message) -> Option<Request> {
    state.update(ActivityMessage::ExecutionLimits(message), true)
}

fn finish(
    state: &mut Activities,
    request: Request,
    result: Result<Response, String>,
) -> Option<Request> {
    state.update(
        ActivityMessage::Loaded {
            generation: request.generation,
            result,
        },
        true,
    )
}

fn load(state: &mut Activities, current: Option<ActivityExecutionLimits>) {
    let request = update(state, Message::Refresh).unwrap();
    assert!(matches!(&request.action, Action::GetExecutionLimits(id) if id == ACTIVITY));
    assert!(finish(state, request, Ok(response(current))).is_none());
}

fn set_request(request: &Request) -> &ActivityExecutionLimitsSetRequest {
    match &request.action {
        Action::SetExecutionLimits { request, .. } => request,
        _ => panic!("expected a limits set request"),
    }
}

#[test]
fn activity_execution_limits_unknown_null_and_failed_reads_do_not_invent_configuration() {
    let mut state = ready(ActivityState::Active);
    assert!(state.execution_limits.response.is_none());
    assert!(update(&mut state, Message::Configure).is_none());
    assert!(state.error.is_some());
    let before = state.detail.clone();
    load(&mut state, None);
    assert!(
        state
            .execution_limits
            .response
            .as_ref()
            .unwrap()
            .execution_limits
            .is_none()
    );
    assert_eq!(state.detail, before);
    assert!(state.notice.is_none());
    let _ = state.view(true);
    let read = update(&mut state, Message::Refresh).unwrap();
    finish(
        &mut state,
        read,
        Err("Execution limits service unavailable".into()),
    );
    assert!(state.execution_limits.response.is_none());
    assert_eq!(
        state.error.as_deref(),
        Some("Execution limits service unavailable")
    );
    assert!(!state.should_poll());
}

#[test]
fn activity_execution_limits_create_is_explicit_null_cas_and_waits_for_backend_identity() {
    let mut state = ready(ActivityState::Active);
    let before = state.detail.clone();
    load(&mut state, None);
    update(&mut state, Message::Configure);
    assert!(
        update(&mut state, Message::Save).is_none(),
        "expiry is required"
    );
    update(&mut state, Message::Field(Field::MaxAttempts, "5".into()));
    update(&mut state, Message::Field(Field::MaxTurns, "30".into()));
    update(
        &mut state,
        Message::Field(Field::ExpiresAt, "2099-01-01T05:00:00-07:00".into()),
    );
    let request = update(&mut state, Message::Save).unwrap();
    let body = set_request(&request).clone();
    assert!(body.expected_revision.is_none());
    let encoded = serde_json::to_value(&body).unwrap();
    assert!(encoded["expected_revision"].is_null());
    assert_eq!(encoded.as_object().unwrap().len(), 2);
    assert_eq!(body.limits.max_attempts, 5);
    assert_eq!(body.limits.max_turns_per_attempt, 30);
    assert!(
        state
            .execution_limits
            .response
            .as_ref()
            .unwrap()
            .execution_limits
            .is_none()
    );
    assert!(update(&mut state, Message::Save).is_none());
    update(&mut state, Message::Field(Field::MaxAttempts, "99".into()));
    assert_eq!(
        state.execution_limits.form.as_ref().unwrap().max_attempts,
        "5"
    );
    let mut created = policy(true);
    created.revision = 1;
    created.used_attempts = 0;
    created.limits = body.limits;
    created.limits.expires_at = "2099-01-01T12:00:00.000000000Z".into();
    let read = finish(
        &mut state,
        request,
        Ok(Response::ExecutionLimitsSaved(Box::new(created.clone()))),
    )
    .unwrap();
    assert!(matches!(read.action, Action::GetExecutionLimits(_)));
    assert!(state.execution_limits.form.is_none());
    assert!(state.execution_limits.response.is_none());
    assert_eq!(state.detail, before);
    finish(&mut state, read, Ok(response(Some(created))));
    assert_eq!(state.detail, before);
}

#[test]
fn activity_execution_limits_update_can_exhaust_without_reset_or_reenable() {
    let mut state = ready(ActivityState::Paused);
    let before = state.detail.clone();
    let previous = policy(false);
    load(&mut state, Some(previous.clone()));
    update(&mut state, Message::Configure);
    update(&mut state, Message::Field(Field::MaxAttempts, "3".into()));
    let request = update(&mut state, Message::Save).unwrap();
    assert_eq!(
        set_request(&request).expected_revision,
        Some(previous.revision)
    );
    let mut updated = previous.clone();
    updated.revision += 1;
    updated.used_attempts += 1;
    updated.limits = set_request(&request).limits.clone();
    let read = finish(
        &mut state,
        request,
        Ok(Response::ExecutionLimitsSaved(Box::new(updated.clone()))),
    )
    .unwrap();
    finish(&mut state, read, Ok(response(Some(updated))));
    let current = state
        .execution_limits
        .response
        .as_ref()
        .unwrap()
        .execution_limits
        .as_ref()
        .unwrap();
    assert_eq!(current.used_attempts, 9);
    assert_eq!(current.remaining_attempts(), 0);
    assert!(!current.enabled);
    assert_eq!(state.detail, before);
    let _ = state.view(true);
}

#[test]
fn activity_execution_limits_conflicts_keep_the_original_revision_even_after_edits() {
    let mut state = ready(ActivityState::Active);
    let previous = policy(true);
    load(&mut state, Some(previous.clone()));
    update(&mut state, Message::Configure);
    let request = update(&mut state, Message::Save).unwrap();
    finish(
        &mut state,
        request,
        Err("Conflict: execution limits revision changed".into()),
    );
    assert!(state.error.as_ref().unwrap().contains("Conflict"));
    assert!(!state.should_poll());
    assert!(update(&mut state, Message::Refresh).is_none());
    assert!(state.update(ActivityMessage::Refresh, true).is_none());
    update(&mut state, Message::Field(Field::MaxAttempts, "20".into()));
    let retry = update(&mut state, Message::Save).unwrap();
    assert_eq!(
        set_request(&retry).expected_revision,
        Some(previous.revision)
    );
    assert_eq!(set_request(&retry).limits.max_attempts, 20);
    finish(&mut state, retry, Err("Conflict: still stale".into()));
    let refresh = update(&mut state, Message::Discard).unwrap();
    let mut newer = previous;
    newer.revision += 1;
    finish(&mut state, refresh, Ok(response(Some(newer.clone()))));
    update(&mut state, Message::Configure);
    let request = update(&mut state, Message::Save).unwrap();
    assert_eq!(
        set_request(&request).expected_revision,
        Some(newer.revision)
    );
}

#[test]
fn activity_execution_limits_toggle_is_explicit_cas_preserves_limits_and_refetches() {
    for enabled in [false, true] {
        let mut state = ready(ActivityState::Active);
        let before = state.detail.clone();
        let previous = policy(enabled);
        load(&mut state, Some(previous.clone()));
        assert!(update(&mut state, Message::SetEnabled(enabled)).is_none());
        let request = update(&mut state, Message::SetEnabled(!enabled)).unwrap();
        assert!(
            matches!(&request.action, Action::EnableExecutionLimits { request, .. }
            if request.expected_revision == previous.revision && request.enabled != enabled)
        );
        assert_eq!(
            state
                .execution_limits
                .response
                .as_ref()
                .unwrap()
                .execution_limits,
            Some(previous.clone())
        );
        let mut changed = previous;
        changed.enabled = !enabled;
        changed.revision += 1;
        changed.used_attempts += 1;
        let refresh = finish(
            &mut state,
            request,
            Ok(Response::ExecutionLimitsSaved(Box::new(changed.clone()))),
        )
        .unwrap();
        assert!(matches!(refresh.action, Action::GetExecutionLimits(_)));
        finish(&mut state, refresh, Ok(response(Some(changed))));
        assert_eq!(state.detail, before);
        assert!(state.notice.is_some());
    }
}

#[test]
fn activity_execution_limits_toggle_conflicts_do_not_rebase_or_apply_optimistically() {
    let mut state = ready(ActivityState::Active);
    let previous = policy(true);
    load(&mut state, Some(previous.clone()));
    let request = update(&mut state, Message::SetEnabled(false)).unwrap();
    finish(
        &mut state,
        request,
        Err("Conflict: execution limits revision changed".into()),
    );
    assert!(state.error.as_ref().unwrap().contains("Conflict"));
    assert_eq!(
        state
            .execution_limits
            .response
            .as_ref()
            .unwrap()
            .execution_limits,
        Some(previous.clone())
    );
    let retry = update(&mut state, Message::SetEnabled(false)).unwrap();
    assert!(
        matches!(retry.action, Action::EnableExecutionLimits { request, .. }
        if request.expected_revision == previous.revision && !request.enabled)
    );
}

#[test]
fn activity_execution_limits_terminal_states_allow_read_and_disable_but_not_configure_or_enable() {
    for lifecycle in [ActivityState::Completed, ActivityState::Cancelled] {
        let mut state = ready(lifecycle);
        let before = state.detail.clone();
        let current = policy(true);
        load(&mut state, Some(current.clone()));
        assert!(update(&mut state, Message::Configure).is_none());
        assert!(state.execution_limits.form.is_none());
        assert!(state.error.is_some());
        let disable = update(&mut state, Message::SetEnabled(false)).unwrap();
        let mut disabled = current;
        disabled.revision += 1;
        disabled.enabled = false;
        let refresh = finish(
            &mut state,
            disable,
            Ok(Response::ExecutionLimitsSaved(Box::new(disabled.clone()))),
        )
        .unwrap();
        finish(&mut state, refresh, Ok(response(Some(disabled))));
        assert!(update(&mut state, Message::SetEnabled(true)).is_none());
        assert!(state.error.is_some());
        assert_eq!(state.detail, before);
        let _ = state.view(true);
    }
    for lifecycle in [ActivityState::Active, ActivityState::Paused] {
        let mut state = ready(lifecycle);
        load(&mut state, Some(policy(false)));
        assert!(update(&mut state, Message::SetEnabled(true)).is_some());
    }
}

#[test]
fn activity_execution_limits_set_rejects_owner_revision_counter_enabled_and_payload_mismatches() {
    for mismatch in 0..7 {
        let mut state = ready(ActivityState::Active);
        let previous = policy(false);
        load(&mut state, Some(previous.clone()));
        update(&mut state, Message::Configure);
        let request = update(&mut state, Message::Save).unwrap();
        let mut invalid = previous.clone();
        invalid.revision += 1;
        match mismatch {
            0 => invalid.owner_uid += 1,
            1 => invalid.activity_id = OTHER.into(),
            2 => invalid.revision += 1,
            3 => invalid.used_attempts = 0,
            4 => invalid.enabled = true,
            5 => invalid.limits.max_turns_per_attempt += 1,
            _ => invalid.limits.expires_at = "2099-01-01T12:00:01Z".into(),
        }
        assert!(
            finish(
                &mut state,
                request,
                Ok(Response::ExecutionLimitsSaved(Box::new(invalid)))
            )
            .is_none()
        );
        assert!(state.error.is_some());
        assert!(state.execution_limits.form.is_some());
        assert_eq!(
            state
                .execution_limits
                .response
                .as_ref()
                .unwrap()
                .execution_limits,
            Some(previous)
        );
    }
}

#[test]
fn activity_execution_limits_toggle_cannot_reset_extend_or_change_unrequested_limits() {
    for mismatch in 0..6 {
        let mut state = ready(ActivityState::Active);
        let previous = policy(true);
        load(&mut state, Some(previous.clone()));
        let request = update(&mut state, Message::SetEnabled(false)).unwrap();
        let mut invalid = previous.clone();
        invalid.revision += 1;
        invalid.enabled = false;
        match mismatch {
            0 => invalid.owner_uid += 1,
            1 => invalid.revision += 1,
            2 => invalid.used_attempts -= 1,
            3 => invalid.enabled = true,
            4 => invalid.limits.max_attempts += 1,
            _ => invalid.limits.expires_at = "2100-01-01T12:00:00Z".into(),
        }
        assert!(
            finish(
                &mut state,
                request,
                Ok(Response::ExecutionLimitsSaved(Box::new(invalid)))
            )
            .is_none()
        );
        assert!(state.error.is_some());
        assert_eq!(
            state
                .execution_limits
                .response
                .as_ref()
                .unwrap()
                .execution_limits,
            Some(previous)
        );
    }
}

#[test]
fn activity_execution_limits_navigation_discards_stale_get_set_and_toggle_results_and_errors() {
    for action in 0..3 {
        let mut state = ready(ActivityState::Active);
        let previous = policy(true);
        load(&mut state, Some(previous.clone()));
        let (stale, result) = match action {
            0 => (
                update(&mut state, Message::Refresh).unwrap(),
                response(Some(previous.clone())),
            ),
            1 => {
                update(&mut state, Message::Configure);
                let mut changed = previous.clone();
                changed.revision += 1;
                (
                    update(&mut state, Message::Save).unwrap(),
                    Response::ExecutionLimitsSaved(Box::new(changed)),
                )
            }
            _ => {
                let mut changed = previous;
                changed.revision += 1;
                changed.enabled = false;
                (
                    update(&mut state, Message::SetEnabled(false)).unwrap(),
                    Response::ExecutionLimitsSaved(Box::new(changed)),
                )
            }
        };
        let current = state
            .update(ActivityMessage::Open(OTHER.into()), true)
            .unwrap();
        finish(
            &mut state,
            stale.clone(),
            Err("Stale limits failure".into()),
        );
        finish(&mut state, stale, Ok(result));
        assert!(state.execution_limits.response.is_none());
        assert!(state.execution_limits.form.is_none());
        assert!(state.error.is_none());
        assert!(state.notice.is_none());
        let mut detail = ready(ActivityState::Paused).detail.unwrap();
        detail.activity.id = OTHER.into();
        finish(&mut state, current, Ok(Response::Detail(Box::new(detail))));
        assert_eq!(state.selected.as_deref(), Some(OTHER));
    }
}

#[test]
fn activity_execution_limits_read_identity_schema_and_counter_shapes_fail_without_defaults() {
    for mismatch in 0..4 {
        let mut state = ready(ActivityState::Active);
        let request = update(&mut state, Message::Refresh).unwrap();
        let mut result = ActivityExecutionLimitsResponse {
            schema: 1,
            activity_id: ACTIVITY.into(),
            execution_limits: Some(policy(true)),
        };
        match mismatch {
            0 => result.schema = 2,
            1 => result.activity_id = OTHER.into(),
            2 => result.execution_limits.as_mut().unwrap().activity_id = OTHER.into(),
            _ => {
                result
                    .execution_limits
                    .as_mut()
                    .unwrap()
                    .limits
                    .max_attempts = 0
            }
        }
        finish(&mut state, request, Ok(Response::ExecutionLimits(result)));
        assert!(state.execution_limits.response.is_none());
        assert!(state.error.is_some());
    }
}

#[test]
fn activity_execution_limits_form_validation_and_future_expiry_errors_preserve_cas() {
    let mut state = ready(ActivityState::Active);
    load(&mut state, Some(policy(true)));
    update(&mut state, Message::Configure);
    for (field, value) in [
        (Field::MaxAttempts, "0"),
        (Field::MaxAttempts, "1001"),
        (Field::MaxAttempts, "-1"),
        (Field::MaxAttempts, "1.5"),
    ] {
        update(&mut state, Message::Field(field, value.into()));
        assert!(update(&mut state, Message::Save).is_none());
        assert!(state.error.is_some());
    }
    update(&mut state, Message::Field(Field::MaxAttempts, "2".into()));
    update(&mut state, Message::Field(Field::MaxTurns, "101".into()));
    assert!(update(&mut state, Message::Save).is_none());
    update(&mut state, Message::Field(Field::MaxTurns, "1".into()));
    update(
        &mut state,
        Message::Field(Field::ExpiresAt, "not RFC3339".into()),
    );
    assert!(update(&mut state, Message::Save).is_none());
    update(
        &mut state,
        Message::Field(Field::ExpiresAt, "2000-01-01T00:00:00Z".into()),
    );
    let request = update(&mut state, Message::Save).unwrap();
    finish(
        &mut state,
        request,
        Err("Expiry must be future at transaction time".into()),
    );
    assert_eq!(
        state.execution_limits.form.as_ref().unwrap().expires_at,
        "2000-01-01T00:00:00Z"
    );
    assert_eq!(
        state
            .execution_limits
            .form
            .as_ref()
            .unwrap()
            .previous
            .as_ref()
            .unwrap()
            .revision,
        5
    );
    assert!(state.error.as_ref().unwrap().contains("transaction time"));
}

#[test]
fn activity_execution_limits_forms_preserve_other_activity_surfaces_and_never_replay_writes() {
    let mut state = ready(ActivityState::Active);
    let before = state.detail.clone();
    load(&mut state, Some(policy(true)));
    update(&mut state, Message::Configure);
    assert!(!state.should_poll());
    assert!(state.update(ActivityMessage::Tick, true).is_none());
    assert!(
        state
            .update(ActivityMessage::RefreshReceipts, true)
            .is_none()
    );
    assert!(
        state
            .update(
                ActivityMessage::ObjectState(super::super::ObjectStateMessage::Refresh),
                true
            )
            .is_none()
    );
    assert!(state.update(ActivityMessage::Run, true).is_none());
    assert!(
        state
            .update(ActivityMessage::Transition(ActivityState::Completed), true)
            .is_none()
    );
    state.update(ActivityMessage::Edit, true);
    state.update(ActivityMessage::NewObject, true);
    assert!(state.form.is_none());
    assert!(state.object_form.is_none());
    assert_eq!(state.detail, before);
    state.hide();
    assert!(state.execution_limits.form.is_some());
    assert!(state.update(ActivityMessage::Show, true).is_none());
    let pending = update(&mut state, Message::Save).unwrap();
    let mut updated = policy(true);
    updated.revision += 1;
    state.hide();
    assert!(state.execution_limits.form.is_none());
    finish(
        &mut state,
        pending,
        Ok(Response::ExecutionLimitsSaved(Box::new(updated))),
    );
    assert!(state.notice.is_none());
    assert!(state.execution_limits.response.is_none());
    assert!(matches!(
        state.update(ActivityMessage::Show, true).unwrap().action,
        Action::Get(_)
    ));
}

#[test]
fn activity_execution_limits_existing_forms_and_disconnection_do_not_admit_mutations() {
    let mut state = ready(ActivityState::Active);
    load(&mut state, Some(policy(true)));
    state.update(ActivityMessage::Edit, true);
    assert!(update(&mut state, Message::Configure).is_none());
    assert!(state.execution_limits.form.is_none());
    state.form = None;
    update(&mut state, Message::Configure);
    assert!(
        state
            .update(ActivityMessage::ExecutionLimits(Message::Save), false)
            .is_none()
    );
    assert!(state.error.is_some());
    assert!(state.execution_limits.form.is_some());
    assert!(state.pending.is_none());
}

#[test]
fn activity_execution_limits_enabled_expired_and_exhausted_widgets_are_read_only_projections() {
    let mut state = ready(ActivityState::Completed);
    let before = state.detail.clone();
    let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    assert_eq!(
        enabled_label(false),
        fl!("activity-execution-limits-disabled")
    );
    assert_eq!(expiry_label(true), fl!("activity-execution-limits-expired"));
    for enabled in [false, true] {
        for exhausted in [false, true] {
            for expired in [false, true] {
                let mut current = policy(enabled);
                if exhausted {
                    current.limits.max_attempts = 3;
                }
                if expired {
                    current.limits.expires_at = "2000-01-01T00:00:00Z".into();
                }
                let _ = limits_summary(&current, now);
                assert_eq!(current.limits.is_expired_at(now).unwrap(), expired);
                assert_eq!(current.remaining_attempts() == 0, exhausted);
                load(&mut state, Some(current.clone()));
                let _ = state.view(true);
                assert_eq!(
                    state
                        .execution_limits
                        .response
                        .as_ref()
                        .unwrap()
                        .execution_limits,
                    Some(current)
                );
                assert_eq!(state.detail, before);
                assert!(state.notice.is_none());
            }
        }
    }
}
