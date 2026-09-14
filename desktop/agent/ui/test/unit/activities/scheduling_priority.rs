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
                "activity": {
                    "id": ACTIVITY,
                    "title": "Release",
                    "goal": "Prepare",
                    "state": lifecycle
                },
                "jobs": []
            }))
            .unwrap(),
        ),
        ..Activities::default()
    }
}

fn policy(priority: ActivitySchedulingPriority) -> ActivitySchedulingPolicy {
    ActivitySchedulingPolicy {
        activity_id: ACTIVITY.into(),
        owner_uid: 1000,
        revision: 5,
        priority,
        created_at: "2026-09-13T00:00:00Z".into(),
        updated_at: "2026-09-13T01:00:00Z".into(),
    }
}

fn response(current: Option<ActivitySchedulingPolicy>) -> Response {
    Response::SchedulingPriority(ActivitySchedulingPriorityResponse {
        schema: 1,
        activity_id: ACTIVITY.into(),
        scheduling_policy: current,
    })
}

fn update(state: &mut Activities, message: Message) -> Option<Request> {
    state.update(ActivityMessage::SchedulingPriority(message), true)
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

fn load(state: &mut Activities, current: Option<ActivitySchedulingPolicy>) {
    let request = update(state, Message::Refresh).unwrap();
    assert!(matches!(&request.action, Action::GetSchedulingPriority(id) if id == ACTIVITY));
    assert!(finish(state, request, Ok(response(current))).is_none());
}

#[test]
fn scheduling_priority_absence_create_and_exact_revision_are_explicit() {
    let mut state = ready(ActivityState::Active);
    assert!(update(&mut state, Message::Configure).is_none());
    load(&mut state, None);
    update(&mut state, Message::Configure);
    update(
        &mut state,
        Message::Select(ActivitySchedulingPriority::Foreground),
    );
    let request = update(&mut state, Message::Save).unwrap();
    let submitted = match &request.action {
        Action::SetSchedulingPriority { request, .. } => request.clone(),
        _ => panic!("expected scheduling-priority set"),
    };
    assert_eq!(submitted.expected_revision, None);
    assert_eq!(submitted.priority, ActivitySchedulingPriority::Foreground);
    let mut created = policy(submitted.priority);
    created.revision = 1;
    let refresh = finish(
        &mut state,
        request,
        Ok(Response::SchedulingPrioritySaved(Box::new(created.clone()))),
    )
    .unwrap();
    assert!(matches!(refresh.action, Action::GetSchedulingPriority(_)));
    finish(&mut state, refresh, Ok(response(Some(created))));
    assert!(state.scheduling_priority.form.is_none());
}

#[test]
fn scheduling_priority_conflicts_retain_the_original_cas_and_selection() {
    let mut state = ready(ActivityState::Paused);
    let previous = policy(ActivitySchedulingPriority::Standard);
    load(&mut state, Some(previous.clone()));
    update(&mut state, Message::Configure);
    update(
        &mut state,
        Message::Select(ActivitySchedulingPriority::Background),
    );
    let request = update(&mut state, Message::Save).unwrap();
    finish(
        &mut state,
        request,
        Err("Conflict: scheduling policy revision changed".into()),
    );
    let retry = update(&mut state, Message::Save).unwrap();
    assert!(matches!(
        &retry.action,
        Action::SetSchedulingPriority { request, .. }
            if request.expected_revision == Some(previous.revision)
                && request.priority == ActivitySchedulingPriority::Background
    ));
    let _ = state.view(true);
}

#[test]
fn scheduling_priority_terminal_state_blocks_mutation_without_starting_work() {
    let mut state = ready(ActivityState::Completed);
    load(
        &mut state,
        Some(policy(ActivitySchedulingPriority::Foreground)),
    );
    assert!(update(&mut state, Message::Configure).is_none());
    assert!(state.pending.is_none());
}

#[test]
fn scheduling_priority_revision_renders_without_a_fluent_placeholder() {
    crate::localize::localize();
    let label = revision_label(9_007_199_254_740_993);
    assert_eq!(
        label.replace(['\u{2068}', '\u{2069}'], ""),
        "Configuration revision: 9007199254740993"
    );
    assert!(!label.contains('{'));
    assert!(!label.contains("$revision"));

    let mut state = ready(ActivityState::Active);
    load(
        &mut state,
        Some(ActivitySchedulingPolicy {
            revision: 9_007_199_254_740_993,
            ..policy(ActivitySchedulingPriority::Foreground)
        }),
    );
    let detail = state.detail.as_ref().unwrap();
    let _ = state.scheduling_priority_view(detail, true);
}
