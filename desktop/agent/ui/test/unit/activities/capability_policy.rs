use super::*;
use crate::activities::Response;
use cos_agent_protocol::ActivityState;
use serde_json::json;

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OTHER: &str = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";

fn ready(lifecycle: ActivityState) -> Activities {
    crate::localize::localize();
    Activities {
        visible: true, selected: Some(ACTIVITY.into()),
        detail: Some(serde_json::from_value(json!({
            "activity": {"id": ACTIVITY, "title": "Release", "goal": "Prepare a release", "state": lifecycle},
            "jobs": [{"id": "existing-job", "status": "ok"}]
        })).unwrap()),
        ..Activities::default()
    }
}

fn rule() -> CapabilityPolicyRule {
    CapabilityPolicyRule {
        verb: "fs.read".into(),
        mode: CapabilityPolicyMode::Normal,
        scopes: vec![CapabilityPolicyScope::Path {
            value: "/workspace/**".into(),
        }],
    }
}

fn policy(enabled: bool) -> ActivityCapabilityPolicy {
    ActivityCapabilityPolicy {
        activity_id: ACTIVITY.into(),
        owner_uid: 1000,
        revision: 5,
        enabled,
        rules: vec![rule()],
        created_at: "2026-09-11T12:00:00Z".into(),
        updated_at: "2026-09-11T13:00:00Z".into(),
    }
}

fn update(state: &mut Activities, message: Message) -> Option<Request> {
    state.update(ActivityMessage::CapabilityPolicy(message), true)
}

fn response(policy: Option<ActivityCapabilityPolicy>) -> Response {
    Response::CapabilityPolicy(ActivityCapabilityPolicyResponse {
        schema: 1,
        activity_id: ACTIVITY.into(),
        capability_policy: policy,
    })
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

fn load(state: &mut Activities, policy: Option<ActivityCapabilityPolicy>) {
    let request = update(state, Message::Refresh).unwrap();
    assert!(matches!(&request.action, Action::GetCapabilityPolicy(id) if id == ACTIVITY));
    assert!(finish(state, request, Ok(response(policy))).is_none());
}

fn submitted(request: &Request) -> &ActivityCapabilityPolicySetRequest {
    match &request.action {
        Action::SetCapabilityPolicy { request, .. } => request,
        _ => panic!("expected a policy set request"),
    }
}

#[test]
fn capability_policy_unknown_null_and_failed_reads_never_invent_permissions() {
    let mut state = ready(ActivityState::Active);
    let before = state.detail.clone();
    assert!(update(&mut state, Message::Configure).is_none());
    assert!(state.error.is_some());
    load(&mut state, None);
    assert!(
        state
            .capability_policy
            .response
            .as_ref()
            .unwrap()
            .capability_policy
            .is_none()
    );
    assert_eq!(state.detail, before);
    let _ = state.view(true);
    let request = update(&mut state, Message::Refresh).unwrap();
    finish(&mut state, request, Err("Policy unavailable".into()));
    assert!(state.capability_policy.response.is_none());
    assert_eq!(state.error.as_deref(), Some("Policy unavailable"));
    assert!(!state.should_poll());
}

#[test]
fn capability_policy_creation_uses_null_cas_and_empty_rules_grant_nothing() {
    let mut state = ready(ActivityState::Active);
    let before = state.detail.clone();
    load(&mut state, None);
    update(&mut state, Message::Configure);
    let request = update(&mut state, Message::Save).unwrap();
    assert_eq!(
        serde_json::to_value(submitted(&request)).unwrap(),
        json!({
            "expected_revision": null, "policy": {"rules": []}
        })
    );
    assert!(update(&mut state, Message::Save).is_none());
    let mut created = policy(true);
    created.revision = 1;
    created.rules.clear();
    let refresh = finish(
        &mut state,
        request,
        Ok(Response::CapabilityPolicySaved(Box::new(created.clone()))),
    )
    .unwrap();
    assert!(state.capability_policy.form.is_none());
    assert!(state.capability_policy.response.is_none());
    assert!(matches!(refresh.action, Action::GetCapabilityPolicy(_)));
    finish(&mut state, refresh, Ok(response(Some(created))));
    assert_eq!(state.detail, before);
    assert!(state.notice.as_ref().unwrap().contains("No permissions"));
}

#[test]
fn capability_policy_fixed_editor_authors_modes_and_explicit_scopes_without_json_ui() {
    let mut state = ready(ActivityState::Active);
    load(&mut state, None);
    update(&mut state, Message::Configure);
    update(&mut state, Message::AddRule);
    update(&mut state, Message::Verb(0, "fs.read".into()));
    assert!(
        update(&mut state, Message::Save).is_none(),
        "no implicit wildcard for a new rule"
    );
    update(
        &mut state,
        Message::ScopeValue(0, 0, "/workspace/**".into()),
    );
    update(
        &mut state,
        Message::Mode(0, CapabilityPolicyMode::RequireApproval),
    );
    update(&mut state, Message::AddScope(0));
    update(&mut state, Message::ScopeValue(0, 1, "/notes/**".into()));
    let request = update(&mut state, Message::Save).unwrap();
    let rules = &submitted(&request).policy.rules;
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].verb, "fs.read");
    assert_eq!(rules[0].mode, CapabilityPolicyMode::RequireApproval);
    assert_eq!(rules[0].scopes.len(), 2);
    let _ = state.view(true);
    finish(&mut state, request, Err("fixture refusal".into()));
    update(&mut state, Message::Mode(0, CapabilityPolicyMode::Deny));
    assert!(
        state.capability_policy.form.as_ref().unwrap().draft.rules[0]
            .scopes
            .is_empty()
    );
    update(&mut state, Message::AddScope(0));
    assert!(
        state.capability_policy.form.as_ref().unwrap().draft.rules[0]
            .scopes
            .is_empty()
    );
    let request = update(&mut state, Message::Save).unwrap();
    assert_eq!(
        serde_json::to_value(&submitted(&request).policy.rules[0]).unwrap(),
        json!({
            "verb": "fs.read", "mode": "deny", "scopes": []
        })
    );
    finish(&mut state, request, Err("fixture refusal".into()));
    update(&mut state, Message::Mode(0, CapabilityPolicyMode::Normal));
    assert!(update(&mut state, Message::Save).is_none());
}

