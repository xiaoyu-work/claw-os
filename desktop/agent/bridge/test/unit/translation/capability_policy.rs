use super::*;
use serde_json::json;

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OTHER: &str = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";

fn value() -> Value {
    json!({
        "activity_id": ACTIVITY, "owner_uid": 1000, "revision": 5, "enabled": false,
        "rules": [{"verb": "fs.read", "mode": "require_approval", "scopes": [{"kind": "path", "value": "/workspace/**"}]}],
        "created_at": "2026-09-11T12:00:00Z", "updated_at": "2026-09-11T13:00:00Z"
    })
}

#[test]
fn capability_policy_translation_preserves_null_disabled_and_typed_constraints() {
    let absent = get(
        json!({"schema": 1, "activity_id": ACTIVITY, "capability_policy": null}),
        ACTIVITY,
        1000,
    )
    .unwrap();
    assert!(serde_json::to_value(absent).unwrap()["capability_policy"].is_null());
    let projected = get(
        json!({"schema": 1, "activity_id": ACTIVITY, "capability_policy": value()}),
        ACTIVITY,
        1000,
    )
    .unwrap();
    let policy = projected.capability_policy.unwrap();
    assert!(!policy.enabled);
    assert_eq!(
        policy.rules[0].mode,
        cos_agent_protocol::CapabilityPolicyMode::RequireApproval
    );
    assert_eq!(policy.rules[0].scopes[0].value(), Some("/workspace/**"));
}

#[test]
fn capability_policy_translation_rejects_owner_scope_shape_and_missing_data() {
    assert!(policy(value(), ACTIVITY, 1001).is_err());
    assert!(policy(value(), OTHER, 1000).is_err());
    for envelope in [
        json!({"schema": 1, "activity_id": ACTIVITY}),
        json!({"schema": 2, "activity_id": ACTIVITY, "capability_policy": value()}),
        json!({"schema": 1, "activity_id": OTHER, "capability_policy": value()}),
        json!({"schema": 1, "activity_id": ACTIVITY, "capability_policy": false}),
    ] {
        assert!(get(envelope, ACTIVITY, 1000).is_err());
    }
    let mut data = value();
    data["rules"][0]["mode"] = json!("allow");
    assert!(policy(data, ACTIVITY, 1000).is_err());
    let mut data = value();
    data["rules"][0]["mode"] = json!("deny");
    assert!(
        policy(data, ACTIVITY, 1000).is_err(),
        "deny cannot be scoped"
    );
}

#[test]
fn capability_policy_translation_cannot_project_fake_grants_or_approvals() {
    let mut data = value();
    data["granted"] = json!(true);
    data["approved"] = json!(true);
    data["worker_token"] = json!("not projected");
    let projected = serde_json::to_value(policy(data, ACTIVITY, 1000).unwrap()).unwrap();
    assert_eq!(projected.as_object().unwrap().len(), 7);
    for field in ["granted", "approved", "worker_token"] {
        assert!(projected.get(field).is_none());
    }
}
