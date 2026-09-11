use super::*;

fn detail(id: &str, state: ActivityState) -> ActivityDetailResponse {
    ActivityDetailResponse {
        activity: ActivityView {
            id: id.into(),
            title: "Prepare a release".into(),
            goal: "Publish a verified release".into(),
            state,
            completion_criteria: "Tests and review".into(),
            boundaries: "Ask before publishing".into(),
            resources: vec![ActivityResource {
                label: "Notes".into(),
                reference: "notes.txt".into(),
            }],
            completion_note: None,
            created_at: "2026-09-10T12:00:00Z".into(),
            updated_at: "2026-09-10T12:00:00Z".into(),
        },
        jobs: vec![ActivityJobView {
            id: "job-1".into(),
            status: "ok".into(),
            title: "Release checks".into(),
            session_id: Some("session-1".into()),
            created_at: "2026-09-10T12:00:00Z".into(),
            finished_at: Some("2026-09-10T12:01:00Z".into()),
            response: Some("Checks passed".into()),
            error: None,
            waiting_on: vec![],
        }],
        sessions: vec!["session-1".into()],
        pending_approvals: vec![],
        approvals_error: None,
    }
}

fn ready(state: ActivityState) -> Activities {
    crate::localize::localize();
    Activities {
        visible: true,
        selected: Some("activity-1".into()),
        detail: Some(detail("activity-1", state)),
        ..Activities::default()
    }
}

fn finish(
    state: &mut Activities,
    request: Request,
    response: Result<Response, String>,
) -> Option<Request> {
    state.update(
        Message::Loaded {
            generation: request.generation,
            result: response,
        },
        true,
    )
}

#[test]
fn activity_creation_waits_for_backend_identity_and_fetches_detail() {
    let mut state = ready(ActivityState::Active);
    state.update(Message::New, true);
    state.update(Message::Field(Field::Title, "New goal".into()), true);
    state.update(Message::Field(Field::Goal, "Do the work".into()), true);
    let request = state.update(Message::Save, true).unwrap();
    assert!(matches!(&request.action, Action::Create(body) if body.title == "New goal"));
    assert!(state.list.is_empty());
    assert!(state.detail.is_none());
    assert!(state.selected.is_none());

    let fetched = detail("backend-created-id", ActivityState::Active);
    let next = finish(
        &mut state,
        request,
        Ok(Response::Saved(Box::new(fetched.activity.clone()))),
    )
    .unwrap();
    assert!(matches!(&next.action, Action::Get(id) if id == "backend-created-id"));
    assert!(state.detail.is_none());
    finish(&mut state, next, Ok(Response::Detail(Box::new(fetched))));
    assert_eq!(
        state.detail.as_ref().unwrap().activity.id,
        "backend-created-id"
    );
}

#[test]
fn successful_job_never_marks_the_goal_complete() {
    let mut state = ready(ActivityState::Active);
    let request = state.update(Message::Run, true).unwrap();
    assert!(matches!(&request.action, Action::Run(id, _) if id == "activity-1"));
    assert!(
        state.update(Message::Run, true).is_none(),
        "no duplicate admission while pending"
    );
    let next = finish(
        &mut state,
        request,
        Ok(Response::Work(ActivityWorkResponse {
            id: "job-2".into(),
            status: "ok".into(),
            session_id: Some("session-2".into()),
            activity_id: Some("activity-1".into()),
        })),
    )
    .unwrap();
    assert!(matches!(&next.action, Action::Get(id) if id == "activity-1"));
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Active
    );
    finish(
        &mut state,
        next,
        Ok(Response::Detail(Box::new(detail(
            "activity-1",
            ActivityState::Active,
        )))),
    );
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Active
    );
    assert!(
        state
            .detail
            .as_ref()
            .unwrap()
            .activity
            .completion_note
            .is_none()
    );
}

#[test]
fn completion_requires_a_user_note_and_never_applies_optimistically() {
    let mut state = ready(ActivityState::Active);
    assert!(
        state
            .update(Message::Transition(ActivityState::Completed), true)
            .is_none()
    );
    assert!(state.error.is_some());
    state.update(
        Message::Field(Field::CompletionNote, "I checked every result".into()),
        true,
    );
    let request = state
        .update(Message::Transition(ActivityState::Completed), true)
        .unwrap();
    assert!(matches!(&request.action, Action::Transition(_, body)
        if body.state == ActivityState::Completed
        && body.completion_note.as_deref() == Some("I checked every result")));
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Active
    );
    finish(&mut state, request, Err("completion refused".into()));
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Active
    );
    assert_eq!(state.completion_note, "I checked every result");
    assert_eq!(state.error.as_deref(), Some("completion refused"));
    assert!(
        !state.should_poll(),
        "a refresh must not erase the failure banner"
    );
}

