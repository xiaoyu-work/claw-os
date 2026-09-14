use super::*;
use cos_agent_protocol::{ActivitySchedulingPriority, PortableActivityRules};
use serde_json::json;

const LINEAGE: &str = "00000000-0000-4000-8000-000000000123";
const IMPORTED: &str = "11111111-1111-4111-8111-111111111111";
const SNAPSHOT: &str = "sha256:6d8c822b0519a02e7ab1ebeab48392cb83756fa9cc8b6d39e94b01ab1682882b";

fn document_value() -> Value {
    json!({
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
    })
}

fn acknowledgement() -> Value {
    json!({
        "activity": {
            "id": IMPORTED,
            "owner_uid": 1000,
            "title": "Release",
            "goal": "Publish the release",
            "completion_criteria": "Reviewed and available",
            "boundaries": "Ask before publishing",
            "resources": [{
                "label": "Status",
                "reference": "app://kv/entry?id=release.status&revision=v1"
            }],
            "state": "paused",
            "completion_note": null,
            "created_at": "2026-09-14T00:00:00Z",
            "updated_at": "2026-09-14T00:00:00Z"
        },
        "continuity_id": LINEAGE,
        "continuity_revision": 7,
        "placement": "local"
    })
}

#[test]
fn export_translation_validates_the_closed_document_without_core_types() {
    let document = export(document_value()).unwrap();
    assert_eq!(document.lineage.revision, 7);
    assert_eq!(
        document.rules,
        PortableActivityRules {
            execution_limits: document.rules.execution_limits.clone(),
            scheduling: document.rules.scheduling.clone(),
        }
    );
    assert_eq!(
        document.rules.scheduling.unwrap().priority,
        ActivitySchedulingPriority::Foreground
    );
    for field in ["owner_uid", "authority", "jobs", "proof"] {
        let mut invalid = document_value();
        invalid[field] = json!(true);
        assert!(export(invalid).is_err(), "{field}");
    }
    let mut invalid = document_value();
    invalid["snapshot"] = json!(format!("sha256:{}", "0".repeat(64)));
    assert!(export(invalid).is_err());
}

#[test]
fn import_translation_rejects_owner_authority_shape_and_ack_mismatches() {
    let document = export(document_value()).unwrap();
    let response = import(
        acknowledgement(),
        1000,
        &document,
        ActivityExecutionPlacement::Local,
    )
    .unwrap();
    assert_eq!(response.activity.id, IMPORTED);
    assert_eq!(response.activity.state, ActivityState::Paused);

    for (path, replacement) in [
        ("owner_uid", json!(1001)),
        ("state", json!("active")),
        ("id", json!(LINEAGE)),
        ("title", json!("Changed")),
    ] {
        let mut invalid = acknowledgement();
        invalid["activity"][path] = replacement;
        assert!(
            import(
                invalid,
                1000,
                &document,
                ActivityExecutionPlacement::Local
            )
            .is_err(),
            "{path}"
        );
    }
    for field in ["authority", "proof", "restored_capabilities"] {
        let mut invalid = acknowledgement();
        invalid[field] = json!(true);
        assert!(
            import(
                invalid,
                1000,
                &document,
                ActivityExecutionPlacement::Local
            )
            .is_err(),
            "{field}"
        );
    }
    let mut invalid = acknowledgement();
    invalid["continuity_revision"] = json!(8);
    assert!(
        import(
            invalid,
            1000,
            &document,
            ActivityExecutionPlacement::Local
        )
        .is_err()
    );
}