#[test]
fn capability_policy_scope_controls_cover_all_frozen_kinds_and_keep_values_inert() {
    let mut state = ready(ActivityState::Active);
    load(&mut state, Some(policy(true)));
    update(&mut state, Message::Configure);
    for (kind, value) in [
        (ScopeKind::Path, "/workspace/**"),
        (ScopeKind::Host, "example.org:443"),
        (ScopeKind::Name, "project/*"),
        (ScopeKind::SelfRef, "self"),
        (ScopeKind::Wild, ""),
    ] {
        update(&mut state, Message::ScopeKind(0, 0, kind));
        if kind != ScopeKind::Wild {
            update(&mut state, Message::ScopeValue(0, 0, value.into()));
        }
        let form = state.capability_policy.form.as_ref().unwrap();
        let scope = &form.draft.rules[0].scopes[0];
        assert_eq!(scope_kind(scope), kind);
        let _ = state.view(true);
        if kind == ScopeKind::Wild {
            assert_eq!(
                serde_json::to_value(scope).unwrap(),
                json!({"kind": "wild"})
            );
        } else {
            assert_eq!(scope.value(), Some(value));
        }
    }
    update(
        &mut state,
        Message::ScopeValue(0, 0, "not a wildcard payload".into()),
    );
    assert_eq!(
        state.capability_policy.form.as_ref().unwrap().draft.rules[0].scopes[0],
        CapabilityPolicyScope::Wild {}
    );
    update(&mut state, Message::RemoveScope(0, 0));
    assert!(update(&mut state, Message::Save).is_none());
    update(&mut state, Message::RemoveRule(0));
    assert!(
        state
            .capability_policy
            .form
            .as_ref()
            .unwrap()
            .draft
            .rules
            .is_empty()
    );
}

