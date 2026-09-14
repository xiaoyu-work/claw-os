use super::*;
use crate::activities::{
    ActivityDraft, ActivityService, MonetaryBudgetDraft, MonetaryReservationRequest,
    MonetarySettlement,
};

fn activity(service: &SqliteActivityService, owner: u32) -> crate::activities::Activity {
    service
        .create(
            owner,
            ActivityDraft {
                title: "budget".into(),
                goal: "test".into(),
                completion_criteria: String::new(),
                boundaries: String::new(),
                resources: vec![],
            },
        )
        .unwrap()
}

fn draft(total: u64) -> MonetaryBudgetDraft {
    MonetaryBudgetDraft {
        currency: "USD".into(),
        max_total_microusd: total,
        input_microusd_per_million_tokens: 1_000_000,
        output_microusd_per_million_tokens: 2_000_000,
        max_output_tokens_per_turn: 10,
    }
}

fn settle(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    reservation: &crate::activities::MonetaryReservation,
    settlement: MonetarySettlement,
) -> crate::activities::ActivityMonetaryBudget {
    service
        .settle_monetary(
            owner_uid,
            activity_id,
            &reservation.call_id,
            &reservation.job_id,
            reservation.session_id.as_deref(),
            reservation.turn_index,
            settlement,
        )
        .unwrap()
}

#[test]
fn reserve_settle_cas_and_idempotency_preserve_accounting() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 7);
    let policy = service
        .set_monetary_budget(7, &activity.id, None, draft(100))
        .unwrap();
    assert_eq!(policy.revision, 1);
    assert!(service
        .set_monetary_budget(7, &activity.id, None, draft(100))
        .is_err());
    let request = MonetaryReservationRequest {
        call_id: uuid::Uuid::new_v4().to_string(),
        job_id: "job-1".into(),
        session_id: Some("session-1".into()),
        turn_index: 1,
        input_upper_bound_tokens: 10,
        requested_max_output_tokens: 50,
    };
    let reservation = service
        .reserve_monetary(7, &activity.id, request.clone())
        .unwrap()
        .unwrap();
    assert_eq!(reservation.reserved_microusd, 30);
    assert_eq!(reservation.max_output_tokens, 10);
    assert_eq!(
        service
            .reserve_monetary(7, &activity.id, request)
            .unwrap()
            .unwrap(),
        reservation
    );
    let settlement = MonetarySettlement {
        input_tokens: Some(3),
        output_tokens: Some(4),
        cache_read_tokens: Some(9),
        cache_write_tokens: Some(8),
        provider: "test".into(),
        model: "model".into(),
        conservative: false,
    };
    let state = settle(&service, 7, &activity.id, &reservation, settlement.clone());
    assert_eq!(state.spent_microusd, 11);
    assert_eq!(state.reserved_microusd, 0);
    let persisted: (i64, i64, i64, i64, String, String, String) = service
        .lock()
        .unwrap()
        .query_row(
            "SELECT actual_input_tokens, actual_output_tokens, cache_read_tokens,
                cache_write_tokens, provider, model, settled_at
             FROM activity_monetary_ledger WHERE owner_uid=?1 AND call_id=?2",
            rusqlite::params![7_i64, reservation.call_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        (persisted.0, persisted.1, persisted.2, persisted.3),
        (3, 4, 9, 8)
    );
    assert_eq!(persisted.4, "test");
    assert_eq!(persisted.5, "model");
    assert!(chrono::DateTime::parse_from_rfc3339(&persisted.6).is_ok());
    let replay = settle(&service, 7, &activity.id, &reservation, settlement);
    assert_eq!(replay.spent_microusd, 11);
}

#[test]
fn exhausted_and_crash_reserved_balance_fail_closed() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 9);
    service
        .set_monetary_budget(9, &activity.id, None, draft(30))
        .unwrap();
    let first = MonetaryReservationRequest {
        call_id: uuid::Uuid::new_v4().to_string(),
        job_id: "job".into(),
        session_id: None,
        turn_index: 0,
        input_upper_bound_tokens: 10,
        requested_max_output_tokens: 10,
    };
    service
        .reserve_monetary(9, &activity.id, first)
        .unwrap()
        .unwrap();
    let second = MonetaryReservationRequest {
        call_id: uuid::Uuid::new_v4().to_string(),
        job_id: "job".into(),
        session_id: None,
        turn_index: 1,
        input_upper_bound_tokens: 1,
        requested_max_output_tokens: 1,
    };
    assert!(matches!(
        service.reserve_monetary(9, &activity.id, second),
        Err(ActivityError::MonetaryBlocked(
            MonetaryBlockedReason::Exhausted
        ))
    ));
    assert_eq!(
        service
            .monetary_budget(9, &activity.id)
            .unwrap()
            .unwrap()
            .reserved_microusd,
        30
    );
}

