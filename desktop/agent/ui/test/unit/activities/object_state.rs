use super::*;
use crate::activities::Response;
use cos_agent_protocol::{
    ActivityReceiptOutcome, ActivityReceiptReport, ActivityState, ReceiptResultKind,
    ReceiptResultSummary,
};
use serde_json::json;

const ACTIVITY: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_ACTIVITY: &str = "55555555-5555-4555-8555-555555555555";
const ORIGINAL: &str = "22222222-2222-4222-8222-222222222222";
const SUCCESSOR: &str = "33333333-3333-4333-8333-333333333333";
const RECEIPT: &str = "44444444-4444-4444-8444-444444444444";
const REFERENCE: &str = "app://kv/entry?id=release.status";
const TARGET: &str = "app://kv/entry?id=notes%2Fa%3Fb%3D1";

fn ready(state: ActivityState) -> Activities {
    crate::localize::localize();
    let detail: ActivityDetailResponse = serde_json::from_value(json!({
        "activity": {
            "id": ACTIVITY, "title": "Release", "goal": "Prepare the release", "state": state,
            "resources": [
                {"label": "Status", "reference": REFERENCE},
                {"label": "Notes", "reference": TARGET},
                {"label": "Ordinary resource", "reference": "notes.txt"}
            ]
        },
        "jobs": [{"id": "existing-job", "status": "ok"}]
    }))
    .unwrap();
    Activities {
        visible: true,
        selected: Some(ACTIVITY.into()),
        detail: Some(detail),
        ..Activities::default()
    }
}

fn update(state: &mut Activities, message: Message) -> Option<Request> {
    state.update(ActivityMessage::ObjectState(message), true)
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

fn submitted(request: &Request) -> (&str, &ObjectStateDraft) {
    match &request.action {
        Action::RecordObjectState {
            activity_id,
            request,
        } => (activity_id, &request.entry),
        _ => panic!("expected an object-state record request"),
    }
}

fn snapshot(activity_id: &str, draft: ObjectStateDraft) -> ObjectStateEntry {
    let receipt = match &draft.content {
        ObjectStateContent::AppReport { receipt_id } => Some(ActivityReceiptReport {
            id: receipt_id.clone(),
            app_id: "kv".into(),
            operation: "get".into(),
            package_digest: "reported-package-digest".into(),
            outcome: ActivityReceiptOutcome::Indeterminate,
            result: Some(ReceiptResultSummary {
                kind: ReceiptResultKind::Text,
                sha256: "reported-result-digest".into(),
                bytes: 9000,
                preview: "A bounded caller report".into(),
                preview_truncated: true,
            }),
            error: Some("Reported uncertainty".into()),
        }),
        _ => None,
    };
    ObjectStateEntry {
        id: draft.id.clone(),
        activity_id: activity_id.into(),
        owner_uid: 1000,
        recorded_at: "2026-09-11T12:00:00Z".into(),
        source: ObjectStateSource::CallerReported,
        validity: if draft.observed_at.is_some() {
            ObjectStateValidity::WithinReportedWindow
        } else {
            ObjectStateValidity::Unknown
        },
        draft,
        receipt,
        superseded_by: None,
    }
}

fn original() -> ObjectStateEntry {
    snapshot(
        ACTIVITY,
        ObjectStateDraft {
            id: ORIGINAL.into(),
            reference: REFERENCE.into(),
            content: ObjectStateContent::UserStatement {
                text: "Earlier report".into(),
            },
            observed_at: None,
            valid_until: None,
            supersedes: None,
        },
    )
}

fn response(activity_id: &str, entries: Vec<ObjectStateEntry>) -> Response {
    Response::ObjectState(ActivityObjectStateResponse {
        schema: 1,
        activity_id: activity_id.into(),
        entries,
    })
}

fn load_original(state: &mut Activities) {
    let request = update(state, Message::Refresh).unwrap();
    finish(state, request, Ok(response(ACTIVITY, vec![original()])));
}

fn statement_form(state: &mut Activities) {
    update(state, Message::New);
    update(
        state,
        Message::Field(Field::Text, "Reported observation".into()),
    );
}

#[test]
fn activity_object_state_reads_are_explicit_bounded_and_available_in_every_lifecycle_state() {
    for lifecycle in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let mut state = ready(lifecycle);
        let before = state.detail.clone().unwrap();
        let poll = state.update(ActivityMessage::Tick, true).unwrap();
        assert!(matches!(&poll.action, Action::Get(id) if id == ACTIVITY));
        assert!(
            finish(
                &mut state,
                poll,
                Ok(Response::Detail(Box::new(before.clone())))
            )
            .is_none()
        );
        assert!(state.object_state.entries.is_none());
        let request = update(&mut state, Message::Refresh).unwrap();
        assert!(
            matches!(&request.action, Action::ObjectStateList { activity_id, query }
            if activity_id == ACTIVITY && query.limit == Some(100) && query.reference.is_none())
        );
        assert!(update(&mut state, Message::Refresh).is_none());
        finish(
            &mut state,
            request,
            Ok(response(ACTIVITY, vec![original()])),
        );
        assert_eq!(state.detail.as_ref().unwrap(), &before);
        assert_eq!(
            state.object_state.entries.as_ref().unwrap().entries.len(),
            1
        );
        assert!(state.notice.is_none());
        assert!(state.receipts.is_none());
        assert!(state.objects.is_none());
        let _ = state.view(true);
    }
}

