use super::*;
use serde_json::{Value, json};

const ACTIVITY: &str = "11111111-1111-4111-8111-111111111111";
const ENTRY: &str = "22222222-2222-4222-8222-222222222222";
const PREDECESSOR: &str = "33333333-3333-4333-8333-333333333333";
const RECEIPT: &str = "44444444-4444-4444-8444-444444444444";
const REFERENCE: &str = "app://kv/entry?id=release.status";

fn draft_value(content: Value) -> Value {
    json!({
        "id": ENTRY, "reference": REFERENCE, "content": content,
        "observed_at": null, "valid_until": null, "supersedes": null
    })
}

fn entry_value() -> Value {
    json!({
        "id": ENTRY, "activity_id": ACTIVITY, "owner_uid": 1000,
        "recorded_at": "2026-09-11T12:00:00Z", "source": "caller_reported",
        "draft": draft_value(json!({"kind": "user_statement", "text": "Reported state"})),
        "receipt": null, "superseded_by": null, "validity": "unknown"
    })
}

fn report_value() -> Value {
    json!({
        "id": RECEIPT, "app_id": "kv", "operation": "get",
        "package_digest": "caller-package-digest", "outcome": "indeterminate",
        "result": {
            "kind": "text", "sha256": "caller-result-digest", "bytes": 9000,
            "preview": "not proof of this object's state", "preview_truncated": true
        },
        "error": "caller-reported uncertainty"
    })
}

#[test]
fn activity_object_state_all_content_variants_round_trip_with_exact_draft_fields() {
    for content in [
        json!({"kind": "user_statement", "text": "A reported observation"}),
        json!({"kind": "agent_inference", "text": "A caller-classified inference"}),
        json!({"kind": "app_report", "receipt_id": RECEIPT}),
        json!({"kind": "relation", "relation": "related_to", "target": REFERENCE, "note": ""}),
        json!({"kind": "relation", "relation": "depends_on", "target": REFERENCE, "note": "Planning"}),
        json!({"kind": "relation", "relation": "derived_from", "target": REFERENCE, "note": "Planning"}),
        json!({"kind": "retracted", "reason": "The earlier report was mistaken"}),
    ] {
        let mut value = draft_value(content);
        if value["content"]["kind"] == "retracted" {
            value["supersedes"] = json!(PREDECESSOR);
        }
        let draft: ObjectStateDraft = serde_json::from_value(value.clone()).unwrap();
        draft.validate_shape().unwrap();
        assert_eq!(serde_json::to_value(&draft).unwrap(), value);
        let request = ActivityObjectStateRecordRequest { entry: draft };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"entry": value})
        );
    }
    let draft: ObjectStateDraft = serde_json::from_value(json!({
        "id": ENTRY, "reference": REFERENCE,
        "content": {"kind": "user_statement", "text": "No reported window"}
    }))
    .unwrap();
    assert!(draft.observed_at.is_none());
    assert!(draft.valid_until.is_none());
    assert!(draft.supersedes.is_none());
    assert_eq!(
        serde_json::to_value(draft).unwrap(),
        draft_value(json!({"kind": "user_statement", "text": "No reported window"}))
    );
}

#[test]
fn activity_object_state_requests_reject_owner_source_and_untyped_content() {
    let base = draft_value(json!({"kind": "user_statement", "text": "Report"}));
    for field in [
        "owner_uid",
        "activity_id",
        "source",
        "receipt",
        "validity",
        "execute",
        "caps",
    ] {
        let mut value = json!({"entry": base.clone()});
        value[field] = json!(0);
        assert!(serde_json::from_value::<ActivityObjectStateRecordRequest>(value).is_err());
        let mut value = base.clone();
        value[field] = json!(0);
        assert!(serde_json::from_value::<ObjectStateDraft>(value).is_err());
        let mut value = json!({});
        value[field] = json!(0);
        assert!(serde_json::from_value::<ActivityObjectStateQuery>(value).is_err());
    }
    for content in [
        json!({"kind": "native_app", "text": "Not a content category"}),
        json!({"kind": "verified_fact", "text": "Not a source"}),
        json!({"kind": "app_report", "receipt_id": RECEIPT, "text": "Substitute output"}),
        json!({"kind": "app_report", "receipt_id": {"id": RECEIPT}}),
        json!({"kind": "user_statement", "text": ["untyped"]}),
        json!({"kind": "relation", "relation": "execute_after", "target": REFERENCE, "note": ""}),
        json!({"kind": "retracted", "reason": "Reason", "observed_at": "2026-09-11T12:00:00Z"}),
    ] {
        assert!(serde_json::from_value::<ObjectStateDraft>(draft_value(content)).is_err());
    }
}

