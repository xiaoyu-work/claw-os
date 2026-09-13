use super::*;

fn draft() -> ActivityDraft {
    ActivityDraft {
        title: "Release".to_string(),
        goal: "Publish a reviewed release".to_string(),
        completion_criteria: "Published and verified".to_string(),
        boundaries: "Ask before publishing".to_string(),
        resources: vec![resource()],
    }
}

fn resource() -> ActivityResource {
    ActivityResource {
        label: "Release draft".to_string(),
        reference: "not-a-path-or-url".to_string(),
    }
}

#[test]
fn draft_defaults_optional_fields_and_rejects_unknown_input() {
    let decoded: ActivityDraft =
        serde_json::from_str(r#"{"title":"Release","goal":"Publish"}"#).unwrap();
    decoded.validate().unwrap();
    assert!(decoded.completion_criteria.is_empty());
    assert!(decoded.boundaries.is_empty());
    assert!(decoded.resources.is_empty());
    for json in [
        r#"{"title":"Release"}"#,
        r#"{"goal":"Publish"}"#,
        r#"{"title":"Release","goal":"Publish","owner_uid":0}"#,
        r#"{"title":"Release","goal":"Publish","state":"completed"}"#,
        r#"{"title":"Release","goal":"Publish","job_ids":[]}"#,
    ] {
        assert!(serde_json::from_str::<ActivityDraft>(json).is_err());
    }
    assert!(serde_json::from_str::<ActivityResource>(
        r#"{"label":"Draft","reference":"inert","execute":true}"#
    )
    .is_err());
    for json in [
        r#"{"title":"Release","owner_uid":0}"#,
        r#"{"title":"Release","state":"active"}"#,
        r#"{"title":"Release","completion_note":"done"}"#,
    ] {
        assert!(serde_json::from_str::<ActivityPatch>(json).is_err());
    }
}

#[test]
fn states_have_one_canonical_wire_spelling_and_only_active_allows_work() {
    for (state, name) in [
        (ActivityState::Active, "active"),
        (ActivityState::Paused, "paused"),
        (ActivityState::Completed, "completed"),
        (ActivityState::Cancelled, "cancelled"),
    ] {
        assert_eq!(state.as_str(), name);
        assert_eq!(ActivityState::parse(name).unwrap(), state);
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, format!("\"{name}\""));
        assert_eq!(serde_json::from_str::<ActivityState>(&json).unwrap(), state);
        assert_eq!(state.allows_work(), state == ActivityState::Active);
    }
    for invalid in [
        "", "running", "done", "ACTIVE", " active", "active\n", "active\0",
    ] {
        assert!(matches!(
            ActivityState::parse(invalid),
            Err(ActivityError::Invalid(_))
        ));
    }
}

#[test]
fn byte_limits_apply_to_trimmed_content_and_allow_multiline_planning() {
    let mut input = draft();
    input.title = format!("  {}  ", "é".repeat(MAX_TITLE_BYTES / 2));
    input.goal = format!(" \n{}\r\n ", "🙂".repeat(MAX_GOAL_BYTES / 4));
    input.completion_criteria = format!("\n{}\t", "x".repeat(MAX_PLANNING_BYTES));
    input.boundaries = "  Ask first.\r\n\tKeep the existing files.\n  ".to_string();
    input.resources[0].label = format!(" {} ", "é".repeat(MAX_RESOURCE_LABEL_BYTES / 2));
    input.resources[0].reference = format!(" {} ", "x".repeat(MAX_RESOURCE_REFERENCE_BYTES));
    input.validate().unwrap();
    let normalized = input.normalized().unwrap();
    assert_eq!(normalized.title.len(), MAX_TITLE_BYTES);
    assert_eq!(normalized.goal.len(), MAX_GOAL_BYTES);
    assert_eq!(normalized.completion_criteria.len(), MAX_PLANNING_BYTES);
    assert_eq!(
        normalized.boundaries,
        "Ask first.\r\n\tKeep the existing files."
    );
    assert_eq!(
        normalized.resources[0].label.len(),
        MAX_RESOURCE_LABEL_BYTES
    );
    assert_eq!(
        normalized.resources[0].reference.len(),
        MAX_RESOURCE_REFERENCE_BYTES
    );
}

#[test]
fn every_planning_field_rejects_oversize_or_invalid_required_content() {
    for field in ["title", "goal", "completion_criteria", "boundaries"] {
        let mut input = draft();
        let (value, limit) = match field {
            "title" => (&mut input.title, MAX_TITLE_BYTES),
            "goal" => (&mut input.goal, MAX_GOAL_BYTES),
            "completion_criteria" => (&mut input.completion_criteria, MAX_PLANNING_BYTES),
            _ => (&mut input.boundaries, MAX_PLANNING_BYTES),
        };
        *value = "é".repeat(limit / 2 + 1);
        assert!(matches!(input.validate(), Err(ActivityError::Invalid(_))));
    }
    let mut input = draft();
    input.title = "   ".to_string();
    assert!(input.validate().is_err());
    input = draft();
    input.goal = " \r\n\t ".to_string();
    assert!(input.validate().is_err());
    input = draft();
    input.completion_criteria.clear();
    input.boundaries.clear();
    input.resources.clear();
    input.validate().unwrap();
}

