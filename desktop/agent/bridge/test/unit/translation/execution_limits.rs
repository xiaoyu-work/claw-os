use super::*;
use serde_json::json;

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OTHER: &str = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";

fn value() -> Value {
    json!({
        "activity_id": ACTIVITY, "owner_uid": 1000, "revision": 7, "enabled": false,
        "limits": {"max_attempts": 3, "max_turns_per_attempt": 20, "expires_at": "2000-01-01T00:00:00Z"},
        "used_attempts": 8, "created_at": "1999-01-01T00:00:00Z",
        "updated_at": "1999-12-01T00:00:00Z"
    })
}

#[test]
fn activity_execution_limits_translation_preserves_null_disabled_expired_and_exhausted_data() {
    let absent = get(
        json!({
            "schema": 1, "activity_id": ACTIVITY, "execution_limits": null
        }),
        ACTIVITY,
        1000,
    )
    .unwrap();
    assert!(absent.execution_limits.is_none());
    assert!(serde_json::to_value(absent).unwrap()["execution_limits"].is_null());
    let response = get(
        json!({
            "schema": 1, "activity_id": ACTIVITY, "execution_limits": value()
        }),
        ACTIVITY,
        1000,
    )
    .unwrap();
    let limits = response.execution_limits.unwrap();
    assert!(!limits.enabled);
    assert_eq!(limits.used_attempts, 8);
    assert_eq!(limits.limits.max_attempts, 3);
    assert_eq!(limits.remaining_attempts(), 0);
    assert_eq!(limits.limits.expires_at, "2000-01-01T00:00:00Z");
}

#[test]
fn activity_execution_limits_translation_rejects_wrong_owner_activity_schema_and_missing_policy() {
    assert!(policy(value(), ACTIVITY, 1001).is_err());
    assert!(policy(value(), OTHER, 1000).is_err());
    for envelope in [
        json!({"schema": 2, "activity_id": ACTIVITY, "execution_limits": value()}),
        json!({"schema": 1, "activity_id": OTHER, "execution_limits": value()}),
        json!({"schema": 1, "activity_id": ACTIVITY}),
        json!({"schema": 1, "activity_id": ACTIVITY, "execution_limits": false}),
    ] {
        assert!(get(envelope, ACTIVITY, 1000).is_err());
    }
    let mut invalid_owner = value();
    invalid_owner["owner_uid"] = json!(1001);
    assert!(
        get(
            json!({
                "schema": 1, "activity_id": ACTIVITY, "execution_limits": invalid_owner
            }),
            ACTIVITY,
            1000
        )
        .is_err()
    );
    for (field, replacement) in [
        ("revision", json!(0)),
        ("enabled", json!("false")),
        ("owner_uid", json!(-1)),
        ("updated_at", json!("invalid")),
    ] {
        let mut invalid = value();
        invalid[field] = replacement;
        assert!(policy(invalid, ACTIVITY, 1000).is_err());
    }
    let mut invalid = value();
    invalid["limits"]["max_turns_per_attempt"] = json!(101);
    assert!(policy(invalid, ACTIVITY, 1000).is_err());
}

#[test]
fn activity_execution_limits_translation_projects_only_constraint_fields_not_authority() {
    let mut raw = value();
    raw["caps"] = json!(["fs:write:*"]);
    raw["approved"] = json!(true);
    raw["worker_state"] = json!("private");
    let projected = serde_json::to_value(policy(raw, ACTIVITY, 1000).unwrap()).unwrap();
    assert_eq!(projected.as_object().unwrap().len(), 8);
    for field in ["caps", "approved", "worker_state"] {
        assert!(projected.get(field).is_none());
    }
    assert_eq!(projected["owner_uid"], 1000);
    assert_eq!(projected["enabled"], false);
}