#[test]
fn activity_object_state_bounds_count_utf8_bytes_not_characters() {
    for kind in ["user_statement", "agent_inference"] {
        for (text, valid) in [
            ("".into(), false),
            (" \n ".into(), false),
            ("\u{e9}".repeat(2048), true),
            ("\u{e9}".repeat(2049), false),
        ] {
            let draft: ObjectStateDraft =
                serde_json::from_value(draft_value(json!({"kind": kind, "text": text}))).unwrap();
            assert_eq!(draft.validate_shape().is_ok(), valid);
        }
    }
    for (note, valid) in [
        ("".into(), true),
        ("\u{e9}".repeat(1024), true),
        ("\u{e9}".repeat(1025), false),
    ] {
        let draft: ObjectStateDraft = serde_json::from_value(draft_value(json!({
            "kind": "relation", "relation": "related_to", "target": REFERENCE, "note": note
        })))
        .unwrap();
        assert_eq!(draft.validate_shape().is_ok(), valid);
    }
    for (reason, valid) in [
        ("".into(), false),
        (" ".into(), false),
        ("\u{e9}".repeat(2048), true),
        ("\u{e9}".repeat(2049), false),
    ] {
        let mut value = draft_value(json!({"kind": "retracted", "reason": reason}));
        value["supersedes"] = json!(PREDECESSOR);
        let draft: ObjectStateDraft = serde_json::from_value(value).unwrap();
        assert_eq!(draft.validate_shape().is_ok(), valid);
    }
}

#[test]
fn activity_object_state_windows_and_retractions_have_closed_structural_rules() {
    let mut draft: ObjectStateDraft = serde_json::from_value(draft_value(
        json!({"kind": "user_statement", "text": "Reported observation"}),
    ))
    .unwrap();
    draft.observed_at = Some("2026-09-11T12:00:00Z".into());
    assert!(draft.validate_shape().is_err());
    draft.valid_until = Some("2026-09-12T12:00:00Z".into());
    assert!(draft.validate_shape().is_ok());
    draft.observed_at = None;
    assert!(draft.validate_shape().is_err());
    draft.observed_at = Some("".into());
    assert!(draft.validate_shape().is_err());
    draft.observed_at = Some("2026-09-11T12:00:00Z".into());
    draft.content = ObjectStateContent::Relation {
        relation: ObjectStateRelation::RelatedTo,
        target: REFERENCE.into(),
        note: "".into(),
    };
    assert!(draft.validate_shape().is_err());
    draft.content = ObjectStateContent::Retracted {
        reason: "Correction".into(),
    };
    draft.supersedes = Some(PREDECESSOR.into());
    assert!(draft.validate_shape().is_err());
    draft.observed_at = None;
    draft.valid_until = None;
    assert!(draft.validate_shape().is_ok());
    draft.supersedes = None;
    assert!(draft.validate_shape().is_err());
    draft.supersedes = Some(draft.id.clone());
    assert!(draft.validate_shape().is_err());
    draft.supersedes = Some(" ".into());
    assert!(draft.validate_shape().is_err());
}

#[test]
fn activity_object_state_entry_identity_receipt_and_validity_are_checked() {
    let entry: ObjectStateEntry = serde_json::from_value(entry_value()).unwrap();
    assert!(entry.matches_activity(ACTIVITY));
    assert!(entry.matches_submission(ACTIVITY, &entry.draft));
    assert!(!entry.matches_activity(PREDECESSOR));
    for (field, value) in [
        ("id", json!(PREDECESSOR)),
        ("activity_id", json!("")),
        ("recorded_at", json!("")),
        ("superseded_by", json!(ENTRY)),
        ("superseded_by", json!("")),
        ("validity", json!("expired")),
        ("receipt", report_value()),
    ] {
        let mut value_with_error = entry_value();
        value_with_error[field] = value;
        let entry: ObjectStateEntry = serde_json::from_value(value_with_error).unwrap();
        assert!(!entry.matches_activity(ACTIVITY), "{field}");
    }
    let mut value = entry_value();
    value["draft"]["content"] = json!({"kind": "app_report", "receipt_id": RECEIPT});
    let missing_receipt: ObjectStateEntry = serde_json::from_value(value.clone()).unwrap();
    assert!(!missing_receipt.matches_activity(ACTIVITY));
    value["receipt"] = report_value();
    let report: ObjectStateEntry = serde_json::from_value(value.clone()).unwrap();
    assert!(report.matches_activity(ACTIVITY));
    assert_eq!(serde_json::to_value(&report).unwrap(), value);
    let mut changed = report.draft.clone();
    changed.reference = "app://kv/entry?id=other".into();
    assert!(!report.matches_submission(ACTIVITY, &changed));
}