#[test]
fn activity_object_state_selection_and_filter_changes_discard_late_results_and_errors() {
    let mut state = ready(ActivityState::Active);
    let stale = update(&mut state, Message::Refresh).unwrap();
    update(&mut state, Message::Filter(TARGET.into()));
    assert!(state.pending.is_none());
    let current = update(&mut state, Message::Refresh).unwrap();
    assert!(
        matches!(&current.action, Action::ObjectStateList { query, .. }
        if query.reference.as_deref() == Some(TARGET))
    );
    finish(&mut state, stale.clone(), Err("Old filter failure".into()));
    finish(&mut state, stale, Ok(response(ACTIVITY, vec![original()])));
    assert!(state.error.is_none());
    assert!(state.object_state.entries.is_none());
    let mut entry = original();
    entry.draft.reference = TARGET.into();
    finish(&mut state, current, Ok(response(ACTIVITY, vec![entry])));
    assert_eq!(
        state.object_state.entries.as_ref().unwrap().entries[0]
            .draft
            .reference,
        TARGET
    );

    let stale = update(&mut state, Message::Refresh).unwrap();
    let current = state
        .update(ActivityMessage::Open(OTHER_ACTIVITY.into()), true)
        .unwrap();
    finish(
        &mut state,
        stale.clone(),
        Ok(response(ACTIVITY, vec![original()])),
    );
    finish(&mut state, stale, Err("Old Activity failure".into()));
    assert!(state.object_state.entries.is_none());
    assert!(state.object_state.filter.is_empty());
    assert!(state.error.is_none());
    let mut detail = ready(ActivityState::Paused).detail.unwrap();
    detail.activity.id = OTHER_ACTIVITY.into();
    finish(&mut state, current, Ok(Response::Detail(Box::new(detail))));
    assert_eq!(state.selected.as_deref(), Some(OTHER_ACTIVITY));
}

#[test]
fn activity_object_state_malformed_scope_filter_and_service_errors_remain_visible() {
    for mismatch in 0..5 {
        let mut state = ready(ActivityState::Active);
        let request = update(&mut state, Message::Refresh).unwrap();
        let mut result = ActivityObjectStateResponse {
            schema: 1,
            activity_id: ACTIVITY.into(),
            entries: vec![original()],
        };
        match mismatch {
            0 => result.schema = 2,
            1 => result.activity_id = OTHER_ACTIVITY.into(),
            2 => result.entries[0].activity_id = OTHER_ACTIVITY.into(),
            3 => result.entries[0].draft.id = SUCCESSOR.into(),
            _ => result.entries[0].validity = ObjectStateValidity::Expired,
        }
        finish(&mut state, request, Ok(Response::ObjectState(result)));
        assert!(state.object_state.entries.is_none());
        assert!(state.error.is_some());
        assert!(!state.should_poll());
    }
    let mut state = ready(ActivityState::Active);
    update(&mut state, Message::Filter(TARGET.into()));
    let request = update(&mut state, Message::Refresh).unwrap();
    finish(
        &mut state,
        request,
        Ok(response(ACTIVITY, vec![original()])),
    );
    assert!(state.error.is_some());
    assert!(state.object_state.entries.is_none());
    let request = update(&mut state, Message::Refresh).unwrap();
    finish(
        &mut state,
        request,
        Err("Object-state service unavailable".into()),
    );
    assert_eq!(
        state.error.as_deref(),
        Some("Object-state service unavailable")
    );
    assert!(state.object_state.entries.is_none());
}