#[test]
fn pause_resume_cancel_and_reopen_are_requests_not_local_transitions() {
    for (initial, target) in [
        (ActivityState::Active, ActivityState::Paused),
        (ActivityState::Paused, ActivityState::Active),
        (ActivityState::Active, ActivityState::Cancelled),
        (ActivityState::Completed, ActivityState::Active),
        (ActivityState::Cancelled, ActivityState::Active),
    ] {
        let mut state = ready(initial);
        let request = state.update(Message::Transition(target), true).unwrap();
        assert!(matches!(request.action, Action::Transition(_, body) if body.state == target));
        assert_eq!(state.detail.as_ref().unwrap().activity.state, initial);
        assert_eq!(state.detail.as_ref().unwrap().jobs[0].status, "ok");
    }
}

#[test]
fn leaving_the_view_discards_late_responses_without_cancelling_durable_work() {
    let mut state = ready(ActivityState::Active);
    let request = state.update(Message::Run, true).unwrap();
    state.hide();
    assert!(!state.is_visible());
    assert!(!state.should_poll());
    assert!(state.pending.is_none());
    assert!(
        finish(
            &mut state,
            request,
            Ok(Response::Work(ActivityWorkResponse {
                id: "durable-job".into(),
                status: "running".into(),
                activity_id: Some("activity-1".into()),
                session_id: None,
            }))
        )
        .is_none()
    );
    assert!(state.notice.is_none());
    let refresh = state.update(Message::Show, true).unwrap();
    assert!(matches!(refresh.action, Action::Get(id) if id == "activity-1"));
}

#[test]
fn stale_results_and_errors_cannot_overwrite_new_selection() {
    let mut state = ready(ActivityState::Active);
    let old = state.update(Message::Refresh, true).unwrap();
    let current = state
        .update(Message::Open("activity-2".into()), true)
        .unwrap();
    finish(&mut state, old.clone(), Err("old transport failure".into()));
    assert!(state.error.is_none());
    assert!(state.pending.is_some());
    finish(
        &mut state,
        current,
        Ok(Response::Detail(Box::new(detail(
            "activity-2",
            ActivityState::Paused,
        )))),
    );
    finish(
        &mut state,
        old,
        Ok(Response::Detail(Box::new(detail(
            "activity-1",
            ActivityState::Completed,
        )))),
    );
    assert_eq!(state.detail.as_ref().unwrap().activity.id, "activity-2");
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Paused
    );
}

#[test]
fn wrong_response_identity_and_shape_are_visible_errors() {
    let mut state = ready(ActivityState::Active);
    let request = state.update(Message::Refresh, true).unwrap();
    finish(
        &mut state,
        request,
        Ok(Response::Detail(Box::new(detail(
            "other",
            ActivityState::Completed,
        )))),
    );
    assert!(state.error.is_some());
    assert_eq!(state.detail.as_ref().unwrap().activity.id, "activity-1");
    let request = state.update(Message::Refresh, true).unwrap();
    finish(
        &mut state,
        request,
        Ok(Response::List(ActivityListResponse::default())),
    );
    assert!(state.error.is_some());
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Active
    );
}

#[test]
fn edits_preserve_inert_resources_and_refresh_never_overwrites_forms() {
    let mut state = ready(ActivityState::Paused);
    state.update(Message::Edit, true);
    state.update(Message::Field(Field::Goal, "Revised goal".into()), true);
    state.update(
        Message::ResourceReference(0, "javascript:never-execute-this".into()),
        true,
    );
    assert!(!state.should_poll());
    assert!(state.update(Message::Tick, true).is_none());
    assert!(state.update(Message::Refresh, true).is_none());
    let request = state.update(Message::Save, true).unwrap();
    let Action::Update(id, body) = &request.action else {
        panic!("edits must go to the shared update route");
    };
    assert_eq!(id, "activity-1");
    assert_eq!(body.goal.as_deref(), Some("Revised goal"));
    assert_eq!(
        body.resources.as_ref().unwrap()[0].reference,
        "javascript:never-execute-this"
    );
    assert_eq!(
        state.detail.as_ref().unwrap().activity.goal,
        "Publish a verified release"
    );
    finish(&mut state, request, Err("update refused".into()));
    assert_eq!(state.form.as_ref().unwrap().goal, "Revised goal");
    assert!(state.error.is_some());
}

