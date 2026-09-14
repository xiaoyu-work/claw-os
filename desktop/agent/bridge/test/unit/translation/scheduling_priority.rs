use super::*;
use serde_json::json;

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn value() -> Value {
    json!({
        "activity_id": ACTIVITY,
        "owner_uid": 1000,
        "revision": 7,
        "priority": "foreground",
        "created_at": "2026-09-13T00:00:00Z",
        "updated_at": "2026-09-13T01:00:00Z"
    })
}

#[test]
fn scheduling_priority_translation_preserves_absence_and_closed_values() {
    let absent = get(
        json!({"schema": 1, "activity_id": ACTIVITY, "scheduling_policy": null}),
        ACTIVITY,
        1000,
    )
    .unwrap();
    assert!(absent.scheduling_policy.is_none());
    let response = get(
        json!({"schema": 1, "activity_id": ACTIVITY, "scheduling_policy": value()}),
        ACTIVITY,
        1000,
    )
    .unwrap();
    assert_eq!(
        response.scheduling_policy.unwrap().priority,
        cos_agent_protocol::ActivitySchedulingPriority::Foreground
    );
}

#[test]
fn scheduling_priority_translation_rejects_owner_authority_and_invalid_acknowledgements() {
    assert!(policy(value(), ACTIVITY, 1001).is_err());
    for envelope in [
        json!({"schema": 2, "activity_id": ACTIVITY, "scheduling_policy": value()}),
        json!({"schema": 1, "activity_id": ACTIVITY}),
        json!({"schema": 1, "activity_id": ACTIVITY, "scheduling_policy": false}),
    ] {
        assert!(get(envelope, ACTIVITY, 1000).is_err());
    }
    for (field, replacement) in [
        ("owner_uid", json!(1001)),
        ("revision", json!(0)),
        ("priority", json!("urgent")),
        ("created_at", json!("invalid")),
    ] {
        let mut invalid = value();
        invalid[field] = replacement;
        assert!(policy(invalid, ACTIVITY, 1000).is_err());
    }
    for field in ["authority", "job_id", "preempt", "cancel"] {
        let mut invalid = value();
        invalid[field] = json!(true);
        assert!(policy(invalid, ACTIVITY, 1000).is_err(), "{field}");
    }
}