#[test]
fn activity_object_state_authoring_serializes_all_observation_kinds_without_source_or_app_text() {
    for kind in [Kind::UserStatement, Kind::AgentInference, Kind::AppReport] {
        let mut state = ready(ActivityState::Active);
        statement_form(&mut state);
        update(&mut state, Message::Kind(kind));
        update(&mut state, Message::Subject(TARGET.into()));
        update(&mut state, Message::Field(Field::ReceiptId, RECEIPT.into()));
        update(
            &mut state,
            Message::Field(Field::ObservedAt, "2026-09-11T12:00:00Z".into()),
        );
        update(
            &mut state,
            Message::Field(Field::ValidUntil, "2026-09-12T12:00:00Z".into()),
        );
        let request = update(&mut state, Message::Save).unwrap();
        let (activity_id, draft) = submitted(&request);
        assert_eq!(activity_id, ACTIVITY);
        assert!(Uuid::parse_str(&draft.id).is_ok());
        assert_eq!(draft.reference, TARGET);
        assert_eq!(draft.observed_at.as_deref(), Some("2026-09-11T12:00:00Z"));
        assert_eq!(draft.valid_until.as_deref(), Some("2026-09-12T12:00:00Z"));
        assert!(draft.supersedes.is_none());
        assert_eq!(content_kind(&draft.content), kind);
        let encoded = serde_json::to_value(draft).unwrap();
        assert_eq!(encoded.as_object().unwrap().len(), 6);
        for key in ["source", "owner_uid", "receipt", "validity", "execute"] {
            assert!(encoded.get(key).is_none());
        }
        if kind == Kind::AppReport {
            assert_eq!(
                encoded["content"],
                json!({"kind": "app_report", "receipt_id": RECEIPT})
            );
        } else {
            assert_eq!(encoded["content"]["text"], "Reported observation");
        }
        assert!(state.objects.is_none(), "no App declaration or data fetch");
        assert!(
            state.receipts.is_none(),
            "link by ID without automatically fetching receipts"
        );
        let _ = state.view(true);
    }
}

#[test]
fn activity_object_state_relations_select_existing_references_and_never_carry_a_window() {
    for relation in [
        ObjectStateRelation::RelatedTo,
        ObjectStateRelation::DependsOn,
        ObjectStateRelation::DerivedFrom,
    ] {
        let mut state = ready(ActivityState::Active);
        statement_form(&mut state);
        update(
            &mut state,
            Message::Field(Field::ObservedAt, "2026-09-11T12:00:00Z".into()),
        );
        update(
            &mut state,
            Message::Field(Field::ValidUntil, "2026-09-12T12:00:00Z".into()),
        );
        update(&mut state, Message::Kind(Kind::Relation));
        update(&mut state, Message::Relation(relation));
        update(&mut state, Message::Target(TARGET.into()));
        update(&mut state, Message::Field(Field::Text, "".into()));
        update(
            &mut state,
            Message::Field(Field::ObservedAt, "ignored for a relation".into()),
        );
        let request = update(&mut state, Message::Save).unwrap();
        let (_, draft) = submitted(&request);
        assert_eq!(
            draft.content,
            ObjectStateContent::Relation {
                relation,
                target: TARGET.into(),
                note: "".into(),
            }
        );
        assert!(draft.observed_at.is_none());
        assert!(draft.valid_until.is_none());
        assert_eq!(draft.reference, REFERENCE);
        let _ = state.view(true);
    }
}