#[test]
fn activity_object_state_acknowledgements_allow_trimmed_text_and_equivalent_utc_windows() {
    for (start, end, normalized_start, normalized_end) in [
        (
            "2026-09-11T05:00:00.125-07:00",
            "2026-09-11T07:30:00.875-05:00",
            "2026-09-11T12:00:00.125000000Z",
            "2026-09-11T12:30:00.875000000Z",
        ),
        (
            "2026-01-01T00:30:00+02:00",
            "2026-01-01T01:30:00+02:00",
            "2025-12-31T22:30:00.000000000Z",
            "2025-12-31T23:30:00.000000000Z",
        ),
    ] {
        for inference in [false, true] {
            let text = " \tReported  observation\nwith interior whitespace\u{2003}";
            let mut entry: ObjectStateEntry = serde_json::from_value(entry_value()).unwrap();
            let mut submitted = entry.draft.clone();
            submitted.content = if inference {
                ObjectStateContent::AgentInference { text: text.into() }
            } else {
                ObjectStateContent::UserStatement { text: text.into() }
            };
            submitted.observed_at = Some(start.into());
            submitted.valid_until = Some(end.into());
            let original_submission = submitted.clone();
            entry.draft = submitted.clone();
            match &mut entry.draft.content {
                ObjectStateContent::UserStatement { text }
                | ObjectStateContent::AgentInference { text } => *text = text.trim().into(),
                _ => unreachable!(),
            }
            entry.draft.observed_at = Some(normalized_start.into());
            entry.draft.valid_until = Some(normalized_end.into());
            entry.validity = ObjectStateValidity::WithinReportedWindow;
            assert_ne!(entry.draft, submitted);
            assert!(entry.matches_submission(ACTIVITY, &submitted));
            assert_eq!(
                submitted, original_submission,
                "matching must not rewrite retries"
            );
        }
    }
}

#[test]
fn activity_object_state_acknowledgements_compare_uuid_identity_including_receipt_and_predecessor()
{
    let activity_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let entry_id = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";
    let predecessor_id = "cccccccc-dddd-4eee-8fff-aaaaaaaaaaaa";
    let receipt_id = "dddddddd-eeee-4fff-8aaa-bbbbbbbbbbbb";
    let mut entry: ObjectStateEntry = serde_json::from_value(entry_value()).unwrap();
    entry.activity_id = activity_id.into();
    entry.id = entry_id.into();
    entry.draft.id = entry_id.into();
    entry.draft.supersedes = Some(predecessor_id.into());
    entry.draft.content = ObjectStateContent::AppReport {
        receipt_id: receipt_id.into(),
    };
    entry.receipt = Some(serde_json::from_value(report_value()).unwrap());
    let mut submitted = entry.draft.clone();
    submitted.id = Uuid::parse_str(entry_id)
        .unwrap()
        .simple()
        .to_string()
        .to_uppercase();
    submitted.supersedes = Some(format!("{{{}}}", predecessor_id.to_uppercase()));
    submitted.content = ObjectStateContent::AppReport {
        receipt_id: receipt_id.to_uppercase(),
    };
    assert!(entry.matches_submission(&activity_id.to_uppercase(), &submitted));
    submitted.supersedes = Some(RECEIPT.into());
    assert!(!entry.matches_submission(activity_id, &submitted));
    submitted.supersedes = None;
    assert!(!entry.matches_submission(activity_id, &submitted));
    submitted.supersedes = entry.draft.supersedes.clone();
    submitted.content = ObjectStateContent::AppReport {
        receipt_id: PREDECESSOR.into(),
    };
    assert!(!entry.matches_submission(activity_id, &submitted));
    submitted.content = entry.draft.content.clone();
    submitted.id = PREDECESSOR.into();
    assert!(!entry.matches_submission(activity_id, &submitted));
    submitted.id = entry_id.into();
    assert!(!entry.matches_submission(ACTIVITY, &submitted));
    submitted.id = "not-a-uuid".into();
    entry.id = submitted.id.clone();
    entry.draft.id = submitted.id.clone();
    assert!(!entry.matches_submission(activity_id, &submitted));
}