#[test]
fn control_characters_are_not_hidden_by_trimming() {
    for control in ['\0', '\u{1b}', '\u{7f}', '\u{85}'] {
        for field in ["title", "goal", "completion_criteria", "boundaries"] {
            let mut input = draft();
            let value = match field {
                "title" => &mut input.title,
                "goal" => &mut input.goal,
                "completion_criteria" => &mut input.completion_criteria,
                _ => &mut input.boundaries,
            };
            value.push(control);
            assert!(matches!(input.validate(), Err(ActivityError::Invalid(_))));
        }
    }
    for control in ['\n', '\r', '\t', '\0', '\u{1b}'] {
        let mut input = draft();
        input.title.push(control);
        assert!(input.validate().is_err());
        input = draft();
        input.resources[0].label.push(control);
        assert!(input.validate().is_err());
        input = draft();
        input.resources[0].reference.push(control);
        assert!(input.validate().is_err());
    }
}

#[test]
fn resources_are_bounded_display_strings_not_resolved_objects() {
    let mut input = draft();
    input.resources = vec![resource(); MAX_RESOURCES];
    input.validate().unwrap();
    input.resources.push(resource());
    assert!(matches!(input.validate(), Err(ActivityError::Invalid(_))));
    input = draft();
    input.resources[0].label = "x".repeat(MAX_RESOURCE_LABEL_BYTES + 1);
    assert!(input.validate().is_err());
    input = draft();
    input.resources[0].reference = "x".repeat(MAX_RESOURCE_REFERENCE_BYTES + 1);
    assert!(input.validate().is_err());
    input = draft();
    input.resources[0].label = " ".to_string();
    assert!(input.validate().is_err());
    input = draft();
    input.resources[0].reference.clear();
    assert!(input.validate().is_err());
    for reference in [
        r"Z:\does-not-exist\release.md",
        "https://example.invalid/release",
        "opaque App reference for a later phase",
    ] {
        input.resources[0] = resource();
        input.resources[0].reference = reference.to_string();
        input.validate().unwrap();
    }
}

#[test]
fn patches_require_a_field_and_apply_the_same_bounds() {
    assert!(ActivityPatch::default().validate().is_err());
    let nulls: ActivityPatch = serde_json::from_str(r#"{"title":null,"resources":null}"#).unwrap();
    assert!(nulls.validate().is_err());
    for patch in [
        ActivityPatch {
            title: Some(" ".to_string()),
            ..Default::default()
        },
        ActivityPatch {
            title: Some("x".repeat(MAX_TITLE_BYTES + 1)),
            ..Default::default()
        },
        ActivityPatch {
            goal: Some("x".repeat(MAX_GOAL_BYTES + 1)),
            ..Default::default()
        },
        ActivityPatch {
            goal: Some("done\0".to_string()),
            ..Default::default()
        },
        ActivityPatch {
            completion_criteria: Some("x".repeat(MAX_PLANNING_BYTES + 1)),
            ..Default::default()
        },
        ActivityPatch {
            boundaries: Some("x".repeat(MAX_PLANNING_BYTES + 1)),
            ..Default::default()
        },
        ActivityPatch {
            resources: Some(vec![resource(); MAX_RESOURCES + 1]),
            ..Default::default()
        },
    ] {
        assert!(matches!(patch.validate(), Err(ActivityError::Invalid(_))));
    }
    let clearing = ActivityPatch {
        completion_criteria: Some(String::new()),
        boundaries: Some(String::new()),
        resources: Some(Vec::new()),
        ..Default::default()
    };
    clearing.validate().unwrap();
    let value = serde_json::to_value(clearing).unwrap();
    assert!(value.get("title").is_none());
    assert!(value.get("goal").is_none());
    assert_eq!(value["resources"], serde_json::json!([]));
}

#[test]
fn uuid_identifiers_are_validated_and_normalized_without_trimming() {
    let id = uuid::Uuid::new_v4();
    for value in [
        id.to_string(),
        id.to_string().to_uppercase(),
        id.simple().to_string(),
        id.urn().to_string(),
    ] {
        validate_id(&value).unwrap();
        assert_eq!(parse_id(&value).unwrap(), id.to_string());
    }
    for value in [
        String::new(),
        "not-a-uuid".to_string(),
        format!("{id}\0"),
        format!("\n{id}"),
        format!(" {id} "),
        "f".repeat(4096),
    ] {
        assert!(matches!(
            validate_id(&value),
            Err(ActivityError::Invalid(_))
        ));
        assert!(matches!(parse_id(&value), Err(ActivityError::Invalid(_))));
    }
}

#[test]
fn completion_requires_a_bounded_note_only_for_the_completed_state() {
    for note in [
        None,
        Some(String::new()),
        Some(" \r\n\t ".to_string()),
        Some("x".repeat(MAX_COMPLETION_NOTE_BYTES + 1)),
        Some("confirmed\0".to_string()),
    ] {
        assert!(matches!(
            normalize_completion_note(ActivityState::Completed, note),
            Err(ActivityError::Invalid(_))
        ));
    }
    let note = format!(" \n{}\r\n ", "é".repeat(MAX_COMPLETION_NOTE_BYTES / 2));
    let normalized = normalize_completion_note(ActivityState::Completed, Some(note))
        .unwrap()
        .unwrap();
    assert_eq!(normalized.len(), MAX_COMPLETION_NOTE_BYTES);
    for state in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Cancelled,
    ] {
        assert!(normalize_completion_note(state, None).unwrap().is_none());
        assert!(normalize_completion_note(state, Some("confirmed".to_string())).is_err());
    }
}
