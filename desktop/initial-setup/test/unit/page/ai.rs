use super::*;
use crate::page::Page as _;

fn update(ai: &mut Page, message: Message) {
    drop(ai.update(message));
}

#[test]
fn provider_changes_replace_only_automatic_models() {
    let mut ai = Page::new();

    update(&mut ai, Message::SelectProvider(0));
    assert_eq!(ai.model, DEFAULT_MODELS[0]);
    assert!(!ai.model_edited);

    update(&mut ai, Message::SelectProvider(1));
    assert_eq!(ai.model, DEFAULT_MODELS[1]);
    assert!(!ai.model_edited);

    ai.apply_result(&Err("verification failed".to_string()));
    update(&mut ai, Message::SelectProvider(8));
    assert!(ai.model.is_empty());
    assert!(!ai.model_edited);

    update(&mut ai, Message::SelectProvider(0));
    assert_eq!(ai.model, DEFAULT_MODELS[0]);

    update(&mut ai, Message::EditModel("my-deployment".to_string()));
    update(&mut ai, Message::SelectProvider(8));
    assert_eq!(ai.model, "my-deployment");
    assert!(ai.model_edited);

    update(&mut ai, Message::SelectProvider(0));
    assert_eq!(ai.model, "my-deployment");
}

#[test]
fn explicit_edit_is_preserved_even_when_text_matches_a_default() {
    let mut ai = Page::new();
    update(&mut ai, Message::SelectProvider(0));
    update(&mut ai, Message::EditModel(DEFAULT_MODELS[0].to_string()));

    update(&mut ai, Message::SelectProvider(1));

    assert_eq!(ai.model, DEFAULT_MODELS[0]);
    assert!(ai.model_edited);
}

#[test]
fn reselecting_provider_preserves_live_automatic_model_and_oauth() {
    let mut ai = Page::new();
    update(&mut ai, Message::SelectProvider(2));
    ai.oauth = OauthState::Authorized;
    update(
        &mut ai,
        Message::OauthModelsRefreshed {
            provider_index: 2,
            result: Ok(vec!["account-model".to_string()]),
        },
    );

    assert_eq!(ai.model, "account-model");
    assert!(!ai.model_edited);

    update(&mut ai, Message::SelectProvider(2));

    assert_eq!(ai.model, "account-model");
    assert!(matches!(ai.oauth, OauthState::Authorized));
}

#[test]
fn oauth_model_refresh_does_not_replace_explicit_input() {
    let mut ai = Page::new();
    update(&mut ai, Message::SelectProvider(2));
    update(
        &mut ai,
        Message::EditModel("custom-copilot-model".to_string()),
    );
    ai.oauth = OauthState::Authorized;

    update(
        &mut ai,
        Message::OauthModelsRefreshed {
            provider_index: 2,
            result: Ok(vec!["live-default".to_string()]),
        },
    );

    assert_eq!(ai.model, "custom-copilot-model");
    assert!(ai.model_edited);
}

#[test]
fn stale_oauth_model_refresh_cannot_change_new_provider() {
    let mut ai = Page::new();
    update(&mut ai, Message::SelectProvider(2));
    ai.oauth = OauthState::Authorized;
    update(&mut ai, Message::SelectProvider(1));

    update(
        &mut ai,
        Message::OauthModelsRefreshed {
            provider_index: 2,
            result: Ok(vec!["copilot-model".to_string()]),
        },
    );

    assert_eq!(ai.model, DEFAULT_MODELS[1]);
    assert!(!ai.model_edited);
    assert!(matches!(ai.oauth, OauthState::Idle));
}

#[test]
fn apply_failure_is_visible_and_retains_inputs_for_retry() {
    let mut ai = Page::new();
    update(&mut ai, Message::SelectProvider(8));
    update(&mut ai, Message::EditModel("deployment-name".to_string()));
    ai.api_key = "retry-me".to_string();
    ai.extras
        .insert("base_url", "https://example.openai.azure.com".to_string());

    ai.apply_result(&Err("provider verification failed".to_string()));

    assert!(matches!(
        ai.last_outcome.as_ref(),
        Some(Err(reason)) if reason == "provider verification failed"
    ));
    assert_eq!(ai.selected, Some(8));
    assert_eq!(ai.model, "deployment-name");
    assert!(ai.model_edited);
    assert_eq!(ai.api_key, "retry-me");
    assert_eq!(
        ai.extras.get("base_url").map(String::as_str),
        Some("https://example.openai.azure.com")
    );

    update(&mut ai, Message::SelectProvider(1));
    assert_eq!(ai.model, "deployment-name");
    assert!(ai.model_edited);
}

#[test]
fn successful_apply_drops_the_secret_only_after_all_pages_finish() {
    let mut ai = Page::new();
    ai.api_key = "discard-me".to_string();

    ai.apply_result(&Ok(()));

    assert!(matches!(ai.last_outcome, Some(Ok(()))));
    assert_eq!(ai.api_key, "discard-me");

    ai.all_settings_applied();

    assert!(ai.api_key.is_empty());
}
