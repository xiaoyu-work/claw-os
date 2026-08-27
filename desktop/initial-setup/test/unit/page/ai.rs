use super::*;
use crate::page::Page as _;

#[test]
fn apply_failure_is_visible_and_retains_inputs_for_retry() {
    let mut ai = Page::new();
    ai.selected = Some(8);
    ai.model = "deployment-name".to_string();
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
    assert_eq!(ai.api_key, "retry-me");
    assert_eq!(
        ai.extras.get("base_url").map(String::as_str),
        Some("https://example.openai.azure.com")
    );
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
