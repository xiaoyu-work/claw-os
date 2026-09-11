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

fn operation_preview_result() -> ActivityOperationPreview {
    ActivityOperationPreview {
        schema: 1,
        app_id: "kv".into(),
        app_name: "Key/value".into(),
        app_version: "2".into(),
        package_digest: "sha256:preview-package".into(),
        operation: "get".into(),
        operation_label: "Get entry".into(),
        effects_declared: true,
        effects: vec![AppDeclaredEffect {
            kind: AppEffectKind::Read,
            label: "App-declared read".into(),
            recovery: AppEffectRecovery::NotApplicable,
            target_arg: Some("key".into()),
            target_kind: Some(AppEffectTargetKind::Name),
            requested_targets: vec!["release.status".into()],
            target_state: AppEffectTargetState::Requested,
        }],
        unresolved_arguments: vec!["runtime_credential".into()],
        authorization_checked: false,
        executed: false,
        effects_confirmed: false,
        notes: vec!["No App data or credentials read".into()],
    }
}

fn ready_preview(state: ActivityState) -> Activities {
    let mut state = ready(state);
    state.objects = Some(object_response("activity-1"));
    state
}

fn request_preview(state: &mut Activities) -> Request {
    state
        .update(
            Message::PreviewObjectOperation("app://kv/entry?id=release.status".into()),
            true,
        )
        .unwrap()
}

#[test]
fn operation_preview_uses_a_declared_invocation_without_starting_work_or_confirming_effects() {
    for activity_state in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let mut state = ready_preview(activity_state);
        let before = state.detail.clone().unwrap();
        let request = request_preview(&mut state);
        let Action::OperationPreview {
            activity_id,
            reference,
            request: body,
        } = &request.action
        else {
            panic!("preview must only call the metadata route");
        };
        assert_eq!(activity_id, "activity-1");
        assert_eq!(reference, "app://kv/entry?id=release.status");
        assert_eq!(body.app_id, "kv");
        assert_eq!(body.operation, "get");
        assert_eq!(body.args, ["--key", "release.status"]);
        assert!(
            finish(
                &mut state,
                request,
                Ok(Response::OperationPreview(Box::new(
                    operation_preview_result()
                )))
            )
            .is_none()
        );
        assert!(state.operation_preview.is_some());
        assert_eq!(state.detail.as_ref().unwrap(), &before);
        assert!(state.notice.is_none());
        assert!(state.pending.is_none());
    }
}

#[test]
fn operation_preview_has_no_generic_command_or_unverified_object_entry_point() {
    let mut state = ready_preview(ActivityState::Active);
    assert!(
        state
            .update(
                Message::PreviewObjectOperation("unknown reference".into()),
                true
            )
            .is_none()
    );
    for status in [
        ActivityObjectStatus::Unavailable,
        ActivityObjectStatus::Invalid,
    ] {
        state.objects.as_mut().unwrap().objects[0].status = status;
        assert!(
            state
                .update(
                    Message::PreviewObjectOperation("app://kv/entry?id=release.status".into()),
                    true,
                )
                .is_none()
        );
    }
    state.objects.as_mut().unwrap().objects[0].status = ActivityObjectStatus::Declared;
    state.objects.as_mut().unwrap().activity_id = "different-activity".into();
    assert!(
        state
            .update(
                Message::PreviewObjectOperation("app://kv/entry?id=release.status".into()),
                true,
            )
            .is_none()
    );
    assert!(state.pending.is_none());
}

#[test]
fn operation_preview_latest_object_selection_rejects_old_results_and_errors() {
    let mut state = ready_preview(ActivityState::Active);
    let mut second = state.objects.as_ref().unwrap().objects[0].clone();
    second.reference = "app://kv/entry?id=second".into();
    second.description.as_mut().unwrap().invocation.args = vec!["--key".into(), "second".into()];
    state.objects.as_mut().unwrap().objects.push(second);
    let first = request_preview(&mut state);
    let second = state
        .update(
            Message::PreviewObjectOperation("app://kv/entry?id=second".into()),
            true,
        )
        .unwrap();
    assert_ne!(first.generation, second.generation);
    finish(&mut state, first.clone(), Err("stale preview error".into()));
    assert!(state.error.is_none());
    let mut preview = operation_preview_result();
    preview.effects[0].requested_targets = vec!["second".into()];
    finish(
        &mut state,
        second,
        Ok(Response::OperationPreview(Box::new(preview))),
    );
    finish(
        &mut state,
        first,
        Ok(Response::OperationPreview(Box::new(
            operation_preview_result(),
        ))),
    );
    let displayed = state.operation_preview.as_ref().unwrap();
    assert_eq!(displayed.reference, "app://kv/entry?id=second");
    assert_eq!(displayed.preview.effects[0].requested_targets, ["second"]);
}