#[test]
fn activity_object_state_retry_retains_exact_intent_and_edits_rotate_uuid() {
    let mut state = ready(ActivityState::Active);
    let before = state.detail.clone().unwrap();
    statement_form(&mut state);
    let first = update(&mut state, Message::Save).unwrap();
    let original_draft = submitted(&first).1.clone();
    update(
        &mut state,
        Message::Field(Field::Text, "Late edit while pending".into()),
    );
    assert!(update(&mut state, Message::Save).is_none());
    assert_eq!(
        state.object_state.form.as_ref().unwrap().text,
        "Reported observation"
    );
    finish(
        &mut state,
        first,
        Err("Timed out; the entry may have been recorded".into()),
    );
    assert_eq!(state.detail.as_ref().unwrap(), &before);
    assert!(state.object_state.entries.is_none());
    assert!(!state.should_poll());
    update(&mut state, Message::Subject(REFERENCE.into()));
    update(
        &mut state,
        Message::Field(Field::Text, "Reported observation".into()),
    );
    update(&mut state, Message::Field(Field::ReceiptId, RECEIPT.into()));
    let retry = update(&mut state, Message::Save).unwrap();
    assert_eq!(submitted(&retry).1, &original_draft);
    finish(&mut state, retry, Err("Still unavailable".into()));
    update(
        &mut state,
        Message::Field(Field::Text, "New submission intent".into()),
    );
    let changed = update(&mut state, Message::Save).unwrap();
    assert_ne!(submitted(&changed).1.id, original_draft.id);
    assert_eq!(state.detail.as_ref().unwrap(), &before);
}

#[test]
fn activity_object_state_success_refetches_shared_history_without_lifecycle_mutation() {
    for lifecycle in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let mut state = ready(lifecycle);
        let before = state.detail.clone().unwrap();
        statement_form(&mut state);
        let request = update(&mut state, Message::Save).unwrap();
        let entry = snapshot(ACTIVITY, submitted(&request).1.clone());
        let refresh = finish(
            &mut state,
            request,
            Ok(Response::ObjectStateRecorded(Box::new(entry.clone()))),
        )
        .unwrap();
        assert!(
            matches!(&refresh.action, Action::ObjectStateList { activity_id, .. } if activity_id == ACTIVITY)
        );
        assert!(state.object_state.form.is_none());
        assert!(
            state.object_state.entries.is_none(),
            "no optimistic insertion or local ledger"
        );
        assert_eq!(state.detail.as_ref().unwrap(), &before);
        finish(&mut state, refresh, Ok(response(ACTIVITY, vec![entry])));
        assert_eq!(state.detail.as_ref().unwrap(), &before);
        assert!(state.notice.as_ref().unwrap().contains("caller report"));
    }
}

#[test]
fn activity_object_state_navigation_discards_late_mutation_success_and_failure() {
    let mut state = ready(ActivityState::Active);
    statement_form(&mut state);
    let stale = update(&mut state, Message::Save).unwrap();
    let entry = snapshot(ACTIVITY, submitted(&stale).1.clone());
    let current = state
        .update(ActivityMessage::Open(OTHER_ACTIVITY.into()), true)
        .unwrap();
    assert!(state.object_state.form.is_none());
    let mut detail = ready(ActivityState::Paused).detail.unwrap();
    detail.activity.id = OTHER_ACTIVITY.into();
    finish(&mut state, current, Ok(Response::Detail(Box::new(detail))));
    finish(
        &mut state,
        stale.clone(),
        Ok(Response::ObjectStateRecorded(Box::new(entry))),
    );
    finish(&mut state, stale, Err("Old write refusal".into()));
    assert!(state.object_state.entries.is_none());
    assert!(state.notice.is_none());
    assert!(state.error.is_none());
    assert!(state.pending.is_none());
    assert_eq!(state.detail.as_ref().unwrap().activity.id, OTHER_ACTIVITY);
}

#[test]
fn activity_object_state_acknowledgements_must_match_activity_and_submitted_intent() {
    for mismatch in 0..5 {
        let mut state = ready(ActivityState::Active);
        statement_form(&mut state);
        let request = update(&mut state, Message::Save).unwrap();
        let draft = submitted(&request).1.clone();
        let mut entry = snapshot(ACTIVITY, draft.clone());
        match mismatch {
            0 => entry.activity_id = OTHER_ACTIVITY.into(),
            1 => entry.draft.reference = TARGET.into(),
            2 => {
                entry.draft.content = ObjectStateContent::AgentInference {
                    text: "Different".into(),
                }
            }
            3 => entry.draft.supersedes = Some(ORIGINAL.into()),
            _ => {
                entry.id = SUCCESSOR.into();
                entry.draft.id = SUCCESSOR.into();
            }
        }
        assert!(
            finish(
                &mut state,
                request,
                Ok(Response::ObjectStateRecorded(Box::new(entry)))
            )
            .is_none()
        );
        assert!(state.error.is_some());
        assert_eq!(state.object_state.form.as_ref().unwrap().id, draft.id);
        assert!(state.object_state.entries.is_none());
        assert!(state.notice.is_none());
    }
}

