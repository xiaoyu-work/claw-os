use super::*;
use serde_json::{Value, json};
use std::time::{Duration, UNIX_EPOCH};

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OTHER: &str = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";

fn draft() -> ExecutionLimitsDraft {
    ExecutionLimitsDraft {
        max_attempts: 10,
        max_turns_per_attempt: 20,
        expires_at: "2099-01-01T12:00:00Z".into(),
    }
}

fn policy() -> ActivityExecutionLimits {
    ActivityExecutionLimits {
        activity_id: ACTIVITY.into(),
        owner_uid: 1000,
        revision: 5,
        enabled: false,
        limits: draft(),
        used_attempts: 8,
        created_at: "2026-09-11T12:00:00Z".into(),
        updated_at: "2026-09-11T13:00:00Z".into(),
    }
}

#[test]
fn activity_execution_limits_exact_shapes_preserve_explicit_null_and_u64_cas() {
    let request = ActivityExecutionLimitsSetRequest {
        expected_revision: None,
        limits: draft(),
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({
            "expected_revision": null,
            "limits": {"max_attempts": 10, "max_turns_per_attempt": 20, "expires_at": "2099-01-01T12:00:00Z"}
        })
    );
    let response = ActivityExecutionLimitsResponse {
        schema: 1,
        activity_id: ACTIVITY.into(),
        execution_limits: None,
    };
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({
            "schema": 1, "activity_id": ACTIVITY, "execution_limits": null
        })
    );
    assert!(response.matches_owner(ACTIVITY, 1000));
    assert!(
        serde_json::from_value::<ActivityExecutionLimitsResponse>(json!({
            "schema": 1, "activity_id": ACTIVITY
        }))
        .is_err(),
        "missing data must not become unconfigured success"
    );
    assert!(
        serde_json::from_value::<ActivityExecutionLimitsSetRequest>(json!({
            "limits": draft()
        }))
        .is_err(),
        "creation requires explicit null revision"
    );
    let request = ActivityExecutionLimitsSetRequest {
        expected_revision: Some(u64::MAX - 1),
        limits: draft(),
    };
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(encoded["expected_revision"].as_u64(), Some(u64::MAX - 1));
    assert_eq!(
        serde_json::from_value::<ActivityExecutionLimitsSetRequest>(encoded).unwrap(),
        request
    );
    let current = policy();
    let encoded = serde_json::to_value(&current).unwrap();
    assert_eq!(encoded.as_object().unwrap().len(), 8);
    assert_eq!(
        serde_json::from_value::<ActivityExecutionLimits>(encoded).unwrap(),
        current
    );
}

#[test]
fn activity_execution_limits_requests_cannot_choose_owner_counters_or_result_state() {
    for field in [
        "id",
        "activity_id",
        "owner_uid",
        "revision",
        "used_attempts",
        "enabled",
        "status",
        "caps",
        "approved",
    ] {
        let mut value = json!({"expected_revision": null, "limits": draft()});
        value[field] = json!(0);
        assert!(
            serde_json::from_value::<ActivityExecutionLimitsSetRequest>(value).is_err(),
            "{field}"
        );
        let mut value = serde_json::to_value(draft()).unwrap();
        value[field] = json!(0);
        assert!(
            serde_json::from_value::<ExecutionLimitsDraft>(value).is_err(),
            "{field}"
        );
    }
    for field in [
        "id",
        "owner_uid",
        "revision",
        "used_attempts",
        "limits",
        "status",
        "caps",
        "approved",
    ] {
        let mut value = json!({"expected_revision": 5, "enabled": false});
        value[field] = json!(0);
        assert!(serde_json::from_value::<ActivityExecutionLimitsEnabledRequest>(value).is_err());
    }
    for field in [
        "owner_uid",
        "revision",
        "used_attempts",
        "enabled",
        "expected_revision",
    ] {
        let mut value = json!({});
        value[field] = json!(0);
        assert!(serde_json::from_value::<ActivityExecutionLimitsQuery>(value).is_err());
    }
    assert!(
        serde_json::from_value::<ActivityExecutionLimitsEnabledRequest>(json!({
            "expected_revision": 1, "enabled": "false"
        }))
        .is_err()
    );
}

#[test]
fn activity_execution_limits_bounds_and_revision_overflow_are_explicit() {
    for (attempts, valid) in [
        (0, false),
        (1, true),
        (1000, true),
        (1001, false),
        (u32::MAX, false),
    ] {
        let mut limits = draft();
        limits.max_attempts = attempts;
        assert_eq!(limits.validate_shape().is_ok(), valid);
    }
    for (turns, valid) in [(0, false), (1, true), (100, true), (101, false)] {
        let mut limits = draft();
        limits.max_turns_per_attempt = turns;
        assert_eq!(limits.validate_shape().is_ok(), valid);
    }
    for (revision, valid) in [
        (None, true),
        (Some(0), false),
        (Some(1), true),
        (Some(u64::MAX - 1), true),
        (Some(u64::MAX), false),
    ] {
        assert_eq!(
            ActivityExecutionLimitsSetRequest {
                expected_revision: revision,
                limits: draft()
            }
            .validate_shape()
            .is_ok(),
            valid,
        );
    }
    let mut limits = draft();
    limits.expires_at = "not RFC3339".into();
    assert!(limits.validate_shape().is_err());
    assert!(limits.is_expired_at(UNIX_EPOCH).is_err());
    limits.expires_at = "2000-01-01T00:00:00Z".into();
    assert!(
        limits.validate_shape().is_ok(),
        "historical expiry is readable; backend admits writes"
    );
}

