use super::*;
use serde_json::json;

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn policy() -> ActivitySchedulingPolicy {
    ActivitySchedulingPolicy {
        activity_id: ACTIVITY.into(),
        owner_uid: 1000,
        revision: 7,
        priority: ActivitySchedulingPriority::Foreground,
        created_at: "2026-09-13T00:00:00Z".into(),
        updated_at: "2026-09-13T01:00:00Z".into(),
    }
}

#[test]
fn scheduling_priority_contract_is_closed_nullable_and_owner_free_on_write() {
    let request = ActivitySchedulingPrioritySetRequest {
        expected_revision: None,
        priority: ActivitySchedulingPriority::Background,
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({"expected_revision": null, "priority": "background"})
    );
    for field in [
        "id",
        "activity_id",
        "owner_uid",
        "job_id",
        "authority",
        "preempt",
        "cancel",
    ] {
        let mut value = json!({"expected_revision": null, "priority": "standard"});
        value[field] = json!(true);
        assert!(
            serde_json::from_value::<ActivitySchedulingPrioritySetRequest>(value).is_err(),
            "{field}"
        );
    }
    assert!(
        serde_json::from_value::<ActivitySchedulingPrioritySetRequest>(
            json!({"priority": "standard"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivitySchedulingPriorityQuery>(json!({"owner_uid": 1000}))
            .is_err()
    );
    let response = ActivitySchedulingPriorityResponse {
        schema: 1,
        activity_id: ACTIVITY.into(),
        scheduling_policy: None,
    };
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        json!({"schema": 1, "activity_id": ACTIVITY, "scheduling_policy": null})
    );
}

#[test]
fn scheduling_priority_acknowledgements_require_exact_cas_identity_and_timestamps() {
    let previous = policy();
    assert!(previous.matches_owner(&ACTIVITY.to_uppercase(), 1000));
    let request = ActivitySchedulingPrioritySetRequest {
        expected_revision: Some(previous.revision),
        priority: ActivitySchedulingPriority::Background,
    };
    let mut saved = previous.clone();
    saved.revision += 1;
    saved.priority = request.priority;
    assert!(saved.matches_set(ACTIVITY, &request));
    assert!(saved.preserves_identity(&previous));
    saved.revision += 1;
    assert!(!saved.matches_set(ACTIVITY, &request));
    saved.revision = previous.revision + 1;
    saved.created_at = "2026-09-13T00:00:01Z".into();
    assert!(!saved.preserves_identity(&previous));

    for value in [
        json!({"expected_revision": 0, "priority": "standard"}),
        json!({"expected_revision": u64::MAX, "priority": "standard"}),
        json!({"expected_revision": 1, "priority": "urgent"}),
    ] {
        if let Ok(value) = serde_json::from_value::<ActivitySchedulingPrioritySetRequest>(value) {
            assert!(value.validate_shape().is_err());
        }
    }
    let mut invalid = policy();
    invalid.updated_at = "invalid".into();
    assert!(!invalid.matches_activity(ACTIVITY));
}
