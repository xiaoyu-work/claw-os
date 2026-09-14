use super::*;
use crate::activities::Response;
use cos_agent_protocol::{
    ActivityState, ImportedActivity, ImportedActivityResource,
};
use serde_json::json;

const SOURCE: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const LINEAGE: &str = "00000000-0000-4000-8000-000000000123";
const IMPORTED: &str = "11111111-1111-4111-8111-111111111111";
const SNAPSHOT: &str = "sha256:6d8c822b0519a02e7ab1ebeab48392cb83756fa9cc8b6d39e94b01ab1682882b";

fn document() -> ActivityContinuityDocument {
    ActivityContinuityDocument::from_json(
        serde_json::to_string(&json!({
            "kind": "claw_os.activity_continuity",
            "schema_version": 1,
            "lineage": {"id": LINEAGE, "revision": 7},
            "snapshot": SNAPSHOT,
            "intent": {
                "title": "Release",
                "goal": "Publish the release",
                "completion_criteria": "Reviewed and available",
                "boundaries": "Ask before publishing"
            },
            "references": [{
                "label": "Status",
                "reference": "app://kv/entry?id=release.status&revision=v1"
            }],
            "rules": {
                "execution_limits": {
                    "enabled": false,
                    "max_attempts": 10,
                    "max_turns_per_attempt": 5,
                    "expires_at": "2030-01-01T00:00:00.000000000Z"
                },
                "scheduling": {"priority": "foreground"}
            }
        }))
        .unwrap()
        .as_bytes(),
    )
    .unwrap()
}

fn acknowledgement() -> ActivityContinuityImportAcknowledgement {
    let document = document();
    ActivityContinuityImportAcknowledgement {
        activity: ImportedActivity {
            id: IMPORTED.into(),
            title: document.intent.title,
            goal: document.intent.goal,
            completion_criteria: document.intent.completion_criteria,
            boundaries: document.intent.boundaries,
            resources: vec![ImportedActivityResource {
                label: "Status".into(),
                reference: "app://kv/entry?id=release.status&revision=v1".into(),
            }],
            state: ActivityState::Paused,
            completion_note: None,
            created_at: "2026-09-14T00:00:00Z".into(),
            updated_at: "2026-09-14T00:00:00Z".into(),
        },
        continuity_id: LINEAGE.into(),
        continuity_revision: 7,
        placement: ActivityExecutionPlacement::Local,
    }
}

fn detail_ready() -> Activities {
    crate::localize::localize();
    Activities {
        visible: true,
        selected: Some(SOURCE.into()),
        detail: Some(
            serde_json::from_value(json!({
                "activity": {
                    "id": SOURCE,
                    "title": "Source",
                    "goal": "Original goal",
                    "state": "active"
                },
                "jobs": []
            }))
            .unwrap(),
        ),
        ..Activities::default()
    }
}

fn list_ready() -> Activities {
    crate::localize::localize();
    Activities {
        visible: true,
        ..Activities::default()
    }
}