#[test]
fn activity_object_state_acknowledgements_only_trim_notes_and_retraction_reasons() {
    for content in [
        ObjectStateContent::Relation {
            relation: ObjectStateRelation::DependsOn,
            target: "app://kv/entry?id=notes%2Fa".into(),
            note: "\tPlanning  note \n".into(),
        },
        ObjectStateContent::Retracted {
            reason: "\u{2003}Mistaken  report\n".into(),
        },
    ] {
        let mut entry: ObjectStateEntry = serde_json::from_value(entry_value()).unwrap();
        let mut submitted = entry.draft.clone();
        submitted.content = content;
        submitted.supersedes = Some(PREDECESSOR.into());
        entry.draft = submitted.clone();
        match &mut entry.draft.content {
            ObjectStateContent::Relation { note, .. } => *note = note.trim().into(),
            ObjectStateContent::Retracted { reason } => *reason = reason.trim().into(),
            _ => unreachable!(),
        }
        assert!(entry.matches_submission(ACTIVITY, &submitted));
        match &mut entry.draft.content {
            ObjectStateContent::Relation { note, .. } => *note = "Planning note".into(),
            ObjectStateContent::Retracted { reason } => *reason = "Mistaken report".into(),
            _ => unreachable!(),
        }
        assert!(
            !entry.matches_submission(ACTIVITY, &submitted),
            "interior text is not normalized"
        );
    }
    let mut entry: ObjectStateEntry = serde_json::from_value(entry_value()).unwrap();
    entry.draft.content = ObjectStateContent::Relation {
        relation: ObjectStateRelation::RelatedTo,
        target: REFERENCE.into(),
        note: "".into(),
    };
    let submitted = entry.draft.clone();
    entry.draft.content = ObjectStateContent::Relation {
        relation: ObjectStateRelation::DependsOn,
        target: REFERENCE.into(),
        note: "".into(),
    };
    assert!(!entry.matches_submission(ACTIVITY, &submitted));
    entry.draft.content = ObjectStateContent::Relation {
        relation: ObjectStateRelation::RelatedTo,
        target: "app://kv/entry?id=other".into(),
        note: "".into(),
    };
    assert!(!entry.matches_submission(ACTIVITY, &submitted));
}

#[test]
fn activity_object_state_acknowledgements_reject_changed_content_instants_and_window_shape() {
    let mut entry: ObjectStateEntry = serde_json::from_value(entry_value()).unwrap();
    let mut submitted = entry.draft.clone();
    submitted.content = ObjectStateContent::UserStatement {
        text: " \tReported observation\n".into(),
    };
    submitted.observed_at = Some("2026-09-11T05:00:00.000000001-07:00".into());
    submitted.valid_until = Some("2026-09-11T05:30:00-07:00".into());
    entry.draft = submitted.clone();
    entry.draft.content = ObjectStateContent::UserStatement {
        text: "Reported observation".into(),
    };
    entry.draft.observed_at = Some("2026-09-11T12:00:00.000000001Z".into());
    entry.draft.valid_until = Some("2026-09-11T12:30:00.000000000Z".into());
    entry.validity = ObjectStateValidity::WithinReportedWindow;
    assert!(entry.matches_submission(ACTIVITY, &submitted));
    for mismatch in 0..7 {
        let mut changed = entry.clone();
        match mismatch {
            0 => {
                changed.draft.content = ObjectStateContent::UserStatement {
                    text: "Different report".into(),
                }
            }
            1 => {
                changed.draft.content = ObjectStateContent::AgentInference {
                    text: "Reported observation".into(),
                }
            }
            2 => changed.draft.observed_at = Some("2026-09-11T12:00:00.000000000Z".into()),
            3 => changed.draft.valid_until = Some("2026-09-11T12:30:01.000000000Z".into()),
            4 => changed.draft.valid_until = None,
            5 => {
                changed.draft.observed_at = None;
                changed.draft.valid_until = None;
                changed.validity = ObjectStateValidity::Unknown;
            }
            _ => changed.draft.reference = "app://kv/entry?id=other".into(),
        }
        assert!(
            !changed.matches_submission(ACTIVITY, &submitted),
            "mismatch {mismatch}"
        );
    }
    for invalid_time in [
        "not RFC3339",
        "2026-09-11T12:30:00Z",
        "2026-09-12T12:30:00Z",
    ] {
        let mut invalid = entry.clone();
        invalid.draft.observed_at = Some(invalid_time.into());
        assert!(!invalid.matches_submission(ACTIVITY, &invalid.draft));
    }
}

