use crate::generated::{
    validate_ai, validate_tool, validate_tool_catalog, WIRE_ENUM, WIRE_MINIMUM,
    WIRE_REQUIRED, WIRE_TYPE, WIRE_UNKNOWN_FIELD,
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

    let mut malformed_input = valid_ai();
    malformed_input["tool_calls"][0]["input"] = serde_json::json!("scalar");
    cases.push((malformed_input, WIRE_TYPE, "$.tool_calls[0].input"));

    for (payload, code, path) in cases {
        let error = validate_ai(&payload).unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.path, path);
    }
}

#[test]
fn validators_accept_valid_payloads_and_reject_malformed_structured_items() {
    validate_ai(&valid_ai()).unwrap();
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