#[test]
fn capability_policy_editor_enforces_rule_scope_and_whole_draft_bounds() {
    let mut state = ready(ActivityState::Active);
    load(&mut state, None);
    update(&mut state, Message::Configure);
    for _ in 0..MAX_CAPABILITY_POLICY_RULES {
        update(&mut state, Message::AddRule);
    }
    update(&mut state, Message::AddRule);
    assert_eq!(
        state
            .capability_policy
            .form
            .as_ref()
            .unwrap()
            .draft
            .rules
            .len(),
        MAX_CAPABILITY_POLICY_RULES
    );
    assert!(state.error.is_some());
    state.capability_policy.form.as_mut().unwrap().draft.rules = vec![rule()];
    for _ in 1..MAX_CAPABILITY_POLICY_SCOPES {
        update(&mut state, Message::AddScope(0));
    }
    update(&mut state, Message::AddScope(0));
    assert_eq!(
        state.capability_policy.form.as_ref().unwrap().draft.rules[0]
            .scopes
            .len(),
        MAX_CAPABILITY_POLICY_SCOPES
    );
    assert!(state.error.is_some());
    state.capability_policy.form.as_mut().unwrap().draft.rules = vec![rule()];
    update(&mut state, Message::ScopeValue(0, 0, "x".repeat(16 * 1024)));
    assert!(update(&mut state, Message::Save).is_none());
    assert!(state.error.as_ref().unwrap().contains("16 KiB"));
}

#[test]
fn capability_policy_updates_preserve_disabled_state_and_accept_only_order_changes() {
    let mut state = ready(ActivityState::Paused);
    let previous = policy(false);
    let before = state.detail.clone();
    load(&mut state, Some(previous.clone()));
    update(&mut state, Message::Configure);
    update(&mut state, Message::AddScope(0));
    update(&mut state, Message::ScopeValue(0, 1, "/notes/**".into()));
    let request = update(&mut state, Message::Save).unwrap();
    assert_eq!(
        submitted(&request).expected_revision,
        Some(previous.revision)
    );
    let mut changed = previous;
    changed.revision += 1;
    changed.rules = submitted(&request).policy.rules.clone();
    changed.rules[0].scopes.reverse();
    let refresh = finish(
        &mut state,
        request,
        Ok(Response::CapabilityPolicySaved(Box::new(changed.clone()))),
    )
    .unwrap();
    finish(&mut state, refresh, Ok(response(Some(changed))));
    assert!(
        !state
            .capability_policy
            .response
            .as_ref()
            .unwrap()
            .capability_policy
            .as_ref()
            .unwrap()
            .enabled
    );
    assert_eq!(state.detail, before);
}

#[test]
fn capability_policy_acknowledgements_accept_host_case_and_duplicate_normalization() {
    let mut state = ready(ActivityState::Active);
    let mut previous = policy(true);
    previous.rules = vec![CapabilityPolicyRule {
        verb: "net.dial".into(),
        mode: CapabilityPolicyMode::RequireApproval,
        scopes: vec![CapabilityPolicyScope::Host {
            value: "example.org:443".into(),
        }],
    }];
    load(&mut state, Some(previous.clone()));
    update(&mut state, Message::Configure);
    update(&mut state, Message::AddScope(0));
    update(&mut state, Message::ScopeKind(0, 1, ScopeKind::Host));
    update(
        &mut state,
        Message::ScopeValue(0, 1, "EXAMPLE.ORG:443".into()),
    );
    let request = update(&mut state, Message::Save).unwrap();
    assert_eq!(submitted(&request).policy.rules[0].scopes.len(), 2);
    let mut normalized = previous;
    normalized.revision += 1;
    let refresh = finish(
        &mut state,
        request,
        Ok(Response::CapabilityPolicySaved(Box::new(
            normalized.clone(),
        ))),
    )
    .unwrap();
    assert!(state.error.is_none());
    finish(&mut state, refresh, Ok(response(Some(normalized))));
}

#[test]
fn capability_policy_conflict_and_edits_never_rebase_the_captured_revision() {
    let mut state = ready(ActivityState::Active);
    let previous = policy(true);
    load(&mut state, Some(previous.clone()));
    update(&mut state, Message::Configure);
    let request = update(&mut state, Message::Save).unwrap();
    finish(
        &mut state,
        request,
        Err("Conflict: policy revision changed".into()),
    );
    assert!(state.error.as_ref().unwrap().contains("Conflict"));
    assert!(update(&mut state, Message::Refresh).is_none());
    assert!(state.update(ActivityMessage::Refresh, true).is_none());
    update(
        &mut state,
        Message::Mode(0, CapabilityPolicyMode::RequireApproval),
    );
    let retry = update(&mut state, Message::Save).unwrap();
    assert_eq!(submitted(&retry).expected_revision, Some(previous.revision));
    finish(&mut state, retry, Err("Conflict: still stale".into()));
    let refresh = update(&mut state, Message::Discard).unwrap();
    let mut newer = previous;
    newer.revision += 1;
    finish(&mut state, refresh, Ok(response(Some(newer.clone()))));
    update(&mut state, Message::Configure);
    assert_eq!(
        state
            .capability_policy
            .form
            .as_ref()
            .unwrap()
            .request()
            .unwrap()
            .expected_revision,
        Some(newer.revision)
    );
}

