use super::*;

#[test]
fn integer_cost_rounds_up_exactly() {
    assert_eq!(accounted_cost(0, 1).unwrap(), 0);
    assert_eq!(accounted_cost(1, 1).unwrap(), 1);
    assert_eq!(accounted_cost(1_000_000, 1).unwrap(), 1);
    assert_eq!(accounted_cost(1_000_001, 1).unwrap(), 2);
    assert_eq!(accounted_cost(500_000, 3).unwrap(), 2);
}

#[test]
fn currency_and_finite_bounds_are_closed() {
    let valid = MonetaryBudgetDraft {
        currency: "USD".into(),
        max_total_microusd: 1,
        input_microusd_per_million_tokens: 1,
        output_microusd_per_million_tokens: 1,
        max_output_tokens_per_turn: 1,
    };
    valid.validate().unwrap();
    let mut invalid = valid.clone();
    invalid.currency = "EUR".into();
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.max_total_microusd = 0;
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.max_total_microusd = MAX_TOTAL_MICROUSD + 1;
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.input_microusd_per_million_tokens =
        MAX_RATE_MICROUSD_PER_MILLION_TOKENS + 1;
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.output_microusd_per_million_tokens = 0;
    assert!(invalid.validate().is_err());
    invalid = valid;
    invalid.max_output_tokens_per_turn = MAX_OUTPUT_TOKENS_PER_TURN + 1;
    assert!(invalid.validate().is_err());
}

#[test]
fn runtime_identity_is_closed_and_bounded() {
    let mut request = MonetaryReservationRequest {
        call_id: uuid::Uuid::new_v4().to_string(),
        job_id: "job".into(),
        session_id: Some("session".into()),
        turn_index: 0,
        input_upper_bound_tokens: 0,
        requested_max_output_tokens: 1,
    };
    validate_runtime_identity(&request).unwrap();
    request.call_id = "guessable".into();
    assert!(validate_runtime_identity(&request).is_err());
    request.call_id = uuid::Uuid::new_v4().to_string();
    request.session_id = Some("x".repeat(129));
    assert!(validate_runtime_identity(&request).is_err());
    request.session_id = None;
    request.requested_max_output_tokens = 0;
    assert!(validate_runtime_identity(&request).is_err());
}
