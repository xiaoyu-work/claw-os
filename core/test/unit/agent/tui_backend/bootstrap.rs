use super::super::tests::info;
use super::*;

#[test]
fn bootstrap_advertises_only_the_configured_claw_model() {
    let info = info();
    let result = models(json!({ "includeHidden": true }), &info).unwrap();
    assert_eq!(result["data"].as_array().unwrap().len(), 1);
    assert_eq!(result["data"][0]["id"], info.model);
    assert_eq!(result["data"][0]["inputModalities"], json!(["text"]));
    assert_eq!(result["data"][0]["supportedReasoningEfforts"], json!([]));
    assert_eq!(result["nextCursor"], Value::Null);
}

#[test]
fn claw_credentials_are_not_claimed_to_be_codex_authentication() {
    let result = account(json!({ "refreshToken": false }), &info()).unwrap();
    assert_eq!(result["account"], Value::Null);
    assert_eq!(result["requiresOpenaiAuth"], false);
    assert_eq!(result["claw"]["ready"], true);
    assert!(account(json!({ "refreshToken": true }), &info()).is_err());
}

#[test]
fn absent_model_is_an_actionable_error_not_a_fake_catalog() {
    let mut info = info();
    info.provider.clear();
    info.model.clear();
    assert_eq!(models(json!({}), &info).unwrap_err().code, -32005);
    info.provider = "mock".into();
    info.model = "fake".into();
    assert!(models(json!({}), &info).is_err());
}

#[test]
fn config_is_a_read_only_allowlisted_projection() {
    let config = config(
        json!({ "includeLayers": true, "cwd": "/some/project" }),
        &info(),
    )
    .unwrap();
    assert_eq!(config["config"]["model_provider"], "claw");
    assert_eq!(config["config"]["web_search"], "disabled");
    assert_eq!(config["config"]["approval_policy"], "on-request");
    assert_eq!(config["layers"][0]["name"]["type"], "sessionFlags");
    let serialized = config.to_string();
    for forbidden in ["api_key", "extra_headers", "base_url", "encrypted_content"] {
        assert!(!serialized.contains(forbidden));
    }
}

#[test]
fn remote_folder_configuration_is_disabled_not_implicitly_trusted() {
    let info = info();
    let config = config(json!({}), &info).unwrap();
    assert_eq!(
        config["config"]["projects"][info.home.to_string_lossy().as_ref()]["trust_level"],
        "untrusted"
    );
    assert_eq!(config["config"]["approval_policy"], "on-request");
    assert_eq!(config["config"]["model_provider"], "claw");
}

#[test]
fn model_catalogue_pagination_preserves_the_configured_default() {
    let mut info = info();
    info.models.push("another-model".into());
    let first = models(json!({ "limit": 1 }), &info).unwrap();
    assert_eq!(first["data"][0]["isDefault"], true);
    let second = models(json!({ "limit": 1, "cursor": first["nextCursor"] }), &info).unwrap();
    assert_eq!(second["data"][0]["model"], "another-model");
    assert_eq!(second["data"][0]["isDefault"], false);
    assert_eq!(second["nextCursor"], Value::Null);
}