#[test]
fn capability_policy_acknowledgements_reject_identity_revision_rule_and_enabled_changes() {
    for mutation in 0..7 {
        let mut state = ready(ActivityState::Active);
        let previous = policy(false);
        load(&mut state, Some(previous.clone()));
        update(&mut state, Message::Configure);
        let request = update(&mut state, Message::Save).unwrap();
        let mut changed = previous.clone();
        changed.revision += 1;
        match mutation {
            0 => changed.owner_uid += 1,
            1 => changed.activity_id = OTHER.into(),
            2 => changed.revision += 1,
            3 => changed.rules[0].mode = CapabilityPolicyMode::RequireApproval,
            4 => changed.rules[0].scopes = vec![CapabilityPolicyScope::Wild {}],
            5 => changed.enabled = true,
            _ => changed.rules.clear(),
        }
        assert!(
            finish(
                &mut state,
                request,
                Ok(Response::CapabilityPolicySaved(Box::new(changed)))
            )
            .is_none()
        );
        assert!(state.error.is_some());
        assert!(state.capability_policy.form.is_some());
        assert_eq!(
            state
                .capability_policy
                .response
                .as_ref()
                .unwrap()
                .capability_policy,
            Some(previous)
        );
    }
}

#[test]
fn capability_policy_terminal_states_allow_inspection_and_disable_not_enable_or_edit() {
    for lifecycle in [ActivityState::Completed, ActivityState::Cancelled] {
        let mut state = ready(lifecycle);
        let before = state.detail.clone();
        let previous = policy(true);
        load(&mut state, Some(previous.clone()));
        assert!(update(&mut state, Message::Configure).is_none());
        let request = update(&mut state, Message::SetEnabled(false)).unwrap();
        let mut disabled = previous;
        disabled.enabled = false;
        disabled.revision += 1;
        let refresh = finish(
            &mut state,
            request,
            Ok(Response::CapabilityPolicySaved(Box::new(disabled.clone()))),
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
fn capability_policy_toggle_cannot_change_rules_or_replay_on_conflict() {
    let mut state = ready(ActivityState::Active);
    let previous = policy(true);
    load(&mut state, Some(previous.clone()));
    let request = update(&mut state, Message::SetEnabled(false)).unwrap();
    finish(
        &mut state,
        request,
        Err("Conflict: policy revision changed".into()),
    );
    assert_eq!(
        state
            .capability_policy
            .response
            .as_ref()
            .unwrap()
            .capability_policy,
        Some(previous.clone())
    );
    let retry = update(&mut state, Message::SetEnabled(false)).unwrap();
    assert!(
        matches!(&retry.action, Action::EnableCapabilityPolicy { request, .. }
        if request.expected_revision == previous.revision && !request.enabled)
    );
    let mut bad = previous.clone();
    bad.revision += 1;
    bad.enabled = false;
    bad.rules.clear();
    assert!(
        finish(
            &mut state,
            retry,
            Ok(Response::CapabilityPolicySaved(Box::new(bad)))
        )
        .is_none()
    );
    assert!(state.error.is_some());
    assert_eq!(
        state
            .capability_policy
            .response
            .as_ref()
            .unwrap()
            .capability_policy,
        Some(previous)
    );
}

#[test]
fn capability_policy_navigation_discards_stale_read_set_toggle_and_error_responses() {
    for action in 0..3 {
        let mut state = ready(ActivityState::Active);
        let previous = policy(true);
        load(&mut state, Some(previous.clone()));
        let (request, result) = match action {
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
                    Response::CapabilityPolicySaved(Box::new(changed)),
                )
            }
            _ => {
                let mut changed = previous;
                changed.revision += 1;
                changed.enabled = false;
                (
                    update(&mut state, Message::SetEnabled(false)).unwrap(),
                    Response::CapabilityPolicySaved(Box::new(changed)),
                )
            }
        };
        let current = state
            .update(ActivityMessage::Open(OTHER.into()), true)
            .unwrap();
        finish(
            &mut state,
            request.clone(),
            Err("stale policy error".into()),
        );
        finish(&mut state, request, Ok(result));
        assert!(state.capability_policy.response.is_none());
        assert!(state.capability_policy.form.is_none());
        assert!(state.error.is_none());
        assert!(state.notice.is_none());
        let mut detail = ready(ActivityState::Paused).detail.unwrap();
        detail.activity.id = OTHER.into();
        finish(&mut state, current, Ok(Response::Detail(Box::new(detail))));
        assert_eq!(state.selected.as_deref(), Some(OTHER));
    }
}

#[test]
fn capability_policy_forms_exclude_other_activity_actions_and_never_replay_on_reconnect() {
    let mut state = ready(ActivityState::Active);
    let before = state.detail.clone();
    load(&mut state, Some(policy(true)));
    update(&mut state, Message::Configure);
    assert!(!state.should_poll());
    assert!(state.update(ActivityMessage::Run, true).is_none());
    assert!(
        state
            .update(ActivityMessage::Transition(ActivityState::Completed), true)
            .is_none()
    );
    assert!(
        state
            .update(ActivityMessage::RefreshReceipts, true)
            .is_none()
    );
    assert!(
        state
            .update(
                ActivityMessage::ExecutionLimits(super::super::ExecutionLimitsMessage::Refresh),
                true
            )
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
    assert_eq!(state.detail, before);
    state.hide();
    assert!(state.capability_policy.form.is_some());
    assert!(state.update(ActivityMessage::Show, true).is_none());
    let request = update(&mut state, Message::Save).unwrap();
    let mut changed = policy(true);
    changed.revision += 1;
    state.hide();
    assert!(state.capability_policy.form.is_none());
    finish(
        &mut state,
        request,
        Ok(Response::CapabilityPolicySaved(Box::new(changed))),
    );
    assert!(state.notice.is_none());
    assert!(matches!(
        state.update(ActivityMessage::Show, true).unwrap().action,
        Action::Get(_)
    ));
}

#[test]
fn capability_policy_unknown_verbs_and_offline_saves_remain_errors_without_local_authority() {
    let mut state = ready(ActivityState::Active);
    load(&mut state, Some(policy(true)));
    update(&mut state, Message::Configure);
    update(&mut state, Message::Verb(0, "not.a.catalogue.verb".into()));
    let request = update(&mut state, Message::Save).unwrap();
    finish(&mut state, request, Err("Unknown catalogue verb".into()));
    assert_eq!(state.error.as_deref(), Some("Unknown catalogue verb"));
    assert!(state.capability_policy.form.is_some());
    assert!(
        state
            .update(ActivityMessage::CapabilityPolicy(Message::Save), false)
            .is_none()
    );
    assert!(state.pending.is_none());
    assert!(state.error.is_some());
}

#[test]
fn capability_policy_mode_and_scope_views_are_fixed_inert_and_not_approval_controls() {
    let state = ready(ActivityState::Active);
    assert_eq!(
        mode_label(CapabilityPolicyMode::Normal),
        fl!("activity-capability-policy-mode-normal")
    );
    assert_eq!(
        mode_label(CapabilityPolicyMode::RequireApproval),
        fl!("activity-capability-policy-mode-ask")
    );
    assert_eq!(
        mode_label(CapabilityPolicyMode::Deny),
        fl!("activity-capability-policy-mode-deny")
    );
    assert_eq!(
        enabled_label(false),
        fl!("activity-capability-policy-disabled")
    );
    let raw = "<script>inert</script> $(not executed)";
    for scope in [
        CapabilityPolicyScope::Path { value: raw.into() },
        CapabilityPolicyScope::Host {
            value: "example.org".into(),
        },
        CapabilityPolicyScope::Name { value: raw.into() },
        CapabilityPolicyScope::SelfRef {
            value: "self".into(),
        },
        CapabilityPolicyScope::Wild {},
    ] {
        let displayed = CapabilityPolicyRule {
            scopes: vec![scope.clone()],
            ..rule()
        };
        let _ = rule_view(&displayed);
        let _ = scope_editor(0, 0, &scope, true);
        assert_eq!(displayed.scopes[0], scope);
    }
    assert!(state.pending.is_none());
}
