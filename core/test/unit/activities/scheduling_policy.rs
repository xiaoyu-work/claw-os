use super::*;

#[test]
fn scheduling_priorities_are_closed_and_ordered_for_admission_only() {
    for (priority, encoded, rank) in [
        (ActivitySchedulingPriority::Foreground, "foreground", 0),
        (ActivitySchedulingPriority::Standard, "standard", 1),
        (ActivitySchedulingPriority::Background, "background", 2),
    ] {
        assert_eq!(serde_json::to_value(priority).unwrap(), encoded);
        assert_eq!(
            serde_json::from_value::<ActivitySchedulingPriority>(encoded.into()).unwrap(),
            priority
        );
        assert_eq!(priority.admission_rank(), rank);
    }
    assert!(
        serde_json::from_value::<ActivitySchedulingPriority>("urgent".into()).is_err()
    );
}

#[test]
fn policy_metadata_contains_no_authority_or_execution_claims() {
    let policy = ActivitySchedulingPolicy {
        activity_id: "00000000-0000-4000-8000-000000000001".into(),
        owner_uid: 7,
        revision: 1,
        priority: ActivitySchedulingPriority::Foreground,
        created_at: "2026-09-14T00:00:00Z".into(),
        updated_at: "2026-09-14T00:00:00Z".into(),
    };
    let value = serde_json::to_value(policy).unwrap();
    for forbidden in [
        "capabilities",
        "consent",
        "approved",
        "completed",
        "executed",
        "preempt",
        "cancel",
        "budget",
    ] {
        assert!(value.get(forbidden).is_none(), "{forbidden}");
    }
}