#[test]
fn activity_object_state_normalized_acknowledgements_refetch_history_without_rewriting_intent() {
    let mut state = ready(ActivityState::Active);
    let before = state.detail.clone().unwrap();
    statement_form(&mut state);
    update(
        &mut state,
        Message::Field(Field::Text, " \tReported  observation\n".into()),
    );
    update(
        &mut state,
        Message::Field(Field::ObservedAt, "2026-09-11T05:00:00-07:00".into()),
    );
    update(
        &mut state,
        Message::Field(Field::ValidUntil, "2026-09-11T05:30:00-07:00".into()),
    );
    let request = update(&mut state, Message::Save).unwrap();
    let submitted = submitted(&request).1.clone();
    let mut normalized = submitted.clone();
    normalized.content = ObjectStateContent::UserStatement {
        text: "Reported  observation".into(),
    };
    normalized.observed_at = Some("2026-09-11T12:00:00.000000000Z".into());
    normalized.valid_until = Some("2026-09-11T12:30:00.000000000Z".into());
    let entry = snapshot(ACTIVITY, normalized);
    assert_ne!(entry.draft, submitted);
    let refresh = finish(
        &mut state,
        request,
        Ok(Response::ObjectStateRecorded(Box::new(entry.clone()))),
    )
    .unwrap();
    assert!(matches!(refresh.action, Action::ObjectStateList { .. }));
    assert!(state.error.is_none());
    assert!(state.object_state.form.is_none());
    assert!(state.object_state.entries.is_none());
    assert_eq!(state.detail.as_ref().unwrap(), &before);
    finish(
        &mut state,
        refresh,
        Ok(response(ACTIVITY, vec![entry.clone()])),
    );
    assert_eq!(
        state.object_state.entries.as_ref().unwrap().entries[0],
        entry
    );
    assert_eq!(state.detail.as_ref().unwrap(), &before);
    assert_eq!(
        submitted.observed_at.as_deref(),
        Some("2026-09-11T05:00:00-07:00")
    );
}

#[test]
fn activity_object_state_correction_locks_subject_and_keeps_conflicts_visible() {
    let mut state = ready(ActivityState::Active);
    load_original(&mut state);
    update(&mut state, Message::Correct(ORIGINAL.into()));
    update(&mut state, Message::Subject(TARGET.into()));
    update(
        &mut state,
        Message::Field(Field::Text, "Corrected caller report".into()),
    );
    let request = update(&mut state, Message::Save).unwrap();
    let draft = submitted(&request).1.clone();
    assert_ne!(draft.id, ORIGINAL);
    assert_eq!(draft.reference, REFERENCE);
    assert_eq!(draft.supersedes.as_deref(), Some(ORIGINAL));
    assert_eq!(
        state.object_state.entries.as_ref().unwrap().entries[0],
        original()
    );
    finish(
        &mut state,
        request,
        Err("Conflict: the predecessor already has a successor".into()),
    );
    assert!(state.error.as_ref().unwrap().contains("Conflict"));
    assert_eq!(state.object_state.form.as_ref().unwrap().id, draft.id);
    let refresh = update(&mut state, Message::Discard).unwrap();
    let mut historical = original();
    historical.superseded_by = Some(SUCCESSOR.into());
    finish(
        &mut state,
        refresh,
        Ok(response(ACTIVITY, vec![historical])),
    );
    assert!(update(&mut state, Message::Correct(ORIGINAL.into())).is_none());
    assert!(state.object_state.form.is_none());
    assert!(state.error.is_some());
}