#[test]
fn activity_execution_limits_identity_and_required_fields_are_checked() {
    let current = policy();
    assert!(current.matches_owner(&ACTIVITY.to_uppercase(), 1000));
    assert!(!current.matches_owner(OTHER, 1000));
    assert!(!current.matches_owner(ACTIVITY, 1001));
    for (field, replacement) in [
        ("activity_id", json!("")),
        ("revision", json!(0)),
        ("created_at", json!("not a timestamp")),
        ("updated_at", json!("")),
    ] {
        let mut value = serde_json::to_value(&current).unwrap();
        value[field] = replacement;
        let invalid: ActivityExecutionLimits = serde_json::from_value(value).unwrap();
        assert!(!invalid.matches_owner(ACTIVITY, 1000));
    }
    for field in [
        "owner_uid",
        "revision",
        "enabled",
        "limits",
        "used_attempts",
        "created_at",
        "updated_at",
    ] {
        let mut value = serde_json::to_value(&current).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<ActivityExecutionLimits>(value).is_err(),
            "{field}"
        );
    }
    for (field, invalid) in [
        ("owner_uid", json!(-1)),
        ("used_attempts", json!(4294967296_u64)),
        ("revision", json!("5")),
        ("enabled", Value::Null),
    ] {
        let mut value = serde_json::to_value(&current).unwrap();
        value[field] = invalid;
        assert!(serde_json::from_value::<ActivityExecutionLimits>(value).is_err());
    }
    for schema in [0, 2] {
        assert!(
            !ActivityExecutionLimitsResponse {
                schema,
                activity_id: ACTIVITY.into(),
                execution_limits: Some(current.clone()),
            }
            .matches_owner(ACTIVITY, 1000)
        );
    }
}

#[test]
fn activity_execution_limits_set_acknowledgements_allow_utc_normalization_but_require_exact_cas() {
    let request = ActivityExecutionLimitsSetRequest {
        expected_revision: None,
        limits: ExecutionLimitsDraft {
            expires_at: "2099-01-01T05:00:00.125-07:00".into(),
            ..draft()
        },
    };
    let mut created = policy();
    created.revision = 1;
    created.enabled = true;
    created.used_attempts = 0;
    created.limits.expires_at = "2099-01-01T12:00:00.125000000Z".into();
    assert!(created.matches_set(ACTIVITY, &request));
    created.enabled = false;
    assert!(!created.matches_set(ACTIVITY, &request));
    created.enabled = true;
    created.revision = 2;
    assert!(!created.matches_set(ACTIVITY, &request));

    let request = ActivityExecutionLimitsSetRequest {
        expected_revision: Some(5),
        limits: request.limits,
    };
    created.revision = 6;
    assert!(created.matches_set(ACTIVITY, &request));
    for mutation in 0..5 {
        let mut changed = created.clone();
        match mutation {
            0 => changed.activity_id = OTHER.into(),
            1 => changed.revision = 5,
            2 => changed.revision = 7,
            3 => changed.limits.max_attempts += 1,
            _ => changed.limits.expires_at = "2099-01-01T12:00:00.125000001Z".into(),
        }
        assert!(!changed.matches_set(ACTIVITY, &request));
    }
}

#[test]
fn activity_execution_limits_updates_and_toggles_preserve_lifetime_not_ceiling_relation() {
    let previous = policy();
    let mut updated = previous.clone();
    updated.revision += 1;
    updated.limits.max_attempts = 3;
    updated.used_attempts = 9;
    let request = ActivityExecutionLimitsSetRequest {
        expected_revision: Some(previous.revision),
        limits: updated.limits.clone(),
    };
    assert!(updated.matches_set(ACTIVITY, &request));
    assert!(updated.preserves_lifetime(&previous));
    assert_eq!(updated.remaining_attempts(), 0);
    assert!(!updated.enabled);
    let enabled_request = ActivityExecutionLimitsEnabledRequest {
        expected_revision: previous.revision,
        enabled: true,
    };
    updated.enabled = true;
    assert!(updated.matches_enabled(ACTIVITY, &enabled_request));
    updated.enabled = false;
    assert!(!updated.matches_enabled(ACTIVITY, &enabled_request));
    updated.used_attempts = 0;
    assert!(!updated.preserves_lifetime(&previous));
    updated.used_attempts = previous.used_attempts;
    updated.owner_uid = 1001;
    assert!(!updated.preserves_lifetime(&previous));
    updated.owner_uid = previous.owner_uid;
    updated.created_at = "2026-09-11T12:00:01Z".into();
    assert!(!updated.preserves_lifetime(&previous));
}

#[test]
fn activity_execution_limits_expiry_and_remaining_are_presentation_only() {
    let mut current = policy();
    current.limits.expires_at = "1970-01-01T01:00:10+01:00".into();
    assert!(
        !current
            .limits
            .is_expired_at(UNIX_EPOCH + Duration::from_secs(9))
            .unwrap()
    );
    assert!(
        current
            .limits
            .is_expired_at(UNIX_EPOCH + Duration::from_secs(10))
            .unwrap()
    );
    assert!(
        current
            .limits
            .is_expired_at(UNIX_EPOCH + Duration::from_secs(11))
            .unwrap()
    );
    current.used_attempts = 8;
    assert_eq!(current.remaining_attempts(), 2);
    current.used_attempts = 10;
    assert_eq!(current.remaining_attempts(), 0);
    current.used_attempts = u32::MAX;
    assert_eq!(current.remaining_attempts(), 0);
    assert!(
        !current.enabled,
        "status calculation cannot re-enable or reset the policy"
    );
}