#[test]
fn activity_object_state_incoming_source_validity_owner_and_report_claims_are_closed() {
    for (field, value) in [
        ("source", json!("os_confirmed")),
        ("source", json!("verified")),
        ("validity", json!("current")),
        ("validity", json!("fresh")),
        ("owner_uid", json!(-1)),
        ("owner_uid", json!(4294967296_u64)),
        ("owner_uid", json!("1000")),
        ("recorded_at", json!(true)),
    ] {
        let mut invalid = entry_value();
        invalid[field] = value;
        assert!(serde_json::from_value::<ObjectStateEntry>(invalid).is_err());
    }
    for field in ["source", "owner_uid", "validity", "draft"] {
        let mut value = entry_value();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<ObjectStateEntry>(value).is_err());
    }
    let mut value = entry_value();
    value["draft"]["content"] = json!({"kind": "app_report", "receipt_id": RECEIPT});
    value["receipt"] = report_value();
    value["receipt"]["outcome"] = json!("verified");
    assert!(serde_json::from_value::<ObjectStateEntry>(value).is_err());
}

#[test]
fn activity_object_state_lists_are_bounded_filterable_and_keep_superseded_history() {
    let mut response: ActivityObjectStateResponse = serde_json::from_value(json!({
        "schema": 1, "activity_id": ACTIVITY, "entries": [entry_value()]
    }))
    .unwrap();
    response.entries[0].superseded_by = Some(PREDECESSOR.into());
    assert!(response.matches_activity(ACTIVITY));
    assert!(response.matches_query(ACTIVITY, &ActivityObjectStateQuery::default()));
    assert!(!response.matches_query(
        ACTIVITY,
        &ActivityObjectStateQuery {
            reference: Some("app://kv/entry?id=other".into()),
            limit: Some(100),
        }
    ));
    response.entries.push(response.entries[0].clone());
    assert!(!response.matches_activity(ACTIVITY));
    response.entries.clear();
    for index in 0..100 {
        let mut entry: ObjectStateEntry = serde_json::from_value(entry_value()).unwrap();
        entry.id = format!("00000000-0000-4000-8000-{index:012}");
        entry.draft.id = entry.id.clone();
        response.entries.push(entry);
    }
    assert!(response.matches_activity(ACTIVITY));
    assert!(!response.matches_query(ACTIVITY, &ActivityObjectStateQuery::default()));
    assert!(response.matches_query(
        ACTIVITY,
        &ActivityObjectStateQuery {
            reference: Some(REFERENCE.into()),
            limit: Some(100),
        }
    ));
    response.entries.push(response.entries[0].clone());
    assert!(!response.matches_activity(ACTIVITY));
    response.entries.clear();
    response.schema = 2;
    assert!(!response.matches_activity(ACTIVITY));
    response.schema = 1;
    response.activity_id = PREDECESSOR.into();
    assert!(!response.matches_activity(ACTIVITY));
}

#[test]
fn activity_object_state_queries_default_to_no_selectors_and_reject_invalid_bounds() {
    assert_eq!(
        serde_json::to_value(ActivityObjectStateQuery::default()).unwrap(),
        json!({})
    );
    for (limit, valid) in [(0, false), (1, true), (50, true), (100, true), (101, false)] {
        assert_eq!(
            ActivityObjectStateQuery {
                limit: Some(limit),
                reference: None
            }
            .validate_shape()
            .is_ok(),
            valid
        );
    }
    assert!(
        ActivityObjectStateQuery {
            reference: Some(" ".into()),
            limit: None,
        }
        .validate_shape()
        .is_err()
    );
    for reference in [json!(123), json!(["app://kv/entry?id=x"])] {
        assert!(
            serde_json::from_value::<ActivityObjectStateQuery>(json!({"reference": reference}))
                .is_err()
        );
    }
}