#[test]
fn continuation_uses_only_fetched_sessions_and_does_not_claim_authority() {
    let mut state = ready(ActivityState::Active);
    state.update(Message::UseSession(Some("unrelated".into())), true);
    assert!(state.continue_session.is_none());
    state.update(Message::UseSession(Some("session-1".into())), true);
    state.update(
        Message::Field(Field::Prompt, "Continue the checks".into()),
        true,
    );
    let request = state.update(Message::Run, true).unwrap();
    let Action::Run(_, body) = request.action else {
        panic!("expected Activity run")
    };
    assert_eq!(body.session_id.as_deref(), Some("session-1"));
    assert_eq!(body.prompt.as_deref(), Some("Continue the checks"));
    let value = serde_json::to_value(body).unwrap();
    assert!(value.get("owner_uid").is_none());
    assert!(value.get("caps").is_none());
    assert!(
        value.get("boundaries").is_none(),
        "planning context is assembled by the backend"
    );
    assert!(state.session_title("unrelated").is_none());
}

#[test]
fn job_controls_are_separate_from_the_activity_lifecycle() {
    let mut state = ready(ActivityState::Cancelled);
    assert!(
        state
            .update(Message::CancelJob("unknown".into()), true)
            .is_none()
    );
    let request = state
        .update(Message::CancelJob("job-1".into()), true)
        .unwrap();
    assert!(matches!(&request.action, Action::CancelJob(id) if id == "job-1"));
    let next = finish(
        &mut state,
        request,
        Ok(Response::JobCancellation(CancelResponse {
            id: "job-1".into(),
            status: "running".into(),
            cancelled: false,
            cancel_requested: true,
            reason: None,
        })),
    )
    .unwrap();
    assert!(matches!(next.action, Action::Get(_)));
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Cancelled
    );
    assert_eq!(state.detail.as_ref().unwrap().jobs[0].status, "ok");
}

#[test]
fn disconnected_work_is_an_error_without_fake_admission() {
    let mut state = ready(ActivityState::Active);
    assert!(state.update(Message::Run, false).is_none());
    assert!(state.error.is_some());
    assert!(state.pending.is_none());
    assert!(state.notice.is_none());
    assert_eq!(state.detail.as_ref().unwrap().jobs.len(), 1);
}

#[test]
fn refresh_does_not_swallow_work_or_completion_drafts() {
    let mut state = ready(ActivityState::Active);
    let request = state.update(Message::Refresh, true).unwrap();
    state.update(Message::Field(Field::Prompt, "Next step".into()), true);
    state.update(
        Message::Field(Field::CompletionNote, "Verified the result".into()),
        true,
    );
    finish(
        &mut state,
        request,
        Ok(Response::Detail(Box::new(detail(
            "activity-1",
            ActivityState::Active,
        )))),
    );
    assert_eq!(state.prompt, "Next step");
    assert_eq!(state.completion_note, "Verified the result");
    assert!(!state.should_poll());
    let request = state.update(Message::Run, true).unwrap();
    state.update(Message::Field(Field::Prompt, "Different step".into()), true);
    assert_eq!(
        state.prompt, "Next step",
        "mutating requests retain their form snapshot"
    );
    finish(&mut state, request, Err("submission refused".into()));
    assert_eq!(state.prompt, "Next step");
}

#[test]
fn switching_to_chat_preserves_unsent_forms_but_does_not_replay_pending_saves() {
    let mut state = ready(ActivityState::Active);
    state.update(Message::Edit, true);
    state.update(Message::Field(Field::Goal, "Unsaved goal".into()), true);
    state.hide();
    assert!(state.update(Message::Show, true).is_none());
    assert_eq!(state.form.as_ref().unwrap().goal, "Unsaved goal");

    let request = state.update(Message::Save, true).unwrap();
    state.hide();
    assert!(state.form.is_none());
    finish(
        &mut state,
        request,
        Ok(Response::Saved(Box::new(
            detail("activity-1", ActivityState::Active).activity,
        ))),
    );
    assert!(matches!(
        state.update(Message::Show, true).unwrap().action,
        Action::Get(_)
    ));
}

