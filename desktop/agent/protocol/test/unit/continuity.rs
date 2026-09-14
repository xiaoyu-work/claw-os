use super::*;
use serde_json::json;

const LINEAGE: &str = "00000000-0000-4000-8000-000000000123";
const IMPORTED: &str = "11111111-1111-4111-8111-111111111111";

fn document() -> ActivityContinuityDocument {
    let mut document = ActivityContinuityDocument {
        kind: ACTIVITY_CONTINUITY_KIND.into(),
        schema_version: ACTIVITY_CONTINUITY_SCHEMA_VERSION,
        lineage: ActivityContinuityLineage {
            id: LINEAGE.into(),
            revision: 9_007_199_254_740_993,
        },
        snapshot: String::new(),
        intent: PortableActivityIntent {
            title: "Release".into(),
            goal: "Publish the release".into(),
            completion_criteria: "Reviewed and available".into(),
            boundaries: "Ask before publishing".into(),
        },
        references: vec![PortableActivityReference {
            label: "Status".into(),
            reference: "app://kv/entry?id=release.status&revision=v1".into(),
        }],
        rules: PortableActivityRules {
            execution_limits: Some(PortableExecutionLimits {
                enabled: false,
                max_attempts: 10,
                max_turns_per_attempt: 5,
                expires_at: "2030-01-01T00:00:00.000000000Z".into(),
            }),
            scheduling: Some(PortableSchedulingPreference {
                priority: ActivitySchedulingPriority::Foreground,
            }),
        },
    };
    document.snapshot = document.snapshot_digest().unwrap();
    document
}

fn acknowledgement(document: &ActivityContinuityDocument) -> ActivityContinuityImportAcknowledgement {
    ActivityContinuityImportAcknowledgement {
        activity: ImportedActivity {
            id: IMPORTED.into(),
            title: document.intent.title.clone(),
            goal: document.intent.goal.clone(),
            completion_criteria: document.intent.completion_criteria.clone(),
            boundaries: document.intent.boundaries.clone(),
            resources: document
                .references
                .iter()
                .map(|reference| ImportedActivityResource {
                    label: reference.label.clone(),
                    reference: reference.reference.clone(),
                })
                .collect(),
            state: ActivityState::Paused,
            completion_note: None,
            created_at: "2026-09-14T00:00:00Z".into(),
            updated_at: "2026-09-14T00:00:00Z".into(),
        },
        continuity_id: document.lineage.id.clone(),
        continuity_revision: document.lineage.revision,
        placement: ActivityExecutionPlacement::Local,
    }
}

#[test]
fn continuity_v1_round_trips_exact_shape_and_preserves_large_u64() {
    let document = document();
    let encoded = document.to_json().unwrap();
    let decoded = ActivityContinuityDocument::from_json(encoded.as_bytes()).unwrap();
    assert_eq!(decoded, document);
    let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        value["lineage"]["revision"].as_u64(),
        Some(9_007_199_254_740_993)
    );
    assert_eq!(value.as_object().unwrap().len(), 7);
    assert_eq!(value["rules"].as_object().unwrap().len(), 2);
}