#[test]
fn activity_object_state_corrections_preserve_each_typed_payload_and_reported_window() {
    for content in [
        ObjectStateContent::UserStatement {
            text: "Caller statement".into(),
        },
        ObjectStateContent::AgentInference {
            text: "Caller-classified inference".into(),
        },
        ObjectStateContent::AppReport {
            receipt_id: RECEIPT.into(),
        },
        ObjectStateContent::Relation {
            relation: ObjectStateRelation::RelatedTo,
            target: TARGET.into(),
            note: "".into(),
        },
        ObjectStateContent::Relation {
            relation: ObjectStateRelation::DependsOn,
            target: TARGET.into(),
            note: "Planning only".into(),
        },
        ObjectStateContent::Relation {
            relation: ObjectStateRelation::DerivedFrom,
            target: TARGET.into(),
            note: "Not execution evidence".into(),
        },
    ] {
        let mut state = ready(ActivityState::Paused);
        let mut draft = original().draft;
        draft.content = content.clone();
        if content_kind(&content).has_window() {
            draft.observed_at = Some("2026-09-11T12:00:00Z".into());
            draft.valid_until = Some("2026-09-12T12:00:00Z".into());
        }
        let entry = snapshot(ACTIVITY, draft.clone());
        let read = update(&mut state, Message::Refresh).unwrap();
        finish(
            &mut state,
            read,
            Ok(response(ACTIVITY, vec![entry.clone()])),
        );
        let _ = state.view(true);
        update(&mut state, Message::Correct(ORIGINAL.into()));
        let request = update(&mut state, Message::Save).unwrap();
        let (_, correction) = submitted(&request);
        assert_ne!(correction.id, draft.id);
        assert_eq!(correction.reference, draft.reference);
        assert_eq!(correction.content, content);
        assert_eq!(correction.observed_at, draft.observed_at);
        assert_eq!(correction.valid_until, draft.valid_until);
        assert_eq!(correction.supersedes.as_deref(), Some(ORIGINAL));
        assert_eq!(entry.source, ObjectStateSource::CallerReported);
        let _ = state.view(true);
    }
}

#[test]
fn activity_object_state_retraction_appends_reason_without_window_and_cannot_be_corrected() {
    let mut state = ready(ActivityState::Completed);
    let mut previous = original();
    previous.draft.observed_at = Some("2026-09-09T12:00:00Z".into());
    previous.draft.valid_until = Some("2026-09-10T12:00:00Z".into());
    previous.validity = ObjectStateValidity::Expired;
    let read = update(&mut state, Message::Refresh).unwrap();
    finish(
        &mut state,
        read,
        Ok(response(ACTIVITY, vec![previous.clone()])),
    );
    update(&mut state, Message::Retract(ORIGINAL.into()));
    assert_eq!(
        state.object_state.form.as_ref().unwrap().kind,
        Kind::Retracted
    );
    assert!(
        update(&mut state, Message::Save).is_none(),
        "a reason is required"
    );
    update(&mut state, Message::Kind(Kind::UserStatement));
    assert_eq!(
        state.object_state.form.as_ref().unwrap().kind,
        Kind::Retracted
    );
    update(
        &mut state,
        Message::Field(Field::Text, "Mistaken association".into()),
    );
    let request = update(&mut state, Message::Save).unwrap();
    let draft = submitted(&request).1.clone();
    assert_eq!(
        draft.content,
        ObjectStateContent::Retracted {
            reason: "Mistaken association".into()
        }
    );
    assert_eq!(draft.reference, REFERENCE);
    assert_eq!(draft.supersedes.as_deref(), Some(ORIGINAL));
    assert!(draft.observed_at.is_none());
    assert!(draft.valid_until.is_none());
    let retraction = snapshot(ACTIVITY, draft);
    previous.superseded_by = Some(retraction.id.clone());
    let refresh = finish(
        &mut state,
        request,
        Ok(Response::ObjectStateRecorded(Box::new(retraction.clone()))),
    )
    .unwrap();
    finish(
        &mut state,
        refresh,
        Ok(response(ACTIVITY, vec![retraction.clone(), previous])),
    );
    for message in [
        Message::Correct(retraction.id.clone()),
        Message::Retract(retraction.id.clone()),
    ] {
        assert!(update(&mut state, message).is_none());
        assert!(state.object_state.form.is_none());
        assert!(state.error.is_some());
    }
    let _ = state.view(true);
    statement_form(&mut state);
    assert!(
        state
            .object_state
            .form
            .as_ref()
            .unwrap()
            .supersedes
            .is_none()
    );
    assert_ne!(state.object_state.form.as_ref().unwrap().id, retraction.id);
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Completed
    );
}

