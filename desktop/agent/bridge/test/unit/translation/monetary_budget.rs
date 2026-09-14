use super::*;
use serde_json::json;

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn value() -> Value {
    json!({
        "activity_id": ACTIVITY, "owner_uid": 1000, "revision": 7, "enabled": false,
        "spent_microusd": 2000000, "reserved_microusd": 1000000,
        "budget": {
            "currency": "USD", "max_total_microusd": 5000000,
            "input_microusd_per_million_tokens": 250000,
            "output_microusd_per_million_tokens": 1000000,
            "max_output_tokens_per_turn": 4096
        },
        "created_at": "2026-09-13T00:00:00Z",
        "updated_at": "2026-09-13T01:00:00Z"
    })
}

#[test]
fn monetary_budget_translation_preserves_absence_and_configured_accounting() {
    let absent = get(
        json!({"schema": 1, "activity_id": ACTIVITY, "monetary_budget": null}),
        ACTIVITY,
        1000,
    )
    .unwrap();
    assert!(absent.monetary_budget.is_none());
    let response = get(
        json!({"schema": 1, "activity_id": ACTIVITY, "monetary_budget": value()}),
        ACTIVITY,
        1000,
    )
    .unwrap();
    let budget = response.monetary_budget.unwrap();
    assert_eq!(budget.spent_microusd, 2_000_000);
    assert_eq!(budget.reserved_microusd, 1_000_000);
    assert_eq!(budget.remaining_microusd(), 2_000_000);
    assert_eq!(budget.budget.currency, cos_agent_protocol::MonetaryCurrency::Usd);
}

#[test]
fn monetary_budget_translation_rejects_wrong_owner_shape_currency_and_authority_fields() {
    assert!(policy(value(), ACTIVITY, 1001).is_err());
    for envelope in [
        json!({"schema": 2, "activity_id": ACTIVITY, "monetary_budget": value()}),
        json!({"schema": 1, "activity_id": ACTIVITY}),
        json!({"schema": 1, "activity_id": ACTIVITY, "monetary_budget": false}),
    ] {
        assert!(get(envelope, ACTIVITY, 1000).is_err());
    }
    for (path, replacement) in [
        ("owner_uid", json!(1001)),
        ("revision", json!(0)),
        ("spent_microusd", json!(-1)),
        ("created_at", json!("invalid")),
    ] {
        let mut invalid = value();
        invalid[path] = replacement;
        assert!(policy(invalid, ACTIVITY, 1000).is_err());
    }
    let mut invalid = value();
    invalid["budget"]["currency"] = json!("EUR");
    assert!(policy(invalid, ACTIVITY, 1000).is_err());
    let mut authority = value();
    authority["provider_price"] = json!(true);
    assert!(policy(authority, ACTIVITY, 1000).is_err());
}
