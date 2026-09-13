use super::*;
use serde_json::json;

fn statement() -> ObjectStateDraft {
    ObjectStateDraft {
        id: uuid::Uuid::new_v4().to_string(),
        reference: "app://kv/entry?id=release.status".into(),
        content: ObjectStateContent::UserStatement {
            text: "The caller reports readiness.\n\tReview is still required.".into(),
        },
        observed_at: None,
        valid_until: None,
        supersedes: None,
    }
}

fn relation() -> ObjectStateContent {
    ObjectStateContent::Relation {
        relation: ObjectRelationKind::DependsOn,
        target: "app://kv/entry?id=review.status".into(),
        note: String::new(),
    }
}

fn text_content(kind: &str, text: String) -> ObjectStateContent {
    match kind {
        "statement" => ObjectStateContent::UserStatement { text },
        "inference" => ObjectStateContent::AgentInference { text },
        "retracted" => ObjectStateContent::Retracted { reason: text },
        "relation" => ObjectStateContent::Relation {
            relation: ObjectRelationKind::RelatedTo,
            target: "app://kv/entry?id=another".into(),
            note: text,
        },
        _ => panic!("unknown test content kind"),
    }
}

#[test]
fn optional_fields_and_relation_notes_default_without_accepting_authority_fields() {
    let mut value = serde_json::to_value(statement()).unwrap();
    for field in ["observed_at", "valid_until", "supersedes"] {
        value.as_object_mut().unwrap().remove(field);
    }
    let decoded: ObjectStateDraft = serde_json::from_value(value.clone()).unwrap();
    decoded.validate().unwrap();
    assert!(decoded.observed_at.is_none());
    assert!(decoded.valid_until.is_none());
    assert!(decoded.supersedes.is_none());
    for field in [
        "source",
        "owner_uid",
        "activity_id",
        "recorded_at",
        "authentication",
        "confirmed",
        "receipt",
        "validity",
        "superseded_by",
        "grant",
    ] {
        let mut forged = value.clone();
        forged[field] = json!("forged");
        assert!(
            serde_json::from_value::<ObjectStateDraft>(forged).is_err(),
            "{field}"
        );
    }
    value["content"] = json!({
        "kind": "relation",
        "relation": "depends_on",
        "target": "app://kv/entry?id=review.status"
    });
    let decoded: ObjectStateDraft = serde_json::from_value(value).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded.content, relation());
}

#[test]
fn nullable_draft_and_entry_fields_serialize_as_null_in_the_frozen_output_shape() {
    let draft = statement();
    let expected_draft = json!({
        "id": draft.id,
        "reference": draft.reference,
        "content": draft.content,
        "observed_at": null,
        "valid_until": null,
        "supersedes": null,
    });
    assert_eq!(serde_json::to_value(&draft).unwrap(), expected_draft);

    let entry = ObjectStateEntry {
        id: draft.id.clone(),
        activity_id: uuid::Uuid::new_v4().to_string(),
        owner_uid: 7,
        recorded_at: "2026-01-01T00:00:00.000000000Z".into(),
        source: ObjectStateSource::CallerReported,
        draft,
        receipt: None,
        superseded_by: None,
        validity: ObjectStateValidity::Unknown,
    };
    let serialized = serde_json::to_value(&entry).unwrap();
    assert_eq!(
        serialized,
        json!({
            "id": entry.id,
            "activity_id": entry.activity_id,
            "owner_uid": 7,
            "recorded_at": "2026-01-01T00:00:00.000000000Z",
            "source": "caller_reported",
            "draft": expected_draft,
            "receipt": null,
            "superseded_by": null,
            "validity": "unknown",
        })
    );
    assert_eq!(
        serde_json::from_value::<ObjectStateEntry>(serialized).unwrap(),
        entry
    );
}

#[test]
fn tagged_content_has_strict_fields_and_app_reports_have_no_text_override() {
    let cases = [
        (
            ObjectStateContent::UserStatement {
                text: "Report".into(),
            },
            "user_statement",
        ),
        (
            ObjectStateContent::AgentInference {
                text: "Possibly".into(),
            },
            "agent_inference",
        ),
        (
            ObjectStateContent::AppReport {
                receipt_id: uuid::Uuid::new_v4().to_string(),
            },
            "app_report",
        ),
        (relation(), "relation"),
        (
            ObjectStateContent::Retracted {
                reason: "Correction".into(),
            },
            "retracted",
        ),
    ];
    for (content, kind) in cases {
        let value = serde_json::to_value(&content).unwrap();
        assert_eq!(value["kind"], kind);
        assert_eq!(
            serde_json::from_value::<ObjectStateContent>(value.clone()).unwrap(),
            content
        );
        for field in [
            "owner_uid",
            "source",
            "confirmed",
            "authentication",
            "report",
        ] {
            let mut forged = value.clone();
            forged[field] = json!("forged");
            assert!(
                serde_json::from_value::<ObjectStateContent>(forged).is_err(),
                "{kind}.{field}"
            );
        }
    }
    for value in [
        json!({"kind":"app_report","receipt_id":uuid::Uuid::new_v4().to_string(),"text":"override"}),
        json!({"kind":"app_report","text":"not a receipt"}),
        json!({"kind":"verified","text":"confirmed"}),
        json!({"kind":"user_statement","text":"Report","reason":"extra"}),
        json!({"kind":"retracted","reason":"Report","text":"extra"}),
        json!({"kind":"relation","relation":"executes","target":"app://kv/entry?id=b"}),
    ] {
        assert!(serde_json::from_value::<ObjectStateContent>(value).is_err());
    }
}