#[test]
fn absence_preserves_legacy_behavior_and_owner_scope() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 41);
    assert!(service.monetary_budget(41, &activity.id).unwrap().is_none());
    let request = MonetaryReservationRequest {
        call_id: uuid::Uuid::new_v4().to_string(),
        job_id: "legacy-job".into(),
        session_id: None,
        turn_index: 0,
        input_upper_bound_tokens: 100,
        requested_max_output_tokens: 20,
    };
    assert!(service
        .reserve_monetary(41, &activity.id, request)
        .unwrap()
        .is_none());
    assert!(matches!(
        service.monetary_budget(42, &activity.id),
        Err(ActivityError::NotFound)
    ));
}

#[test]
fn lifecycle_and_cas_controls_fail_closed_without_erasing_state() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 43);
    let created = service
        .set_monetary_budget(43, &activity.id, None, draft(100))
        .unwrap();
    assert!(matches!(
        service.set_monetary_budget_enabled(43, &activity.id, 99, false),
        Err(ActivityError::MonetaryBlocked(MonetaryBlockedReason::Stale))
    ));
    let disabled = service
        .set_monetary_budget_enabled(43, &activity.id, created.revision, false)
        .unwrap();
    assert!(matches!(
        service.reserve_monetary(
            43,
            &activity.id,
            MonetaryReservationRequest {
                call_id: uuid::Uuid::new_v4().to_string(),
                job_id: "disabled-job".into(),
                session_id: None,
                turn_index: 0,
                input_upper_bound_tokens: 0,
                requested_max_output_tokens: 1,
            }
        ),
        Err(ActivityError::MonetaryBlocked(
            MonetaryBlockedReason::Disabled
        ))
    ));
    service
        .transition(
            43,
            &activity.id,
            ActivityState::Completed,
            Some("done".into()),
        )
        .unwrap();
    assert!(
        !service
            .monetary_budget(43, &activity.id)
            .unwrap()
            .unwrap()
            .enabled
    );
    assert!(matches!(
        service.set_monetary_budget_enabled(43, &activity.id, disabled.revision, true),
        Err(ActivityError::MonetaryBlocked(
            MonetaryBlockedReason::Inactive
        ))
    ));
    assert!(matches!(
        service.set_monetary_budget(43, &activity.id, Some(disabled.revision), draft(200)),
        Err(ActivityError::MonetaryBlocked(
            MonetaryBlockedReason::Inactive
        ))
    ));
    let still_disabled = service
        .set_monetary_budget_enabled(43, &activity.id, disabled.revision, false)
        .unwrap();
    assert!(!still_disabled.enabled);
    assert_eq!(still_disabled.spent_microusd, 0);
    assert_eq!(still_disabled.reserved_microusd, 0);
}

#[test]
fn updates_preserve_ledgers_enabled_state_and_reservation_replay() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 44);
    let created = service
        .set_monetary_budget(44, &activity.id, None, draft(200))
        .unwrap();
    let request = MonetaryReservationRequest {
        call_id: uuid::Uuid::new_v4().to_string(),
        job_id: "job-update".into(),
        session_id: Some("session-update".into()),
        turn_index: 2,
        input_upper_bound_tokens: 10,
        requested_max_output_tokens: 5,
    };
    let reservation = service
        .reserve_monetary(44, &activity.id, request.clone())
        .unwrap()
        .unwrap();
    assert_eq!(reservation.policy_max_output_tokens_per_turn, 10);
    assert_eq!(reservation.max_output_tokens, 5);
    let disabled = service
        .set_monetary_budget_enabled(44, &activity.id, created.revision, false)
        .unwrap();
    let revised = service
        .set_monetary_budget(44, &activity.id, Some(disabled.revision), draft(300))
        .unwrap();
    assert!(!revised.enabled);
    assert_eq!(revised.reserved_microusd, reservation.reserved_microusd);
    assert_eq!(
        service
            .reserve_monetary(44, &activity.id, request)
            .unwrap()
            .unwrap(),
        reservation
    );
}