fn update(state: &mut Activities, message: Message) -> Option<Request> {
    state.update(ActivityMessage::Continuity(message), true)
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

#[test]
fn export_copy_uses_only_the_broker_revalidated_document_and_leaves_source_unchanged() {
    let mut state = detail_ready();
    let source = state.detail.clone();
    state.continuity.import_text = r#"{"untrusted":true}"#.into();
    let request = update(&mut state, Message::Export).unwrap();
    assert!(matches!(&request.action, Action::ExportContinuity(id) if id == SOURCE));
    finish(
        &mut state,
        request,
        Ok(Response::ContinuityDocument(Box::new(document()))),
    );
    assert_eq!(state.detail, source);
    assert_eq!(state.continuity_copy_text(), Some(document().to_json().unwrap()));
    assert_ne!(
        state.continuity_copy_text().as_deref(),
        Some(state.continuity.import_text.as_str())
    );
    let _ = state.continuity_export_view(state.detail.as_ref().unwrap(), true);
}

#[test]
fn import_requires_explicit_local_placement_and_creates_a_paused_selection() {
    let mut state = list_ready();
    let json = document().to_json().unwrap();
    update(&mut state, Message::ImportText(json.clone()));
    assert!(update(&mut state, Message::Import).is_none());
    assert!(state.continuity.input_error.is_some());
    update(&mut state, Message::SelectLocalPlacement);
    let request = update(&mut state, Message::Import).unwrap();
    let (submitted, submitted_document) = match &request.action {
        Action::ImportContinuity { request, document } => (request.clone(), document.clone()),
        _ => panic!("expected continuity import"),
    };
    assert_eq!(submitted.placement, ActivityExecutionPlacement::Local);
    assert_eq!(submitted.document, json);
    assert_eq!(*submitted_document, document());
    assert!(state.selected.is_none());
    let refresh = finish(
        &mut state,
        request,
        Ok(Response::ContinuityImported(Box::new(acknowledgement()))),
    )
    .unwrap();
    assert!(matches!(&refresh.action, Action::Get(id) if id == IMPORTED));
    assert_eq!(state.selected.as_deref(), Some(IMPORTED));
    assert!(state.continuity.import_text.is_empty());
    assert!(state.notice.is_some());
}

#[test]
fn duplicate_version_bounds_and_unknown_authority_failures_are_visible() {
    let mut state = list_ready();
    let valid = document().to_json().unwrap();
    for invalid in [
        valid.replacen(
            r#""kind":"claw_os.activity_continuity""#,
            r#""kind":"claw_os.activity_continuity","kind":"claw_os.activity_continuity""#,
            1,
        ),
        valid.replacen(r#""schema_version":1"#, r#""schema_version":2"#, 1),
        valid.replacen(
            r#""rules":"#,
            r#""authority":"root","rules":"#,
            1,
        ),
    ] {
        update(&mut state, Message::ImportText(invalid));
        update(&mut state, Message::SelectLocalPlacement);
        assert!(update(&mut state, Message::Import).is_none());
        assert!(state.continuity.input_error.is_some());
    }
    let before = state.continuity.import_text.clone();
    update(
        &mut state,
        Message::ImportText("x".repeat(MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES + 1)),
    );
    assert_eq!(state.continuity.import_text, before);
    assert!(state.continuity.input_error.is_some());
    let _ = state.continuity_import_view(true);
}

#[test]
fn conflicts_remain_visible_and_retain_the_exact_text_and_placement() {
    let mut state = list_ready();
    let json = document().to_json().unwrap();
    update(&mut state, Message::ImportText(json.clone()));
    update(&mut state, Message::SelectLocalPlacement);
    let request = update(&mut state, Message::Import).unwrap();
    finish(
        &mut state,
        request,
        Err("Conflict: continuity lineage revision already imported".into()),
    );
    assert_eq!(state.continuity.import_text, json);
    assert_eq!(
        state.continuity.placement,
        Some(ActivityExecutionPlacement::Local)
    );
    assert!(state.error.as_deref().unwrap().contains("Conflict"));
}

#[test]
fn stale_export_and_import_responses_do_not_change_the_new_selection() {
    let mut state = detail_ready();
    let export = update(&mut state, Message::Export).unwrap();
    state.update(ActivityMessage::Back, true);
    assert!(
        finish(
            &mut state,
            export,
            Ok(Response::ContinuityDocument(Box::new(document())))
        )
        .is_none()
    );
    assert!(state.continuity_copy_text().is_none());

    let mut state = list_ready();
    update(&mut state, Message::ImportText(document().to_json().unwrap()));
    update(&mut state, Message::SelectLocalPlacement);
    let import = update(&mut state, Message::Import).unwrap();
    state.update(ActivityMessage::Open(SOURCE.into()), true);
    finish(
        &mut state,
        import,
        Ok(Response::ContinuityImported(Box::new(acknowledgement()))),
    );
    assert_eq!(state.selected.as_deref(), Some(SOURCE));
}

#[test]
fn invalid_acknowledgement_is_visible_and_never_treated_as_import_success() {
    let mut state = list_ready();
    update(&mut state, Message::ImportText(document().to_json().unwrap()));
    update(&mut state, Message::SelectLocalPlacement);
    let request = update(&mut state, Message::Import).unwrap();
    let mut invalid = acknowledgement();
    invalid.activity.state = ActivityState::Active;
    finish(
        &mut state,
        request,
        Ok(Response::ContinuityImported(Box::new(invalid))),
    );
    assert!(state.selected.is_none());
    assert!(state.error.is_some());
    assert!(!state.continuity.import_text.is_empty());

    let mut state = list_ready();
    state.list.push(
        serde_json::from_value(json!({
            "id": IMPORTED,
            "title": "Existing",
            "goal": "Existing goal",
            "state": "paused",
            "created_at": "2026-09-14T00:00:00Z",
            "updated_at": "2026-09-14T00:00:00Z"
        }))
        .unwrap(),
    );
    update(&mut state, Message::ImportText(document().to_json().unwrap()));
    update(&mut state, Message::SelectLocalPlacement);
    let request = update(&mut state, Message::Import).unwrap();
    finish(
        &mut state,
        request,
        Ok(Response::ContinuityImported(Box::new(acknowledgement()))),
    );
    assert!(state.selected.is_none());
    assert!(state.error.is_some());
}