#[test]
fn operation_preview_navigation_and_drafts_reject_late_responses() {
    let mut state = ready_preview(ActivityState::Active);
    let request = request_preview(&mut state);
    state.hide();
    finish(&mut state, request, Err("stale hidden preview".into()));
    assert!(state.error.is_none());
    assert!(state.operation_preview.is_none());

    for object_draft in [false, true] {
        let mut state = ready_preview(ActivityState::Active);
        let request = request_preview(&mut state);
        if object_draft {
            state.object_form = Some(ActivityObjectAttachRequest::default());
        } else {
            state.form = Some(ActivityCreateRequest::default());
        }
        finish(&mut state, request.clone(), Err("stale draft error".into()));
        finish(
            &mut state,
            request,
            Ok(Response::OperationPreview(Box::new(
                operation_preview_result(),
            ))),
        );
        assert!(state.error.is_none());
        assert!(state.operation_preview.is_none());
        assert!(
            state
                .update(
                    Message::PreviewObjectOperation("app://kv/entry?id=release.status".into()),
                    true,
                )
                .is_none()
        );
    }
}

#[test]
fn operation_preview_revalidates_invocation_snapshot_before_accepting_a_reply() {
    let mut state = ready_preview(ActivityState::Active);
    let request = request_preview(&mut state);
    state.objects.as_mut().unwrap().objects[0]
        .description
        .as_mut()
        .unwrap()
        .invocation
        .args = vec!["changed draft".into()];
    finish(
        &mut state,
        request,
        Ok(Response::OperationPreview(Box::new(
            operation_preview_result(),
        ))),
    );
    assert!(state.operation_preview.is_none());
    assert!(state.error.is_none());
}

#[test]
fn operation_preview_refusals_and_non_metadata_replies_are_visible_not_grants() {
    for field in 0..6 {
        let mut state = ready_preview(ActivityState::Active);
        let request = request_preview(&mut state);
        let mut preview = operation_preview_result();
        match field {
            0 => preview.authorization_checked = true,
            1 => preview.executed = true,
            2 => preview.effects_confirmed = true,
            3 => preview.schema = 2,
            4 => preview.app_id = "other".into(),
            _ => preview.operation = "write".into(),
        }
        finish(
            &mut state,
            request,
            Ok(Response::OperationPreview(Box::new(preview))),
        );
        assert!(state.operation_preview.is_none());
        assert!(state.error.is_some());
        assert_eq!(
            state.detail.as_ref().unwrap().activity.state,
            ActivityState::Active
        );
    }
    let mut state = ready_preview(ActivityState::Active);
    let request = request_preview(&mut state);
    finish(&mut state, request, Err("App manifest unavailable".into()));
    assert_eq!(state.error.as_deref(), Some("App manifest unavailable"));
    assert!(state.operation_preview.is_none());
}

#[test]
fn operation_preview_refresh_and_editing_discard_old_metadata() {
    for message in [
        Message::DescribeObjects,
        Message::Edit,
        Message::NewObject,
        Message::Back,
    ] {
        let mut state = ready_preview(ActivityState::Active);
        let request = request_preview(&mut state);
        finish(
            &mut state,
            request,
            Ok(Response::OperationPreview(Box::new(
                operation_preview_result(),
            ))),
        );
        assert!(state.operation_preview.is_some());
        state.update(message, true);
        assert!(state.operation_preview.is_none());
    }
}