fn object_response(id: &str) -> ActivityObjectsResponse {
    use cos_agent_protocol::{ActivityObjectResourceView, AppObjectInvocation, AppObjectReference};
    ActivityObjectsResponse {
        activity_id: id.into(),
        objects: vec![ActivityObjectResourceView {
            label: "Release status".into(),
            reference: "app://kv/entry?id=release.status".into(),
            status: ActivityObjectStatus::Declared,
            description: Some(AppObjectDescription {
                object: AppObjectReference {
                    app_id: "kv".into(),
                    object_type: "entry".into(),
                    object_id: "release.status".into(),
                    revision: None,
                },
                reference: "app://kv/entry?id=release.status".into(),
                app_name: "Key/value".into(),
                app_version: "1".into(),
                object_label: "Entry".into(),
                object_summary: "Key/value entry".into(),
                invocation: AppObjectInvocation {
                    app_id: "kv".into(),
                    operation: "get".into(),
                    args: vec!["--key".into(), "release.status".into()],
                },
            }),
            error: None,
        }],
    }
}

fn fill_object_form(state: &mut Activities) {
    state.update(Message::NewObject, true);
    for (field, value) in [
        (ObjectField::Label, "Release status"),
        (ObjectField::AppId, "kv"),
        (ObjectField::ObjectType, "entry"),
        (ObjectField::ObjectId, "release.status"),
    ] {
        state.update(Message::ObjectField(field, value.into()), true);
    }
}

#[test]
fn object_attachment_uses_typed_opaque_components_and_preserves_failed_forms() {
    let mut state = ready(ActivityState::Paused);
    fill_object_form(&mut state);
    state.update(
        Message::ObjectField(ObjectField::ObjectId, " a/b?x=1&y=2 ".into()),
        true,
    );
    state.update(
        Message::ObjectField(ObjectField::Revision, "version/#? ".into()),
        true,
    );
    assert!(!state.should_poll());
    let request = state.update(Message::AttachObject, true).unwrap();
    let Action::AttachObject(id, body) = &request.action else {
        panic!("expected attachment")
    };
    assert_eq!(id, "activity-1");
    assert_eq!(body.object.object_id, " a/b?x=1&y=2 ");
    assert_eq!(body.object.revision.as_deref(), Some("version/#? "));
    let encoded = serde_json::to_value(body).unwrap();
    assert!(encoded.get("owner_uid").is_none());
    assert!(encoded.get("reference").is_none());
    assert_eq!(
        state.detail.as_ref().unwrap().activity.resources[0].reference,
        "notes.txt"
    );
    assert!(state.update(Message::AttachObject, true).is_none());
    finish(&mut state, request, Err("App signature was revoked".into()));
    assert_eq!(state.error.as_deref(), Some("App signature was revoked"));
    assert_eq!(
        state.object_form.as_ref().unwrap().object.object_id,
        " a/b?x=1&y=2 "
    );
    assert_eq!(state.detail.as_ref().unwrap().activity.resources.len(), 1);
}

#[test]
fn object_attachment_refetches_metadata_and_descriptions_without_local_upsert() {
    for activity_state in [ActivityState::Active, ActivityState::Paused] {
        let mut state = ready(activity_state);
        state.completion_note = "Unsent confirmation".into();
        fill_object_form(&mut state);
        let request = state.update(Message::AttachObject, true).unwrap();
        assert_eq!(state.detail.as_ref().unwrap().activity.resources.len(), 1);
        let mut fetched = detail("activity-1", activity_state);
        fetched.activity.resources.push(ActivityResource {
            label: "Canonical label".into(),
            reference: "app://kv/entry?id=release.status".into(),
        });
        let refresh = finish(
            &mut state,
            request,
            Ok(Response::Saved(Box::new(fetched.activity.clone()))),
        )
        .unwrap();
        assert!(matches!(refresh.action, Action::Get(_)));
        assert!(state.object_form.is_none());
        assert!(state.detail.is_none());
        let describe =
            finish(&mut state, refresh, Ok(Response::Detail(Box::new(fetched)))).unwrap();
        assert!(matches!(describe.action, Action::Objects(_)));
        assert_eq!(
            state.detail.as_ref().unwrap().activity.resources[0].reference,
            "notes.txt"
        );
        assert_eq!(
            state.detail.as_ref().unwrap().activity.resources[1].label,
            "Canonical label"
        );
        assert!(
            finish(
                &mut state,
                describe,
                Ok(Response::Objects(object_response("activity-1")))
            )
            .is_none()
        );
        assert_eq!(
            state.detail.as_ref().unwrap().activity.state,
            activity_state
        );
        assert_eq!(state.completion_note, "Unsent confirmation");
    }
}