#[test]
fn activity_object_state_detached_subject_history_stays_readable_but_is_not_rewritten() {
    let mut state = ready(ActivityState::Cancelled);
    load_original(&mut state);
    state.detail.as_mut().unwrap().activity.resources.clear();
    assert!(!can_supersede(&original(), &[]));
    let _ = state.view(true);
    update(&mut state, Message::Correct(ORIGINAL.into()));
    assert!(state.error.is_some());
    assert!(state.object_state.form.is_none());
    update(&mut state, Message::New);
    assert!(state.object_state.form.is_none());
    update(&mut state, Message::Filter(REFERENCE.into()));
    let request = update(&mut state, Message::Refresh).unwrap();
    finish(
        &mut state,
        request,
        Ok(response(ACTIVITY, vec![original()])),
    );
    assert_eq!(
        state.object_state.entries.as_ref().unwrap().entries.len(),
        1
    );
    assert_eq!(
        state.detail.as_ref().unwrap().activity.state,
        ActivityState::Cancelled
    );
}

#[test]
fn activity_object_state_forms_validate_bounds_membership_and_paired_windows() {
    let mut state = ready(ActivityState::Active);
    update(&mut state, Message::New);
    assert!(update(&mut state, Message::Save).is_none());
    update(&mut state, Message::Subject("notes.txt".into()));
    assert_eq!(
        state.object_state.form.as_ref().unwrap().reference,
        REFERENCE
    );
    assert!(state.error.is_some());
    update(
        &mut state,
        Message::Field(Field::Text, "\u{e9}".repeat(2049)),
    );
    assert!(update(&mut state, Message::Save).is_none());
    update(
        &mut state,
        Message::Field(Field::Text, "\u{e9}".repeat(2048)),
    );
    update(
        &mut state,
        Message::Field(Field::ObservedAt, "2026-09-11T12:00:00Z".into()),
    );
    assert!(update(&mut state, Message::Save).is_none());
    update(&mut state, Message::Field(Field::ObservedAt, "".into()));
    update(&mut state, Message::Kind(Kind::AppReport));
    update(
        &mut state,
        Message::Field(Field::ReceiptId, "not-a-uuid".into()),
    );
    assert!(update(&mut state, Message::Save).is_none());
    update(&mut state, Message::Kind(Kind::Relation));
    update(
        &mut state,
        Message::Target("app://kv/entry?id=not-attached".into()),
    );
    assert!(state.object_state.form.as_ref().unwrap().target.is_empty());
    assert!(update(&mut state, Message::Save).is_none());
    update(&mut state, Message::Target(TARGET.into()));
    update(
        &mut state,
        Message::Field(Field::Text, "\u{e9}".repeat(1025)),
    );
    assert!(update(&mut state, Message::Save).is_none());
    update(
        &mut state,
        Message::Field(Field::Text, "\u{e9}".repeat(1024)),
    );
    assert!(update(&mut state, Message::Save).is_some());
}

#[test]
fn activity_object_state_broker_timestamp_validation_errors_preserve_the_reported_inputs() {
    let mut state = ready(ActivityState::Active);
    statement_form(&mut state);
    update(
        &mut state,
        Message::Field(Field::ObservedAt, "not RFC3339".into()),
    );
    update(
        &mut state,
        Message::Field(Field::ValidUntil, "also invalid".into()),
    );
    let request = update(&mut state, Message::Save).unwrap();
    let draft = submitted(&request).1.clone();
    finish(
        &mut state,
        request,
        Err("observed_at must be RFC3339 and end must be after start".into()),
    );
    assert_eq!(
        state.object_state.form.as_ref().unwrap().observed_at,
        "not RFC3339"
    );
    assert_eq!(state.object_state.form.as_ref().unwrap().id, draft.id);
    assert!(state.error.as_ref().unwrap().contains("RFC3339"));
    assert!(state.object_state.entries.is_none());
}