#[test]
fn operation_preview_missing_effects_are_unknown_and_all_metadata_variants_render() {
    crate::localize::localize();
    let mut preview = operation_preview_result();
    preview.effects_declared = false;
    preview.effects.clear();
    preview.operation = "delete".into();
    assert_eq!(
        missing_effect_notice(&preview),
        Some(fl!("activity-preview-unknown"))
    );
    let _ = operation_preview_view(&preview);
    let mut preview = operation_preview_result();
    for (kind, recovery, target_state, target_kind) in [
        (
            AppEffectKind::Read,
            AppEffectRecovery::NotApplicable,
            AppEffectTargetState::Requested,
            Some(AppEffectTargetKind::Path),
        ),
        (
            AppEffectKind::Create,
            AppEffectRecovery::Reversible,
            AppEffectTargetState::Unspecified,
            Some(AppEffectTargetKind::Host),
        ),
        (
            AppEffectKind::Update,
            AppEffectRecovery::Compensatable,
            AppEffectTargetState::Unresolved,
            Some(AppEffectTargetKind::Name),
        ),
        (
            AppEffectKind::Delete,
            AppEffectRecovery::Irreversible,
            AppEffectTargetState::Requested,
            None,
        ),
        (
            AppEffectKind::External,
            AppEffectRecovery::Unknown,
            AppEffectTargetState::Unspecified,
            None,
        ),
        (
            AppEffectKind::Execute,
            AppEffectRecovery::Unknown,
            AppEffectTargetState::Unresolved,
            None,
        ),
    ] {
        preview.effects[0].kind = kind;
        preview.effects[0].recovery = recovery;
        preview.effects[0].target_state = target_state;
        preview.effects[0].target_kind = target_kind;
        preview.effects[0].requested_targets = vec!["../inert;$(value)".into()];
        let _ = operation_preview_view(&preview);
        assert_eq!(preview.effects[0].requested_targets, ["../inert;$(value)"]);
        assert!(preview.is_metadata_only());
    }
}

#[test]
fn operation_preview_draft_changes_invalidate_pending_and_displayed_results() {
    let mut state = ready_preview(ActivityState::Active);
    let request = request_preview(&mut state);
    state.update(Message::Field(Field::Prompt, "New work draft".into()), true);
    assert!(state.pending.is_none());
    finish(&mut state, request, Err("stale preview failure".into()));
    assert!(state.error.is_none());
    assert!(state.operation_preview.is_none());
    let request = request_preview(&mut state);
    finish(
        &mut state,
        request,
        Ok(Response::OperationPreview(Box::new(
            operation_preview_result(),
        ))),
    );
    assert!(state.operation_preview.is_some());
    state.update(
        Message::Field(Field::CompletionNote, "New confirmation draft".into()),
        true,
    );
    assert!(state.operation_preview.is_none());
    assert_eq!(state.prompt, "New work draft");
    assert_eq!(state.completion_note, "New confirmation draft");
}

fn receipt_response(id: &str, outcome: ActivityReceiptOutcome) -> ActivityReceiptsResponse {
    use cos_agent_protocol::{ActivityReceiptReport, ReceiptResultSummary};
    ActivityReceiptsResponse {
        schema: 1,
        activity_id: id.into(),
        receipts: vec![ActivityReceiptView {
            id: "receipt-1".into(),
            activity_id: id.into(),
            received_at: "2026-09-10T21:00:00Z".into(),
            source: ActivityReceiptSource::CallerReported,
            report: ActivityReceiptReport {
                id: "caller-report-1".into(),
                app_id: "kv".into(),
                operation: "get".into(),
                package_digest: "reported-package".into(),
                outcome,
                result: Some(ReceiptResultSummary {
                    kind: ReceiptResultKind::Json,
                    sha256: "reported-output".into(),
                    bytes: 4096,
                    preview: r#"{"goal_completed":true,"outcome":"applied","os_confirmed":true}"#
                        .into(),
                    preview_truncated: true,
                }),
                error: Some("[REDACTED] caller error".into()),
            },
            declaration: None,
            declaration_error: Some("package changed or revoked".into()),
        }],
    }
}

#[test]
fn receipts_are_readable_in_all_states_without_job_or_goal_completion_inference() {
    for activity_state in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        for outcome in [
            ActivityReceiptOutcome::Returned,
            ActivityReceiptOutcome::ReportedError,
            ActivityReceiptOutcome::Indeterminate,
        ] {
            let mut state = ready(activity_state);
            let before = state.detail.clone().unwrap();
            let request = state.update(Message::RefreshReceipts, true).unwrap();
            assert!(matches!(&request.action, Action::Receipts(id) if id == "activity-1"));
            assert!(
                finish(
                    &mut state,
                    request,
                    Ok(Response::Receipts(receipt_response("activity-1", outcome)))
                )
                .is_none()
            );
            assert_eq!(state.detail.as_ref().unwrap(), &before);
            assert_eq!(
                state.receipts.as_ref().unwrap().receipts[0].report.outcome,
                outcome
            );
            assert!(state.pending.is_none());
            assert!(state.notice.is_none());
            let _ = state.view(true);
        }
    }
}

