use super::*;
use serde_json::{Value, json};

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OTHER: &str = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";

fn draft() -> MonetaryBudgetDraft {
    MonetaryBudgetDraft {
        currency: MonetaryCurrency::Usd,
        max_total_microusd: 5_000_000,
        input_microusd_per_million_tokens: 250_000,
        output_microusd_per_million_tokens: 1_000_000,
        max_output_tokens_per_turn: 4096,
    }
}

fn policy() -> ActivityMonetaryBudget {
    ActivityMonetaryBudget {
        activity_id: ACTIVITY.into(),
        owner_uid: 1000,
        revision: 5,
        enabled: false,
        spent_microusd: 2_000_000,
        reserved_microusd: 1_000_000,
        budget: draft(),
        created_at: "2026-09-13T00:00:00Z".into(),
        updated_at: "2026-09-13T01:00:00Z".into(),
    }
}

#[test]
fn monetary_budget_contract_is_closed_and_preserves_null_and_u64_values() {
    let request = ActivityMonetaryBudgetSetRequest {
        expected_revision: None,
        budget: draft(),
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({"expected_revision": null, "budget": {
            "currency": "USD", "max_total_microusd": 5000000,
            "input_microusd_per_million_tokens": 250000,
            "output_microusd_per_million_tokens": 1000000,
            "max_output_tokens_per_turn": 4096,
        }})
    );
    assert!(
        serde_json::from_value::<ActivityMonetaryBudgetSetRequest>(
            json!({"budget": draft()})
        )
        .is_err()
    );
    let response = ActivityMonetaryBudgetResponse {
        schema: 1,
        activity_id: ACTIVITY.into(),
        monetary_budget: None,
    };
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"schema": 1, "activity_id": ACTIVITY, "monetary_budget": null})
    );
    assert!(
        serde_json::from_value::<ActivityMonetaryBudgetResponse>(
            json!({"schema": 1, "activity_id": ACTIVITY})
        )
        .is_err()
    );
    let mut large = policy();
    large.revision = u64::MAX - 1;
    large.spent_microusd = u64::MAX;
    let encoded = serde_json::to_value(&large).unwrap();
    assert_eq!(encoded["revision"].as_u64(), Some(u64::MAX - 1));
    assert_eq!(encoded["spent_microusd"].as_u64(), Some(u64::MAX));
}

#[test]
fn monetary_budget_requests_reject_owner_ledger_and_authority_fields() {
    for field in [
        "id",
        "activity_id",
        "owner_uid",
        "revision",
        "enabled",
        "spent_microusd",
        "reserved_microusd",
        "provider",
        "model",
        "approved",
    ] {
        let mut value = json!({"expected_revision": null, "budget": draft()});
        value[field] = json!(0);
        assert!(
            serde_json::from_value::<ActivityMonetaryBudgetSetRequest>(value).is_err(),
            "{field}"
        );
    }
    for field in ["owner_uid", "spent_microusd", "budget", "provider"] {
        let mut value = json!({"expected_revision": 5, "enabled": false});
        value[field] = json!(0);
        assert!(
            serde_json::from_value::<ActivityMonetaryBudgetEnabledRequest>(value).is_err(),
            "{field}"
        );
    }
    assert!(
        serde_json::from_value::<ActivityMonetaryBudgetQuery>(json!({"owner_uid": 0})).is_err()
    );
}

#[test]
fn monetary_budget_bounds_currency_identity_and_timestamps_are_strict() {
    for (total, valid) in [
        (0, false),
        (1, true),
        (MAX_TOTAL_MICROUSD, true),
        (MAX_TOTAL_MICROUSD + 1, false),
    ] {
        let mut value = draft();
        value.max_total_microusd = total;
        assert_eq!(value.validate_shape().is_ok(), valid);
    }
    for (tokens, valid) in [
        (0, false),
        (1, true),
        (MAX_OUTPUT_TOKENS_PER_TURN, true),
        (MAX_OUTPUT_TOKENS_PER_TURN + 1, false),
    ] {
        let mut value = draft();
        value.max_output_tokens_per_turn = tokens;
        assert_eq!(value.validate_shape().is_ok(), valid);
    }
    assert!(
        serde_json::from_value::<MonetaryBudgetDraft>(json!({
            "currency": "EUR", "max_total_microusd": 1,
            "input_microusd_per_million_tokens": 1,
            "output_microusd_per_million_tokens": 1,
            "max_output_tokens_per_turn": 1,
        }))
        .is_err()
    );
    let current = policy();
    assert!(current.matches_owner(&ACTIVITY.to_uppercase(), 1000));
    assert!(!current.matches_owner(OTHER, 1000));
    assert!(!current.matches_owner(ACTIVITY, 1001));
    for (field, replacement) in [
        ("revision", json!(0)),
        ("created_at", json!("invalid")),
        ("updated_at", Value::Null),
    ] {
        let mut value = serde_json::to_value(&current).unwrap();
        value[field] = replacement;
        match serde_json::from_value::<ActivityMonetaryBudget>(value) {
            Ok(value) => assert!(!value.matches_activity(ACTIVITY)),
            Err(_) => {}
        }
    }
}

#[test]
fn monetary_budget_acknowledgements_use_exact_cas_and_preserve_policy_identity() {
    let previous = policy();
    let request = ActivityMonetaryBudgetSetRequest {
        expected_revision: Some(previous.revision),
        budget: MonetaryBudgetDraft {
            max_total_microusd: 8_000_000,
            ..draft()
        },
    };
    let mut saved = previous.clone();
    saved.revision += 1;
    saved.budget = request.budget.clone();
    assert!(saved.matches_set(ACTIVITY, &request));
    assert!(saved.preserves_identity(&previous));
    saved.revision += 1;
    assert!(!saved.matches_set(ACTIVITY, &request));

    let toggle = ActivityMonetaryBudgetEnabledRequest {
        expected_revision: previous.revision,
        enabled: true,
    };
    let mut enabled = previous.clone();
    enabled.revision += 1;
    enabled.enabled = true;
    enabled.spent_microusd = 3_000_000;
    enabled.reserved_microusd = 500_000;
    assert!(enabled.matches_enabled(ACTIVITY, &toggle));
    assert!(enabled.preserves_identity(&previous));
    assert_eq!(enabled.remaining_microusd(), 1_500_000);
    enabled.created_at = "2026-09-13T00:00:01Z".into();
    assert!(!enabled.preserves_identity(&previous));
}