#[test]
fn reservation_and_settlement_replays_bind_exact_identity() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let first_activity = activity(&service, 45);
    let second_activity = activity(&service, 45);
    service
        .set_monetary_budget(45, &first_activity.id, None, draft(100))
        .unwrap();
    service
        .set_monetary_budget(45, &second_activity.id, None, draft(100))
        .unwrap();
    let request = MonetaryReservationRequest {
        call_id: uuid::Uuid::new_v4().to_string(),
        job_id: "job-exact".into(),
        session_id: Some("session-exact".into()),
        turn_index: 3,
        input_upper_bound_tokens: 2,
        requested_max_output_tokens: 2,
    };
    let reservation = service
        .reserve_monetary(45, &first_activity.id, request.clone())
        .unwrap()
        .unwrap();
    let mut mismatch = request;
    mismatch.turn_index += 1;
    assert!(matches!(
        service.reserve_monetary(45, &first_activity.id, mismatch),
        Err(ActivityError::Conflict(_))
    ));
    let settlement = MonetarySettlement {
        input_tokens: Some(1),
        output_tokens: Some(1),
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(0),
        provider: "provider".into(),
        model: "model".into(),
        conservative: false,
    };
    assert!(matches!(
        service.settle_monetary(
            45,
            &second_activity.id,
            &reservation.call_id,
            &reservation.job_id,
            reservation.session_id.as_deref(),
            reservation.turn_index,
            settlement.clone()
        ),
        Err(ActivityError::Conflict(_))
    ));
    assert!(matches!(
        service.settle_monetary(
            45,
            &first_activity.id,
            &reservation.call_id,
            "another-job",
            reservation.session_id.as_deref(),
            reservation.turn_index,
            settlement.clone(),
        ),
        Err(ActivityError::Conflict(_))
    ));
    settle(
        &service,
        45,
        &first_activity.id,
        &reservation,
        settlement.clone(),
    );
    let mut conflicting = settlement;
    conflicting.output_tokens = Some(2);
    assert!(matches!(
        service.settle_monetary(
            45,
            &first_activity.id,
            &reservation.call_id,
            &reservation.job_id,
            reservation.session_id.as_deref(),
            reservation.turn_index,
            conflicting
        ),
        Err(ActivityError::Conflict(_))
    ));
}

#[test]
fn actual_overage_is_recorded_and_conservative_failure_charges_full_reserve() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let overage_activity = activity(&service, 46);
    let tiny = MonetaryBudgetDraft {
        currency: "USD".into(),
        max_total_microusd: 1,
        input_microusd_per_million_tokens: 1,
        output_microusd_per_million_tokens: 1,
        max_output_tokens_per_turn: 1,
    };
    service
        .set_monetary_budget(46, &overage_activity.id, None, tiny)
        .unwrap();
    let reservation = service
        .reserve_monetary(
            46,
            &overage_activity.id,
            MonetaryReservationRequest {
                call_id: uuid::Uuid::new_v4().to_string(),
                job_id: "job-overage".into(),
                session_id: None,
                turn_index: 0,
                input_upper_bound_tokens: 0,
                requested_max_output_tokens: 1,
            },
        )
        .unwrap()
        .unwrap();
    let overage = settle(
        &service,
        46,
        &overage_activity.id,
        &reservation,
        MonetarySettlement {
            input_tokens: Some(1_000_001),
            output_tokens: Some(1_000_001),
            cache_read_tokens: Some(99),
            cache_write_tokens: Some(88),
            provider: "provider".into(),
            model: "model".into(),
            conservative: false,
        },
    );
    assert_eq!(overage.spent_microusd, 4);
    assert_eq!(overage.reserved_microusd, 0);
    assert!(matches!(
        service.reserve_monetary(
            46,
            &overage_activity.id,
            MonetaryReservationRequest {
                call_id: uuid::Uuid::new_v4().to_string(),
                job_id: "job-overage".into(),
                session_id: None,
                turn_index: 1,
                input_upper_bound_tokens: 0,
                requested_max_output_tokens: 1,
            }
        ),
        Err(ActivityError::MonetaryBlocked(
            MonetaryBlockedReason::Exhausted
        ))
    ));

    let conservative_activity = activity(&service, 46);
    service
        .set_monetary_budget(46, &conservative_activity.id, None, draft(100))
        .unwrap();
    let reservation = service
        .reserve_monetary(
            46,
            &conservative_activity.id,
            MonetaryReservationRequest {
                call_id: uuid::Uuid::new_v4().to_string(),
                job_id: "job-failed".into(),
                session_id: None,
                turn_index: 0,
                input_upper_bound_tokens: 10,
                requested_max_output_tokens: 10,
            },
        )
        .unwrap()
        .unwrap();
    let state = settle(
        &service,
        46,
        &conservative_activity.id,
        &reservation,
        MonetarySettlement {
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            provider: "provider".into(),
            model: "model".into(),
            conservative: true,
        },
    );
    assert_eq!(state.spent_microusd, reservation.reserved_microusd);
    assert_eq!(state.reserved_microusd, 0);
}

#[test]
fn corrupt_accounting_totals_are_rejected() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = activity(&service, 47);
    service
        .set_monetary_budget(47, &activity.id, None, draft(100))
        .unwrap();
    service
        .lock()
        .unwrap()
        .execute(
            "UPDATE activity_monetary_budgets SET reserved_microusd=1
             WHERE owner_uid=?1 AND activity_id=?2",
            rusqlite::params![47_i64, activity.id],
        )
        .unwrap();
    assert!(matches!(
        service.monetary_budget(47, &activity.id),
        Err(ActivityError::Corrupt(_))
    ));
}