#[test]
fn receipt_selection_changes_reject_old_results_and_errors() {
    let mut state = ready(ActivityState::Active);
    let old = state.update(Message::RefreshReceipts, true).unwrap();
    let current = state
        .update(Message::Open("activity-2".into()), true)
        .unwrap();
    finish(&mut state, old.clone(), Err("stale receipt error".into()));
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
        Ok(Response::Receipts(receipt_response(
            "activity-1",
            ActivityReceiptOutcome::Returned,
        ))),
    );
    assert!(state.receipts.is_none());
    assert_eq!(state.detail.as_ref().unwrap().activity.id, "activity-2");
}

#[test]
fn receipt_refresh_is_explicit_and_failures_do_not_become_empty_success() {
    let mut state = ready(ActivityState::Active);
    let poll = state.update(Message::Tick, true).unwrap();
    assert!(matches!(poll.action, Action::Get(_)));
    finish(
        &mut state,
        poll,
        Ok(Response::Detail(Box::new(detail(
            "activity-1",
            ActivityState::Active,
        )))),
    );
    assert!(state.receipts.is_none());
    state.receipts = Some(receipt_response(
        "activity-1",
        ActivityReceiptOutcome::Returned,
    ));
    let request = state.update(Message::RefreshReceipts, true).unwrap();
    assert!(state.receipts.is_none());
    assert!(state.update(Message::RefreshReceipts, true).is_none());
    finish(
        &mut state,
        request,
        Err("receipt service unavailable".into()),
    );
    assert_eq!(state.error.as_deref(), Some("receipt service unavailable"));
    assert!(state.receipts.is_none());
    assert!(!state.should_poll());
}

#[test]
fn receipt_scope_mismatches_and_unknown_schemas_are_visible_errors() {
    for mismatch in 0..3 {
        let mut state = ready(ActivityState::Active);
        let request = state.update(Message::RefreshReceipts, true).unwrap();
        let mut response = receipt_response("activity-1", ActivityReceiptOutcome::Returned);
        match mismatch {
            0 => response.schema = 2,
            1 => response.activity_id = "other".into(),
            _ => response.receipts[0].activity_id = "other".into(),
        }
        finish(&mut state, request, Ok(Response::Receipts(response)));
        assert!(state.error.is_some());
        assert!(state.receipts.is_none());
        assert_eq!(
            state.detail.as_ref().unwrap().activity.state,
            ActivityState::Active
        );
    }
}

#[test]
fn receipt_reads_preserve_drafts_and_existing_object_previews() {
    let mut state = ready_preview(ActivityState::Active);
    let preview = request_preview(&mut state);
    finish(
        &mut state,
        preview,
        Ok(Response::OperationPreview(Box::new(
            operation_preview_result(),
        ))),
    );
    let before = state.operation_preview.as_ref().unwrap().preview.clone();
    let request = state.update(Message::RefreshReceipts, true).unwrap();
    finish(
        &mut state,
        request,
        Ok(Response::Receipts(receipt_response(
            "activity-1",
            ActivityReceiptOutcome::Returned,
        ))),
    );
    assert_eq!(state.operation_preview.as_ref().unwrap().preview, before);
    state.update(Message::Edit, true);
    state.update(Message::Field(Field::Goal, "Unsent edit".into()), true);
    assert!(state.update(Message::RefreshReceipts, true).is_none());
    assert_eq!(state.form.as_ref().unwrap().goal, "Unsent edit");
    state.form = None;
    fill_object_form(&mut state);
    assert!(state.update(Message::RefreshReceipts, true).is_none());
    assert_eq!(
        state.object_form.as_ref().unwrap().object.object_id,
        "release.status"
    );
}