#[test]
fn object_attachment_preserves_terminal_rules_and_does_not_submit_empty_fields() {
    let mut state = ready(ActivityState::Active);
    state.update(Message::NewObject, true);
    assert!(state.update(Message::AttachObject, true).is_none());
    assert!(state.error.is_some());
    for terminal in [ActivityState::Completed, ActivityState::Cancelled] {
        let mut state = ready(terminal);
        state.update(Message::NewObject, true);
        assert!(state.object_form.is_none());
        assert!(state.error.is_some());
        state.object_form = Some(ActivityObjectAttachRequest::default());
        assert!(state.update(Message::AttachObject, true).is_none());
        state.object_form = None;
        let request = state.update(Message::DescribeObjects, true).unwrap();
        assert!(
            matches!(request.action, Action::Objects(_)),
            "terminal goals remain readable"
        );
    }
}

#[test]
fn object_declaration_is_read_only_and_stale_errors_cannot_replace_new_details() {
    let mut state = ready(ActivityState::Active);
    let request = state.update(Message::DescribeObjects, true).unwrap();
    let old = request.clone();
    assert!(
        finish(
            &mut state,
            request,
            Ok(Response::Objects(object_response("activity-1")))
        )
        .is_none()
    );
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Active
    );
    assert!(
        state
            .detail
            .as_ref()
            .unwrap()
            .activity
            .completion_note
            .is_none()
    );
    let next = state
        .update(Message::Open("activity-2".into()), true)
        .unwrap();
    finish(&mut state, old.clone(), Err("stale signature error".into()));
    assert!(state.error.is_none());
    assert!(state.objects.is_none());
    finish(
        &mut state,
        next,
        Ok(Response::Detail(Box::new(detail(
            "activity-2",
            ActivityState::Active,
        )))),
    );
    finish(
        &mut state,
        old,
        Ok(Response::Objects(object_response("activity-1"))),
    );
    assert!(state.objects.is_none());
    assert_eq!(state.detail.as_ref().unwrap().activity.id, "activity-2");
}

#[test]
fn object_identity_mismatches_and_lookup_failures_remain_visible() {
    let mut state = ready(ActivityState::Active);
    let request = state.update(Message::DescribeObjects, true).unwrap();
    finish(
        &mut state,
        request,
        Ok(Response::Objects(object_response("other-activity"))),
    );
    assert!(state.error.is_some());
    assert!(state.objects.is_none());
    let request = state.update(Message::DescribeObjects, true).unwrap();
    finish(
        &mut state,
        request,
        Err("declaration service unavailable".into()),
    );
    assert_eq!(
        state.error.as_deref(),
        Some("declaration service unavailable")
    );
    assert!(state.objects.is_none());
    assert!(!state.should_poll());
}

#[test]
fn object_descriptions_are_invalidated_when_ordinary_resource_edits_change_references() {
    let mut state = ready(ActivityState::Active);
    state.objects = Some(object_response("activity-1"));
    let request = state.update(Message::Refresh, true).unwrap();
    let mut fetched = detail("activity-1", ActivityState::Active);
    fetched.activity.resources.clear();
    let next = finish(&mut state, request, Ok(Response::Detail(Box::new(fetched)))).unwrap();
    assert!(state.objects.is_none());
    assert!(matches!(next.action, Action::Objects(_)));
}

#[test]
fn object_forms_survive_navigation_but_pending_attachments_are_not_replayed() {
    let mut state = ready(ActivityState::Active);
    fill_object_form(&mut state);
    state.hide();
    assert!(state.update(Message::Show, true).is_none());
    assert_eq!(
        state.object_form.as_ref().unwrap().object.object_id,
        "release.status"
    );
    let request = state.update(Message::AttachObject, true).unwrap();
    state.hide();
    assert!(state.object_form.is_none());
    assert!(
        finish(
            &mut state,
            request,
            Ok(Response::Saved(Box::new(
                detail("activity-1", ActivityState::Active).activity
            )))
        )
        .is_none()
    );
    let next = state.update(Message::Show, true).unwrap();
    assert!(matches!(next.action, Action::Get(_)));
}

