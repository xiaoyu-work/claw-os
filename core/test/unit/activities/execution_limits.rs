use super::*;
use serde_json::json;

fn limits() -> ExecutionLimitsDraft {
    ExecutionLimitsDraft {
        max_attempts: 3,
        max_turns_per_attempt: 10,
        expires_at: "2099-01-01T01:00:00+01:00".into(),
    }
}

#[test]
fn execution_limits_require_all_fields_and_reject_authority_or_accounting_overrides() {
    let value = serde_json::to_value(limits()).unwrap();
    for field in ["max_attempts", "max_turns_per_attempt", "expires_at"] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<ExecutionLimitsDraft>(missing).is_err(),
            "{field}"
        );
        let mut null = value.clone();
        null[field] = serde_json::Value::Null;
        assert!(
            serde_json::from_value::<ExecutionLimitsDraft>(null).is_err(),
            "{field}"
        );
    }
    for field in [
        "activity_id",
        "owner_uid",
        "enabled",
        "revision",
        "used_attempts",
        "created_at",
        "capabilities",
        "grant",
        "approvals",
        "token_budget",
        "cost",
        "reset",
    ] {
        let mut forged = value.clone();
        forged[field] = json!("forged");
        assert!(
            serde_json::from_value::<ExecutionLimitsDraft>(forged).is_err(),
            "{field}"
        );
    }
    for number in [json!(-1), json!(4294967296_u64), json!(1.5), json!("10")] {
        let mut invalid = value.clone();
        invalid["max_attempts"] = number;
        assert!(serde_json::from_value::<ExecutionLimitsDraft>(invalid).is_err());
    }
}

#[test]
fn execution_limits_enforce_exact_attempt_and_turn_thresholds() {
    for attempts in [1, 1000] {
        for turns in [1, 100] {
            let draft = ExecutionLimitsDraft {
                max_attempts: attempts,
                max_turns_per_attempt: turns,
                ..limits()
            };
            draft.validate().unwrap();
        }
    }
    for attempts in [0, 1001, u32::MAX] {
        assert!(matches!(
            ExecutionLimitsDraft {
                max_attempts: attempts,
                ..limits()
            }
            .validate(),
            Err(ActivityError::Invalid(_))
        ));
    }
    for turns in [0, 101, u32::MAX] {
        assert!(matches!(
            ExecutionLimitsDraft {
                max_turns_per_attempt: turns,
                ..limits()
            }
            .validate(),
            Err(ActivityError::Invalid(_))
        ));
    }
}

#[test]
fn expiry_normalizes_to_nanosecond_utc_without_rejecting_readable_expired_policies() {
    let mut draft = limits();
    draft.expires_at = "2026-01-01T01:00:00.123456789+01:00".into();
    let canonical = draft.canonicalized().unwrap();
    assert_eq!(canonical.expires_at, "2026-01-01T00:00:00.123456789Z");
    let mut equivalent = canonical.clone();
    equivalent.expires_at = "2025-12-31T16:00:00.123456789-08:00".into();
    assert_eq!(equivalent.canonicalized().unwrap(), canonical);
    let expired = ExecutionLimitsDraft {
        expires_at: "2000-01-01T00:00:00Z".into(),
        ..limits()
    };
    expired.validate().unwrap();
    for expiry in [
        "",
        "tomorrow",
        "2026-01-01",
        "2026-01-01T00:00:00",
        "2026-02-30T00:00:00Z",
        "2026-01-01T00:00:00Z\0",
    ] {
        assert!(
            ExecutionLimitsDraft {
                expires_at: expiry.into(),
                ..limits()
            }
            .validate()
            .is_err(),
            "{expiry}"
        );
    }
}

#[test]
fn job_tokens_match_the_existing_inert_job_store_grammar_without_paths() {
    for id in ["job-20260911_1", "A0", "-", "_", &"a".repeat(128)] {
        validate_job_id(id).unwrap();
    }
    for id in [
        "",
        ".",
        "..",
        "../job",
        "job/child",
        "job\\child",
        "job.name",
        "job name",
        "job:1",
        "job\n",
        "job\0",
        "\u{e9}",
        &"a".repeat(129),
    ] {
        assert!(
            matches!(validate_job_id(id), Err(ActivityError::Invalid(_))),
            "{id:?}"
        );
    }
}