#[test]
fn receipt_reports_and_declarations_render_as_inert_separate_data() {
    use cos_agent_protocol::ActivityReceiptDeclaration;
    crate::localize::localize();
    let mut response = receipt_response("activity-1", ActivityReceiptOutcome::Returned);
    let receipt = &mut response.receipts[0];
    let raw = "**applied** [Approve](https://invalid.example) <script>not executed</script>";
    for kind in [
        ReceiptResultKind::Json,
        ReceiptResultKind::Text,
        ReceiptResultKind::Empty,
    ] {
        let result = receipt.report.result.as_mut().unwrap();
        result.kind = kind;
        result.preview = raw.into();
        result.bytes = u64::MAX;
        let _ = receipt_view(receipt);
        assert_eq!(receipt.report.result.as_ref().unwrap().preview, raw);
        assert_eq!(receipt.report.result.as_ref().unwrap().bytes, u64::MAX);
        assert_eq!(receipt.source, ActivityReceiptSource::CallerReported);
    }
    receipt.report.result = None;
    receipt.report.error = Some(raw.into());
    receipt.declaration = Some(ActivityReceiptDeclaration {
        app_version: "1".into(),
        operation_label: "App-declared operation".into(),
        effects: vec![ReceiptDeclaredEffect {
            kind: AppEffectKind::Delete,
            label: "App-declared deletion".into(),
            recovery: AppEffectRecovery::Reversible,
            target_arg: Some("path".into()),
        }],
    });
    receipt.declaration_error = None;
    let _ = receipt_view(receipt);
    assert_eq!(receipt.report.outcome, ActivityReceiptOutcome::Returned);
    receipt.declaration.as_mut().unwrap().effects.clear();
    let _ = receipt_view(receipt);
    receipt.declaration = None;
    let _ = receipt_view(receipt);
    assert_eq!(
        receipt_outcome_label(ActivityReceiptOutcome::Returned),
        fl!("activity-receipt-returned")
    );
    assert_eq!(
        receipt_outcome_label(ActivityReceiptOutcome::ReportedError),
        fl!("activity-receipt-reported-error")
    );
    assert_eq!(
        receipt_outcome_label(ActivityReceiptOutcome::Indeterminate),
        fl!("activity-receipt-indeterminate")
    );
}

#[test]
fn leaving_the_activity_discards_receipt_responses_without_authoring_anything() {
    let mut state = ready(ActivityState::Active);
    let request = state.update(Message::RefreshReceipts, true).unwrap();
    state.hide();
    finish(
        &mut state,
        request,
        Ok(Response::Receipts(receipt_response(
            "activity-1",
            ActivityReceiptOutcome::Returned,
        ))),
    );
    assert!(state.receipts.is_none());
    assert!(state.pending.is_none());
    assert!(state.update(Message::RefreshReceipts, true).is_none());
    let request = state.update(Message::Show, true).unwrap();
    assert!(matches!(request.action, Action::Get(_)));
}

#[test]
fn receipt_snapshot_stays_historical_and_caller_reported_when_app_metadata_changes() {
    use cos_agent_protocol::ActivityReceiptDeclaration;
    let mut state = ready_preview(ActivityState::Active);
    state.objects.as_mut().unwrap().objects[0]
        .description
        .as_mut()
        .unwrap()
        .app_version = "new-package-version".into();
    let mut response = receipt_response("activity-1", ActivityReceiptOutcome::Returned);
    response.receipts[0].declaration_error = None;
    response.receipts[0].declaration = Some(ActivityReceiptDeclaration {
        app_version: "version-at-recording".into(),
        operation_label: "Historical operation".into(),
        effects: vec![],
    });
    let request = state.update(Message::RefreshReceipts, true).unwrap();
    finish(&mut state, request, Ok(Response::Receipts(response)));
    let receipt = &state.receipts.as_ref().unwrap().receipts[0];
    assert_eq!(receipt.source, ActivityReceiptSource::CallerReported);
    assert_eq!(receipt.report.outcome, ActivityReceiptOutcome::Returned);
    assert_eq!(
        receipt.declaration.as_ref().unwrap().app_version,
        "version-at-recording"
    );
    assert_eq!(
        state.objects.as_ref().unwrap().objects[0]
            .description
            .as_ref()
            .unwrap()
            .app_version,
        "new-package-version",
    );
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Active
    );
    let _ = state.view(true);
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