#[test]
fn object_invocation_details_are_inert_and_unverified_descriptions_are_not_copyable() {
    let mut state = ready(ActivityState::Active);
    let mut response = object_response("activity-1");
    response.objects[0]
        .description
        .as_mut()
        .unwrap()
        .invocation
        .args = vec![
        "value with spaces".into(),
        "line\nbreak;not a command".into(),
    ];
    state.objects = Some(response);
    let copied = state
        .object_operation_text("app://kv/entry?id=release.status")
        .unwrap();
    assert!(copied.contains(&fl!("activity-object-operation-name", operation = "get")));
    assert!(copied.contains("\"value with spaces\""));
    assert!(copied.contains("\"line\\nbreak;not a command\""));
    assert!(state.pending.is_none());
    for status in [
        ActivityObjectStatus::Declared,
        ActivityObjectStatus::Unavailable,
        ActivityObjectStatus::Invalid,
    ] {
        let object = &mut state.objects.as_mut().unwrap().objects[0];
        object.status = status;
        object.error =
            (status != ActivityObjectStatus::Declared).then(|| "retained diagnostic".into());
        let _ = state.view(true);
        if status != ActivityObjectStatus::Declared {
            assert!(
                state
                    .object_operation_text("app://kv/entry?id=release.status")
                    .is_none()
            );
        }
    }
    fill_object_form(&mut state);
    let _ = state.view(true);
}

#[test]
fn object_forms_do_not_overlap_metadata_edits_or_replay_stale_controls() {
    let mut state = ready(ActivityState::Active);
    state.update(Message::Edit, true);
    state.update(Message::NewObject, true);
    assert!(state.object_form.is_none());
    assert!(state.form.is_some());
    state.form = None;
    fill_object_form(&mut state);
    for message in [
        Message::NewObject,
        Message::Edit,
        Message::Run,
        Message::Transition(ActivityState::Cancelled),
    ] {
        assert!(state.update(message, true).is_none());
    }
    assert!(state.form.is_none());
    assert_eq!(
        state.object_form.as_ref().unwrap().object.object_id,
        "release.status"
    );
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Active
    );
}

#[test]
fn object_cards_keep_original_references_separate_from_canonical_descriptions() {
    let mut state = ready(ActivityState::Active);
    let mut response = object_response("activity-1");
    response.objects[0].reference = "app://kv/entry?id=release%2Estatus".into();
    state.objects = Some(response);
    let _ = state.view(true);
    assert!(
        state
            .object_operation_text("app://kv/entry?id=release%2Estatus")
            .is_some()
    );
    assert!(
        state
            .object_operation_text("app://kv/entry?id=release.status")
            .is_none()
    );
}

#[test]
fn object_invocation_arguments_round_trip_as_opaque_json_argv() {
    let mut state = ready(ActivityState::Active);
    let args = [
        "",
        "two words",
        "a'b\"c\\d",
        "; | & $() > < * ?",
        "line\nbreak\t\0\u{001b}",
        "\u{03bb}",
        r#"{"operation":"data, not execution"}"#,
    ]
    .map(str::to_string)
    .to_vec();
    for args in [Vec::new(), args] {
        let mut response = object_response("activity-1");
        response.objects[0]
            .description
            .as_mut()
            .unwrap()
            .invocation
            .args = args.clone();
        state.objects = Some(response);
        let copied = state
            .object_operation_text("app://kv/entry?id=release.status")
            .unwrap();
        let marker = format!("{}\n", fl!("activity-object-operation-args"));
        let (_, argv) = copied.split_once(&marker).unwrap();
        assert_eq!(serde_json::from_str::<Vec<String>>(argv).unwrap(), args);
        assert!(state.pending.is_none());
    }
}

#[test]
fn native_views_build_for_list_forms_and_all_backend_states() {
    for activity_state in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let mut state = ready(activity_state);
        let _ = state.view(true);
        if editable(activity_state) {
            state.update(Message::Edit, true);
            let _ = state.view(true);
        }
        state.clear_detail();
        let _ = state.view(false);
        state.update(Message::New, true);
        let _ = state.view(true);
    }
}
