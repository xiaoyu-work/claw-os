use super::super::tests::info;
use super::*;
use serde_json::json;

#[test]
fn textual_input_is_preserved_without_dropping_other_input_kinds() {
    let params = Params::new(
        json!({ "input": [
            { "type": "text", "text": " one ", "text_elements": [] },
            { "type": "text", "text": "two\n" },
        ]}),
        TURN_FIELDS,
    )
    .unwrap();
    assert_eq!(prompt(&params).unwrap(), " one \ntwo\n");
    for input in [
        json!([{ "type": "image", "url": "https://example.invalid/picture" }]),
        json!([{ "type": "text", "text": "Hello", "text_elements": [{"byteRange":{"start":0,"end":2}}] }]),
        json!([]),
        json!([{ "type": "text", "text": "  " }]),
    ] {
        assert!(prompt(&Params::new(json!({ "input": input }), TURN_FIELDS).unwrap()).is_err());
    }
}

#[test]
fn rejects_execution_options_the_broker_cannot_honor() {
    for override_ in [
        json!({ "model": "different-model" }),
        json!({ "cwd": "/some/project" }),
        json!({ "runtimeWorkspaceRoots": ["/some/project"] }),
        json!({ "approvalPolicy": "never" }),
        json!({ "approvalsReviewer": "auto_review" }),
        json!({ "sandboxPolicy": { "type": "dangerFullAccess" } }),
        json!({ "effort": "high" }),
        json!({ "summary": "detailed" }),
        json!({ "serviceTier": "fast" }),
        json!({ "outputSchema": { "type": "object" } }),
        json!({ "additionalContext": { "x": { "kind": "application", "value": "secret instructions" } } }),
        json!({ "collaborationMode": { "mode": "plan" } }),
    ] {
        let params = Params::new(override_.clone(), TURN_FIELDS).unwrap();
        assert!(settings(&params, &info()).is_err(), "{override_}");
    }
}

#[test]
fn accepts_only_advertised_no_override_defaults() {
    let params = Params::new(
        json!({
            "model": "model-for-tests",
            "cwd": "/home/claw",
            "runtimeWorkspaceRoots": [],
            "approvalPolicy": "on-request",
            "approvalsReviewer": "user",
            "sandboxPolicy": { "type": "externalSandbox", "networkAccess": "restricted" },
            "serviceTier": "default",
        }),
        TURN_FIELDS,
    )
    .unwrap();
    settings(&params, &info()).unwrap();
}

#[test]
fn accepts_the_upstream_clients_mandatory_disabled_native_search_setting() {
    let params = Params::new(
        json!({
            "config": {
                "web_search": "disabled",
                "model_reasoning_effort": "none",
                "model_reasoning_summary": "auto",
                "personality": "none",
            },
        }),
        THREAD_FIELDS,
    )
    .unwrap();
    settings(&params, &info()).unwrap();
    for config in [
        json!({"web_search": "live"}),
        json!({"web_search": "cached"}),
        json!({"model_reasoning_effort": "high"}),
        json!({"model_reasoning_summary": "detailed"}),
        json!({"permissions": {}}),
        json!({"network": {}}),
        json!({"features": {"web_search": true}}),
    ] {
        let params = Params::new(json!({"config": config}), THREAD_FIELDS).unwrap();
        assert!(settings(&params, &info()).is_err());
    }
}