#[test]
fn activity_object_state_forms_do_not_overlap_other_forms_or_replay_on_reconnect() {
    let mut state = ready(ActivityState::Active);
    state.update(ActivityMessage::Edit, true);
    update(&mut state, Message::New);
    assert!(state.object_state.form.is_none());
    state.form = None;
    statement_form(&mut state);
    state.update(ActivityMessage::Edit, true);
    state.update(ActivityMessage::NewObject, true);
    assert!(state.form.is_none());
    assert!(state.object_form.is_none());
    assert!(state.update(ActivityMessage::Refresh, true).is_none());
    assert!(state.update(ActivityMessage::Run, true).is_none());
    assert!(
        state
            .update(ActivityMessage::Transition(ActivityState::Completed), true)
            .is_none()
    );
    let draft_id = state.object_state.form.as_ref().unwrap().id.clone();
    state.hide();
    assert_eq!(state.object_state.form.as_ref().unwrap().id, draft_id);
    assert!(state.update(ActivityMessage::Show, true).is_none());
    let pending = update(&mut state, Message::Save).unwrap();
    let entry = snapshot(ACTIVITY, submitted(&pending).1.clone());
    state.hide();
    assert!(state.object_state.form.is_none());
    assert!(
        finish(
            &mut state,
            pending,
            Ok(Response::ObjectStateRecorded(Box::new(entry)))
        )
        .is_none()
    );
    let reconnect = state.update(ActivityMessage::Show, true).unwrap();
    assert!(matches!(reconnect.action, Action::Get(_)));
    assert!(state.notice.is_none());
}

#[test]
fn activity_object_state_validity_history_and_receipt_widgets_keep_reports_inert() {
    let state = ready(ActivityState::Active);
    assert_eq!(
        validity_label(ObjectStateValidity::Unknown),
        fl!("activity-object-state-unknown")
    );
    assert_eq!(
        validity_label(ObjectStateValidity::Expired),
        fl!("activity-object-state-expired")
    );
    assert_eq!(
        validity_label(ObjectStateValidity::NotYetApplicable),
        fl!("activity-object-state-not-yet-applicable")
    );
    assert_eq!(
        validity_label(ObjectStateValidity::WithinReportedWindow),
        fl!("activity-object-state-within-window")
    );
    assert_eq!(
        kind_label(Kind::AgentInference),
        fl!("activity-object-state-agent-inference")
    );
    assert_eq!(
        history_label(&original()),
        fl!("activity-object-state-no-successor")
    );
    for validity in [
        ObjectStateValidity::Unknown,
        ObjectStateValidity::Expired,
        ObjectStateValidity::NotYetApplicable,
        ObjectStateValidity::WithinReportedWindow,
    ] {
        let mut entry = original();
        entry.validity = validity;
        if validity != ObjectStateValidity::Unknown {
            entry.draft.observed_at = Some("2026-09-09T12:00:00Z".into());
            entry.draft.valid_until = Some("2026-09-10T12:00:00Z".into());
        }
        entry.superseded_by = Some(SUCCESSOR.into());
        assert!(history_label(&entry).contains(SUCCESSOR));
        assert!(!can_supersede(
            &entry,
            &state.detail.as_ref().unwrap().activity.resources
        ));
        let _ = entry_view(&entry, &[], true);
    }
    let mut entry = original();
    entry.draft.content = ObjectStateContent::AppReport {
        receipt_id: RECEIPT.into(),
    };
    let mut entry = snapshot(ACTIVITY, entry.draft);
    let raw = "<script>not executed</script> $(not-a-command) {\"verified\":true}";
    for outcome in [
        ActivityReceiptOutcome::Returned,
        ActivityReceiptOutcome::ReportedError,
        ActivityReceiptOutcome::Indeterminate,
    ] {
        for kind in [
            ReceiptResultKind::Json,
            ReceiptResultKind::Text,
            ReceiptResultKind::Empty,
        ] {
            let receipt = entry.receipt.as_mut().unwrap();
            receipt.outcome = outcome;
            receipt.error = Some(raw.into());
            let result = receipt.result.as_mut().unwrap();
            result.kind = kind;
            result.preview = raw.into();
            let _ = entry_view(&entry, &[], true);
            assert_eq!(
                entry
                    .receipt
                    .as_ref()
                    .unwrap()
                    .result
                    .as_ref()
                    .unwrap()
                    .preview,
                raw
            );
            assert_eq!(entry.source, ObjectStateSource::CallerReported);
        }
    }
    entry.receipt.as_mut().unwrap().result = None;
    let _ = entry_view(&entry, &[], true);
    entry.draft.content = ObjectStateContent::Retracted { reason: raw.into() };
    entry.draft.supersedes = Some(ORIGINAL.into());
    entry.draft.id = SUCCESSOR.into();
    entry.id = SUCCESSOR.into();
    entry.receipt = None;
    assert_eq!(
        history_label(&entry),
        fl!("activity-object-state-retracted")
    );
    let _ = entry_view(&entry, &[], true);
}
