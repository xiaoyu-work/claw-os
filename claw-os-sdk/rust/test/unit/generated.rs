use crate::generated::{
    validate_ai, validate_budget_show, validate_tool, validate_tool_catalog, WIRE_ENUM,
    WIRE_MAXIMUM, WIRE_MINIMUM, WIRE_REQUIRED, WIRE_TYPE, WIRE_UNKNOWN_FIELD,
};

fn valid_ai() -> serde_json::Value {
    serde_json::json!({
        "text": "hello",
        "model": "m",
        "provider": "p",
        "verb": "ai.chat",
        "usage": {"input_tokens": 1, "output_tokens": 2, "units": 3},
        "budget": {"period": "2026-08", "units_used": 3, "units_cap": 100},
        "review": {"safety": "strict", "prompt_redacted": false},
        "tool_calls": [{"id": "c1", "name": "echo", "input": {"value": "ok"}}]
    })
}

fn ai_with_units(units: &str) -> serde_json::Value {
    serde_json::from_str(&format!(
        r#"{{
            "text":"hello","model":"m","provider":"p","verb":"ai.chat",
            "usage":{{"input_tokens":1,"output_tokens":2,"units":{units}}},
            "budget":{{"period":"2026-08","units_used":3,"units_cap":100}},
            "review":{{"safety":"strict","prompt_redacted":false}}
        }}"#
    ))
    .unwrap()
}

#[test]
fn ai_validator_enforces_the_shared_contract() {
    let mut cases = Vec::new();

    let mut missing = valid_ai();
    missing.as_object_mut().unwrap().remove("text");
    cases.push((missing, WIRE_REQUIRED, "$.text"));

    let mut wrong_type = valid_ai();
    wrong_type["usage"]["input_tokens"] = serde_json::json!("1");
    cases.push((wrong_type, WIRE_TYPE, "$.usage.input_tokens"));

    let mut below_minimum = valid_ai();
    below_minimum["usage"]["units"] = serde_json::json!(-1);
    cases.push((below_minimum, WIRE_MINIMUM, "$.usage.units"));

    let mut invalid_enum = valid_ai();
    invalid_enum["verb"] = serde_json::json!("ai.unknown");
    cases.push((invalid_enum, WIRE_ENUM, "$.verb"));

    let mut unknown_nested = valid_ai();
    unknown_nested["usage"]["extra"] = serde_json::json!(true);
    cases.push((unknown_nested, WIRE_UNKNOWN_FIELD, "$.usage.extra"));

    let mut malformed_call = valid_ai();
    malformed_call["tool_calls"][0].as_object_mut().unwrap().remove("name");
    cases.push((malformed_call, WIRE_REQUIRED, "$.tool_calls[0].name"));

    for (payload, code, path) in cases {
        let error = validate_ai(&payload).unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.path, path);
    }
}

#[test]
fn integer_validation_uses_json_schema_mathematical_semantics() {
    for units in [
        "1.0",
        "1e0",
        "1.5e1",
        "9007199254740992",
        "18446744073709551615",
    ] {
        validate_ai(&ai_with_units(units)).unwrap();
    }

    for units in ["1.5", "15e-1", "1e-400", "9007199254740990.5"] {
        let fractional = validate_ai(&ai_with_units(units)).unwrap_err();
        assert_eq!(fractional.code, WIRE_TYPE);
        assert_eq!(fractional.path, "$.usage.units");
    }

    let oversized = validate_ai(&ai_with_units("18446744073709551616")).unwrap_err();
    assert_eq!(oversized.code, WIRE_MAXIMUM);
    assert_eq!(oversized.path, "$.usage.units");

    let fractional_above_max =
        validate_ai(&ai_with_units("18446744073709551615.5")).unwrap_err();
    assert_eq!(fractional_above_max.code, WIRE_TYPE);
}

#[test]
fn validators_accept_v1_tool_inputs_and_reject_malformed_items() {
    for input in [
        serde_json::json!("scalar"),
        serde_json::json!([1, true]),
        serde_json::Value::Null,
    ] {
        let mut payload = valid_ai();
        payload["tool_calls"][0]["input"] = input;
        validate_ai(&payload).unwrap();
    }
    validate_tool(&serde_json::json!({
        "tool": "echo",
        "app_id": "app",
        "status": "ok",
        "result": null
    }))
    .unwrap();

    let error = validate_tool_catalog(&serde_json::json!({
        "tools": [{
            "name": "echo",
            "summary": "Echo",
            "verb": "ipc.invoke",
            "stability": "stable",
            "args_schema": {},
            "returns_schema": {}
        }, 7]
    }))
    .unwrap_err();
    assert_eq!(error.code, WIRE_TYPE);
    assert_eq!(error.path, "$.tools[1]");
}

#[test]
fn root_types_and_budget_show_have_stable_contracts() {
    for root_error in [
        validate_ai(&serde_json::Value::Null).unwrap_err(),
        validate_tool(&serde_json::Value::Null).unwrap_err(),
        validate_tool_catalog(&serde_json::Value::Null).unwrap_err(),
    ] {
        assert_eq!(root_error.code, WIRE_TYPE);
        assert_eq!(root_error.path, "$");
    }

    validate_budget_show(&serde_json::json!({
        "app": "notes",
        "period": "2026-08",
        "units_used": 7
    }))
    .unwrap();
    let chat_budget = validate_budget_show(&serde_json::json!({
        "period": "2026-08",
        "units_used": 7,
        "units_cap": 100
    }))
    .unwrap_err();
    assert_eq!(chat_budget.code, WIRE_REQUIRED);
    assert_eq!(chat_budget.path, "$.app");
}