#[test]
fn requested_turns_are_positive_and_revisions_fit_sqlite_without_wrapping() {
    for request in [None, Some(1), Some(100), Some(101), Some(u32::MAX)] {
        validate_requested_turns(request).unwrap();
    }
    assert!(validate_requested_turns(Some(0)).is_err());
    assert_eq!(revision_sql(1).unwrap(), 1);
    assert_eq!(
        revision_sql(u64::try_from(i64::MAX).unwrap()).unwrap(),
        i64::MAX
    );
    for revision in [0, u64::try_from(i64::MAX).unwrap() + 1, u64::MAX] {
        assert!(matches!(
            revision_sql(revision),
            Err(ActivityError::Invalid(_))
        ));
    }
}

#[test]
fn blocked_reasons_are_typed_and_have_stable_wire_spellings() {
    for (reason, wire) in [
        (ExecutionBlockedReason::Inactive, "inactive"),
        (ExecutionBlockedReason::Disabled, "disabled"),
        (ExecutionBlockedReason::Expired, "expired"),
        (ExecutionBlockedReason::StaleRevision, "stale_revision"),
        (ExecutionBlockedReason::AttemptLimit, "attempt_limit"),
    ] {
        assert_eq!(serde_json::to_value(reason).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ExecutionBlockedReason>(json!(wire)).unwrap(),
            reason
        );
        let error = ActivityError::ExecutionBlocked(reason);
        assert!(matches!(&error, ActivityError::ExecutionBlocked(actual) if *actual == reason));
        assert!(!error.to_string().is_empty());
    }
    assert!(serde_json::from_value::<ExecutionBlockedReason>(json!("authorized")).is_err());
}

#[test]
fn policy_and_reservation_keep_the_exact_frozen_output_shapes_without_authority_fields() {
    let activity_id = uuid::Uuid::new_v4().to_string();
    let policy = ActivityExecutionLimits {
        activity_id: activity_id.clone(),
        owner_uid: 7,
        revision: 1,
        enabled: true,
        limits: limits().canonicalized().unwrap(),
        used_attempts: 0,
        created_at: "2026-01-01T00:00:00.000000000Z".into(),
        updated_at: "2026-01-01T00:00:00.000000000Z".into(),
    };
    let value = serde_json::to_value(&policy).unwrap();
    assert_eq!(
        value,
        json!({
            "activity_id": activity_id, "owner_uid": 7, "revision": 1, "enabled": true,
            "limits": {"max_attempts":3,"max_turns_per_attempt":10,"expires_at":"2099-01-01T00:00:00.000000000Z"},
            "used_attempts": 0, "created_at":policy.created_at, "updated_at":policy.updated_at,
        })
    );
    assert_eq!(
        serde_json::from_value::<ActivityExecutionLimits>(value.clone()).unwrap(),
        policy
    );
    let reservation = ExecutionReservation {
        id: uuid::Uuid::new_v4().to_string(),
        activity_id,
        owner_uid: 7,
        job_id: "job-1".into(),
        policy_revision: 1,
        max_turns: 10,
        expires_at: policy.limits.expires_at.clone(),
        reserved_at: policy.created_at.clone(),
    };
    let reservation_value = serde_json::to_value(&reservation).unwrap();
    assert_eq!(
        reservation_value,
        json!({
            "id": reservation.id, "activity_id": reservation.activity_id, "owner_uid": 7, "job_id": "job-1",
            "policy_revision": 1, "max_turns": 10, "expires_at": reservation.expires_at,
            "reserved_at": reservation.reserved_at,
        })
    );
    assert_eq!(
        serde_json::from_value::<ExecutionReservation>(reservation_value.clone()).unwrap(),
        reservation
    );
    for field in [
        "grant",
        "confirmed",
        "capabilities",
        "requested_max_turns",
        "policy_max_turns",
    ] {
        let mut forged = reservation_value.clone();
        forged[field] = json!("forged");
        assert!(serde_json::from_value::<ExecutionReservation>(forged).is_err());
        let mut forged = value.clone();
        forged[field] = json!("forged");
        assert!(serde_json::from_value::<ActivityExecutionLimits>(forged).is_err());
    }
}
