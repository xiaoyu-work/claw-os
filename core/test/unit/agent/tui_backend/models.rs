use super::*;

#[test]
fn uses_only_the_configured_providers_static_catalogue() {
    let config = AgentConfig {
        provider: "anthropic".into(),
        model: "claude-sonnet-custom".into(),
        ..AgentConfig::default()
    };
    let models = validate_catalog(static_catalog(&config), &config.model).unwrap();
    assert_eq!(models[0], "claude-sonnet-custom");
    assert!(models.iter().any(|model| model == "claude-haiku-4-5"));
    assert!(!models.iter().any(|model| model.starts_with("gpt-")));
}

#[test]
fn custom_endpoints_do_not_inherit_an_unrelated_provider_catalogue() {
    let config = AgentConfig {
        provider: "openai".into(),
        model: "private-deployment".into(),
        base_url: Some("https://models.example.invalid".into()),
        ..AgentConfig::default()
    };
    assert_eq!(static_catalog(&config), vec!["private-deployment"]);
    let native = AgentConfig {
        base_url: Some("https://api.openai.com/v1/".into()),
        ..config
    };
    assert!(static_catalog(&native)
        .iter()
        .any(|model| model == "gpt-4.1"));
}

#[test]
fn catalogue_identifiers_are_bounded_and_never_contain_credentials() {
    for invalid in [
        "".to_string(),
        "with spaces".to_string(),
        "x".repeat(257),
        "sk-abcdefghijklmnopqrstuvwx".to_string(),
    ] {
        assert!(validate_catalog(vec![invalid], "primary").is_err());
    }
    let models = validate_catalog(vec!["b".into(), "a".into(), "b".into()], "b").unwrap();
    assert_eq!(models, ["b", "a"]);
}

#[tokio::test]
async fn unknown_providers_offer_only_the_explicitly_configured_model_without_io() {
    let config = Arc::new(AgentConfig {
        provider: "unknown-test-provider".into(),
        model: "configured-model".into(),
        ..AgentConfig::default()
    });
    assert_eq!(catalog(config, false).await.unwrap(), ["configured-model"]);
}