#[test]
fn source_relations_and_validity_keep_the_frozen_wire_spellings() {
    assert_eq!(
        serde_json::to_value(ObjectStateSource::CallerReported).unwrap(),
        "caller_reported"
    );
    for invalid in ["user", "agent", "authenticated", "os_confirmed", "verified"] {
        assert!(serde_json::from_value::<ObjectStateSource>(json!(invalid)).is_err());
    }
    for (kind, name) in [
        (ObjectRelationKind::RelatedTo, "related_to"),
        (ObjectRelationKind::DependsOn, "depends_on"),
        (ObjectRelationKind::DerivedFrom, "derived_from"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), name);
        assert_eq!(
            serde_json::from_value::<ObjectRelationKind>(json!(name)).unwrap(),
            kind
        );
    }
    for (validity, name) in [
        (ObjectStateValidity::Unknown, "unknown"),
        (ObjectStateValidity::NotYetApplicable, "not_yet_applicable"),
        (
            ObjectStateValidity::WithinReportedWindow,
            "within_reported_window",
        ),
        (ObjectStateValidity::Expired, "expired"),
    ] {
        assert_eq!(serde_json::to_value(validity).unwrap(), name);
        assert_eq!(
            serde_json::from_value::<ObjectStateValidity>(json!(name)).unwrap(),
            validity
        );
    }
    for invalid in ["fresh", "true", "current", "verified"] {
        assert!(serde_json::from_value::<ObjectStateValidity>(json!(invalid)).is_err());
    }
}

#[test]
fn object_references_use_only_the_canonical_public_sdk_format() {
    let object = crate::objects::ObjectRef {
        app_id: "fs".into(),
        object_type: "file".into(),
        object_id: "/drafts/release \u{e9}.md".into(),
        revision: Some("v1 / reviewed".into()),
    };
    let canonical = crate::objects::format_reference(&object).unwrap();
    let mut value = statement();
    value.reference = canonical.clone();
    value.validate().unwrap();
    assert_eq!(value.canonicalized().unwrap().reference, canonical);
    for reference in [
        "https://example.invalid/data",
        "app://kv/entry?id=key&revision=v1&revision=v2",
        "app://kv/entry?revision=v1&id=key",
        "app://kv/entry?id=%6Bey",
        "app://fs/file?id=%2fdraft",
        "App://kv/entry?id=key",
        "app://kv/entry?id=key#fragment",
        "app://kv/entry?id=key ",
        "app://kv/entry?id=%00",
        "app://kv/entry?id=",
    ] {
        let mut value = statement();
        value.reference = reference.into();
        assert!(value.validate().is_err(), "{reference}");
        value = statement();
        value.content = ObjectStateContent::Relation {
            relation: ObjectRelationKind::DerivedFrom,
            target: reference.into(),
            note: String::new(),
        };
        assert!(value.validate().is_err(), "target {reference}");
    }
    let mut value = statement();
    value.content = ObjectStateContent::Relation {
        relation: ObjectRelationKind::RelatedTo,
        target: value.reference.clone(),
        note: String::new(),
    };
    assert!(value.validate().is_err());
}

#[test]
fn uuid_keys_and_links_canonicalize_like_receipts_and_cannot_point_to_self() {
    let id = uuid::Uuid::new_v4();
    let receipt = uuid::Uuid::new_v4();
    let previous = uuid::Uuid::new_v4();
    let mut original = statement();
    original.id = id.to_string();
    original.content = ObjectStateContent::AppReport {
        receipt_id: receipt.to_string(),
    };
    original.supersedes = Some(previous.to_string());
    for (key, receipt_key, previous_key) in [
        (
            id.to_string().to_uppercase(),
            receipt.to_string().to_uppercase(),
            previous.to_string().to_uppercase(),
        ),
        (
            id.simple().to_string(),
            receipt.simple().to_string(),
            previous.simple().to_string(),
        ),
        (
            id.urn().to_string(),
            receipt.urn().to_string(),
            previous.urn().to_string(),
        ),
    ] {
        let mut value = original.clone();
        value.id = key;
        value.content = ObjectStateContent::AppReport {
            receipt_id: receipt_key,
        };
        value.supersedes = Some(previous_key);
        assert_eq!(value.canonicalized().unwrap(), original);
    }
    for invalid in [
        "",
        "not-a-uuid",
        "00000000\0",
        " 00000000-0000-0000-0000-000000000001",
    ] {
        let mut value = original.clone();
        value.id = invalid.into();
        assert!(value.validate().is_err());
        value = original.clone();
        value.supersedes = Some(invalid.into());
        assert!(value.validate().is_err());
        value = original.clone();
        value.content = ObjectStateContent::AppReport {
            receipt_id: invalid.into(),
        };
        assert!(value.validate().is_err());
    }
    original.supersedes = Some(id.simple().to_string());
    assert!(original.validate().is_err());
}

