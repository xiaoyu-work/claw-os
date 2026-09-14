use super::*;
use crate::activities::Response;
use cos_agent_protocol::ActivityState;
use serde_json::json;

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn ready(lifecycle: ActivityState) -> Activities {
    crate::localize::localize();
    Activities {
        visible: true,
        selected: Some(ACTIVITY.into()),
        detail: Some(
            serde_json::from_value(json!({
                "activity": {"id": ACTIVITY, "title": "Release", "goal": "Prepare", "state": lifecycle},
                "jobs": []
            }))
            .unwrap(),
        ),
        ..Activities::default()
    }
}

fn policy(enabled: bool) -> ActivityMonetaryBudget {
    ActivityMonetaryBudget {
        activity_id: ACTIVITY.into(),
        owner_uid: 1000,
        revision: 5,
        enabled,
        spent_microusd: 2_000_000,
        reserved_microusd: 1_000_000,
        budget: MonetaryBudgetDraft {
            currency: MonetaryCurrency::Usd,
            max_total_microusd: 5_000_000,
            input_microusd_per_million_tokens: 250_000,
            output_microusd_per_million_tokens: 1_000_000,
            max_output_tokens_per_turn: 4096,
        },
        created_at: "2026-09-13T00:00:00Z".into(),
        updated_at: "2026-09-13T01:00:00Z".into(),
    }
}

fn response(current: Option<ActivityMonetaryBudget>) -> Response {
    Response::MonetaryBudget(ActivityMonetaryBudgetResponse {
        schema: 1,
        activity_id: ACTIVITY.into(),
        monetary_budget: current,
    })
}

fn update(state: &mut Activities, message: Message) -> Option<Request> {
    state.update(ActivityMessage::MonetaryBudget(message), true)
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

fn load(state: &mut Activities, current: Option<ActivityMonetaryBudget>) {
    let request = update(state, Message::Refresh).unwrap();
    assert!(matches!(&request.action, Action::GetMonetaryBudget(id) if id == ACTIVITY));
    assert!(finish(state, request, Ok(response(current))).is_none());
}

#[test]
fn monetary_budget_absence_create_and_exact_revision_are_explicit() {
    let mut state = ready(ActivityState::Active);
    assert!(update(&mut state, Message::Configure).is_none());
    load(&mut state, None);
    update(&mut state, Message::Configure);
    let request = update(&mut state, Message::Save).unwrap();
    let body = match &request.action {
        Action::SetMonetaryBudget { request, .. } => request.clone(),
        _ => panic!("expected monetary-budget set"),
    };
    assert!(body.expected_revision.is_none());
    assert_eq!(body.budget.currency, MonetaryCurrency::Usd);
    let mut created = policy(true);
    created.revision = 1;
    created.spent_microusd = 0;
    created.reserved_microusd = 0;
    created.budget = body.budget;
    let refresh = finish(
        &mut state,
        request,
        Ok(Response::MonetaryBudgetSaved(Box::new(created.clone()))),
    )
    .unwrap();
    assert!(matches!(refresh.action, Action::GetMonetaryBudget(_)));
    finish(&mut state, refresh, Ok(response(Some(created))));
    assert!(state.monetary_budget.form.is_none());
}

#[test]
fn monetary_budget_update_and_toggle_keep_original_cas_and_backend_accounting() {
    let mut state = ready(ActivityState::Paused);
    let previous = policy(false);
    load(&mut state, Some(previous.clone()));
    update(&mut state, Message::Configure);
    update(
        &mut state,
        Message::Field(Field::MaximumTotal, "8000000".into()),
    );
    let request = update(&mut state, Message::Save).unwrap();
    let submitted = match &request.action {
        Action::SetMonetaryBudget { request, .. } => request.clone(),
        _ => panic!("expected monetary-budget set"),
    };
    assert_eq!(submitted.expected_revision, Some(previous.revision));
    let mut saved = previous.clone();
    saved.revision += 1;
    saved.budget = submitted.budget;
    saved.spent_microusd += 500_000;
    saved.reserved_microusd = 250_000;
    let refresh = finish(
        &mut state,
        request,
        Ok(Response::MonetaryBudgetSaved(Box::new(saved.clone()))),
    )
    .unwrap();
    finish(&mut state, refresh, Ok(response(Some(saved.clone()))));
    assert_eq!(saved.remaining_microusd(), 5_250_000);
    let request = update(&mut state, Message::SetEnabled(true)).unwrap();
    assert!(matches!(
        &request.action,
        Action::EnableMonetaryBudget { request, .. }
            if request.expected_revision == saved.revision && request.enabled
    ));
    let mut enabled = saved;
    enabled.revision += 1;
    enabled.enabled = true;
    enabled.spent_microusd += 1;
    let refresh = finish(
        &mut state,
        request,
        Ok(Response::MonetaryBudgetSaved(Box::new(enabled.clone()))),
    )
    .unwrap();
    finish(&mut state, refresh, Ok(response(Some(enabled))));
}

#[test]
fn monetary_budget_conflicts_and_terminal_rules_do_not_rebase_or_enable() {
    let mut state = ready(ActivityState::Completed);
    load(&mut state, Some(policy(true)));
    assert!(update(&mut state, Message::Configure).is_none());
    assert!(update(&mut state, Message::SetEnabled(false)).is_some());

    let mut state = ready(ActivityState::Active);
    let previous = policy(true);
    load(&mut state, Some(previous.clone()));
    update(&mut state, Message::Configure);
    assert!(state.update(ActivityMessage::Refresh, true).is_none());
    let request = update(&mut state, Message::Save).unwrap();
    finish(
        &mut state,
        request,
        Err("Conflict: monetary budget revision changed".into()),
    );
    update(
        &mut state,
        Message::Field(Field::MaximumOutputTokens, "2048".into()),
    );
    let retry = update(&mut state, Message::Save).unwrap();
    assert!(matches!(
        &retry.action,
        Action::SetMonetaryBudget { request, .. }
            if request.expected_revision == Some(previous.revision)
    ));
    let _ = state.view(true);
}
