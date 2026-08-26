use super::*;

#[tokio::test(flavor = "current_thread")]
async fn with_full_options_propagates_user_agent_to_http_client() {
    let ctx = BrowserContext::with_full_options(
        "test".to_string(),
        None,
        false,
        Some("Custom-UA/1.0".to_string()),
    );
    assert_eq!(ctx.user_agent, "Custom-UA/1.0");
    let client_ua = ctx.http_client.user_agent.read().await.clone();
    assert_eq!(client_ua, "Custom-UA/1.0");
}

#[tokio::test(flavor = "current_thread")]
async fn with_full_options_falls_back_to_chrome_default() {
    let ctx = BrowserContext::with_full_options(
        "test".to_string(),
        None,
        false,
        None,
    );
    assert!(ctx.user_agent.contains("Chrome"));
    let client_ua = ctx.http_client.user_agent.read().await.clone();
    assert!(client_ua.contains("Chrome"));
    assert_eq!(ctx.user_agent, client_ua);
}

#[tokio::test(flavor = "current_thread")]
async fn with_options_keeps_default_user_agent() {
    let ctx = BrowserContext::with_options("test".to_string(), None, false);
    assert!(ctx.user_agent.contains("Chrome"));
}