#[test]
fn text_and_notes_enforce_trimmed_utf8_byte_limits_and_ordinary_multiline_controls() {
    for (kind, limit) in [
        ("statement", 4096),
        ("inference", 4096),
        ("retracted", 4096),
        ("relation", 2048),
    ] {
        let mut value = statement();
        value.supersedes = Some(uuid::Uuid::new_v4().to_string());
        value.content = text_content(kind, format!(" \n{}\t ", "\u{e9}".repeat(limit / 2)));
        value.validate().unwrap();
        assert_eq!(
            value.clone().canonicalized().unwrap().content,
            text_content(kind, "\u{e9}".repeat(limit / 2))
        );
        value.content = text_content(kind, format!("{}x", "\u{e9}".repeat(limit / 2)));
        assert!(value.validate().is_err(), "{kind} limit");
        value.content = text_content(kind, " \n\t\r ".into());
        assert_eq!(value.validate().is_ok(), kind == "relation", "{kind} empty");
        for control in ['\0', '\u{1b}', '\u{7f}', '\u{85}', '\u{0b}', '\u{0c}'] {
            value.content = text_content(kind, format!("Report{control}"));
            assert!(value.validate().is_err(), "{kind} {control:?}");
        }
        value.content = text_content(kind, "One line.\n\r\tAnother line.".into());
        value.validate().unwrap();
    }
}

#[test]
fn retractions_require_history_and_planning_links_cannot_claim_validity_windows() {
    let mut value = statement();
    value.content = ObjectStateContent::Retracted {
        reason: "Withdrawn".into(),
    };
    assert!(value.validate().is_err());
    value.supersedes = Some(uuid::Uuid::new_v4().to_string());
    value.validate().unwrap();
    for content in [value.content.clone(), relation()] {
        value.content = content;
        value.observed_at = Some("2026-01-01T00:00:00Z".into());
        value.valid_until = Some("2026-01-02T00:00:00Z".into());
        assert!(value.validate().is_err());
        value.valid_until = None;
        assert!(value.validate().is_err());
        value.observed_at = None;
        value.valid_until = Some("2026-01-02T00:00:00Z".into());
        assert!(value.validate().is_err());
    }
}

#[test]
fn reported_windows_require_rfc3339_pairs_in_strict_instant_order() {
    let mut value = statement();
    for (start, end) in [
        (Some("2026-01-01T00:00:00Z"), None),
        (None, Some("2026-01-02T00:00:00Z")),
        (Some("yesterday"), Some("tomorrow")),
        (Some("2026-01-01T00:00:00"), Some("2026-01-02T00:00:00Z")),
        (Some("2026-01-01T00:00:00Z"), Some("2026-01-01T00:00:00Z")),
        (Some("2026-01-02T00:00:00Z"), Some("2026-01-01T00:00:00Z")),
        (
            Some("2026-01-01T01:00:00+01:00"),
            Some("2026-01-01T00:00:00Z"),
        ),
    ] {
        value.observed_at = start.map(str::to_string);
        value.valid_until = end.map(str::to_string);
        assert!(value.validate().is_err(), "{start:?} .. {end:?}");
    }
    value.observed_at = Some("2026-01-01T01:00:00.000000001+01:00".into());
    value.valid_until = Some("2025-12-31T16:00:00.000000002-08:00".into());
    let canonical = value.canonicalized().unwrap();
    assert_eq!(
        canonical.observed_at.as_deref(),
        Some("2026-01-01T00:00:00.000000001Z")
    );
    assert_eq!(
        canonical.valid_until.as_deref(),
        Some("2026-01-01T00:00:00.000000002Z")
    );
}

#[test]
fn validity_is_a_clock_projection_of_a_half_open_reported_window_not_truth() {
    let at = |value: &str| {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    };
    let mut value = statement();
    assert_eq!(
        value.validity_at(at("2026-01-01T00:00:00Z")).unwrap(),
        ObjectStateValidity::Unknown
    );
    value.observed_at = Some("2026-01-01T00:00:00Z".into());
    value.valid_until = Some("2026-01-01T01:00:00Z".into());
    for (now, expected) in [
        (
            "2025-12-31T23:59:59.999999999Z",
            ObjectStateValidity::NotYetApplicable,
        ),
        (
            "2026-01-01T00:00:00Z",
            ObjectStateValidity::WithinReportedWindow,
        ),
        (
            "2026-01-01T00:59:59.999999999Z",
            ObjectStateValidity::WithinReportedWindow,
        ),
        ("2026-01-01T01:00:00Z", ObjectStateValidity::Expired),
        ("2027-01-01T00:00:00Z", ObjectStateValidity::Expired),
    ] {
        assert_eq!(value.validity_at(at(now)).unwrap(), expected);
    }
}
