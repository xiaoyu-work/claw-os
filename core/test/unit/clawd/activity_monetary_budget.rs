use super::*;

#[test]
fn wire_body_rejects_unknown_fields_and_currency() {
    let invalid = serde_json::json!({
        "id":"00000000-0000-4000-8000-000000000001",
        "budget":{
            "currency":"EUR",
            "max_total_microusd":1,
            "input_microusd_per_million_tokens":1,
            "output_microusd_per_million_tokens":1,
            "max_output_tokens_per_turn":1
        },
        "extra":true
    });
    assert!(decode::<body::ActivityMonetaryBudgetSet>(invalid).is_err());
}

#[test]
fn routes_accept_no_caller_selected_owner_and_classify_audit_fields() {
    let id = "00000000-0000-4000-8000-000000000001";
    for (command, valid) in [
        (
            crate::clawd::routes::Command::ActivityMonetaryBudgetGet,
            serde_json::json!({"id":id}),
        ),
        (
            crate::clawd::routes::Command::ActivityMonetaryBudgetSet,
            serde_json::json!({
                "id":id,
                "budget":{
                    "currency":"USD",
                    "max_total_microusd":1,
                    "input_microusd_per_million_tokens":1,
                    "output_microusd_per_million_tokens":1,
                    "max_output_tokens_per_turn":1
                }
            }),
        ),
        (
            crate::clawd::routes::Command::ActivityMonetaryBudgetEnabled,
            serde_json::json!({"id":id,"expected_revision":1,"enabled":false}),
        ),
    ] {
        let route = command.route();
        assert_eq!(route.access, crate::clawd::routes::Access::User);
        (route.decode)(valid.clone()).unwrap();
        let mut forged = valid;
        forged["owner_uid"] = serde_json::json!(0);
        assert!((route.decode)(forged).is_err());
        assert!(route.audit_fields.iter().any(|(field, _)| *field == "id"));
        assert!(!route
            .audit_fields
            .iter()
            .any(|(field, _)| *field == "owner_uid"));
    }
}