#[test]
fn continuity_rejects_kind_version_uuid_digest_unknown_duplicate_and_authority() {
    for (field, replacement) in [
        ("kind", json!("other")),
        ("schema_version", json!(2)),
        ("snapshot", json!(format!("sha256:{}", "0".repeat(64)))),
    ] {
        let mut value = serde_json::to_value(document()).unwrap();
        value[field] = replacement;
        assert!(
            ActivityContinuityDocument::from_json(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{field}"
        );
    }
    let mut invalid_uuid = document();
    invalid_uuid.lineage.id = "AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE".into();
    assert!(invalid_uuid.validate().is_err());
    for field in [
        "owner_uid",
        "authority",
        "capabilities",
        "approvals",
        "jobs",
        "sessions",
        "receipts",
        "monetary_budget",
        "proof",
    ] {
        let mut value = serde_json::to_value(document()).unwrap();
        value[field] = json!(true);
        assert!(
            ActivityContinuityDocument::from_json(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{field}"
        );
    }
    let encoded = document().to_json().unwrap();
    let duplicate = encoded.replacen(
        r#""kind":"claw_os.activity_continuity""#,
        r#""kind":"claw_os.activity_continuity","kind":"claw_os.activity_continuity""#,
        1,
    );
    assert!(ActivityContinuityDocument::from_json(duplicate.as_bytes()).is_err());
}

#[test]
fn continuity_rejects_document_depth_string_count_text_rule_and_reference_bounds() {
    assert!(
        ActivityContinuityDocument::from_json(&vec![
            b' ';
            MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES + 1
        ])
        .is_err()
    );
    let deep = format!(
        "{}0{}",
        "[".repeat(MAX_JSON_DEPTH + 1),
        "]".repeat(MAX_JSON_DEPTH + 1)
    );
    assert!(ActivityContinuityDocument::from_json(deep.as_bytes()).is_err());
    let raw_string = format!(r#"{{"x":"{}"}}"#, "a".repeat(MAX_RAW_JSON_STRING_BYTES + 1));
    assert!(ActivityContinuityDocument::from_json(raw_string.as_bytes()).is_err());

    let mut value = document();
    value.references = (0..=MAX_REFERENCES)
        .map(|index| PortableActivityReference {
            label: format!("Reference {index}"),
            reference: format!("app://kv/entry?id={index}"),
        })
        .collect();
    assert!(value.validate().is_err());
    let mut value = document();
    value.intent.title = "x".repeat(MAX_TITLE_BYTES + 1);
    assert!(value.validate().is_err());
    let mut value = document();
    value.references[0].reference = "/home/user/private".into();
    assert!(value.validate().is_err());
    let mut value = document();
    value.rules.execution_limits.as_mut().unwrap().max_attempts = 1001;
    assert!(value.validate().is_err());
}

#[test]
fn import_request_is_owner_free_and_requires_explicit_local_placement() {
    let document = document();
    let request = ActivityContinuityImportRequest {
        placement: ActivityExecutionPlacement::Local,
        document: document.to_json().unwrap(),
    };
    assert_eq!(request.validated_document().unwrap(), document);
    for field in ["owner_uid", "authority", "restore", "live_sync"] {
        let mut value = serde_json::to_value(&request).unwrap();
        value[field] = json!(true);
        assert!(
            serde_json::from_value::<ActivityContinuityImportRequest>(value).is_err(),
            "{field}"
        );
    }
    assert!(
        serde_json::from_value::<ActivityContinuityImportRequest>(
            json!({"document": request.document})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityContinuityExportQuery>(json!({"owner_uid": 1000}))
            .is_err()
    );
}

#[test]
fn import_ack_requires_new_paused_activity_matching_lineage_revision_and_shape() {
    let document = document();
    let valid = acknowledgement(&document);
    assert!(valid.matches(&document, ActivityExecutionPlacement::Local));
    for mutation in 0..7 {
        let mut invalid = valid.clone();
        match mutation {
            0 => invalid.activity.id = document.lineage.id.clone(),
            1 => invalid.activity.state = ActivityState::Active,
            2 => invalid.activity.title.push('!'),
            3 => invalid.continuity_id = IMPORTED.into(),
            4 => invalid.continuity_revision += 1,
            5 => invalid.activity.completion_note = Some("restored".into()),
            _ => invalid.activity.resources.clear(),
        }
        assert!(!invalid.matches(&document, ActivityExecutionPlacement::Local));
    }
    let mut value = serde_json::to_value(valid).unwrap();
    value["activity"]["owner_uid"] = json!(1000);
    assert!(
        serde_json::from_value::<ActivityContinuityImportAcknowledgement>(value).is_err()
    );
}
