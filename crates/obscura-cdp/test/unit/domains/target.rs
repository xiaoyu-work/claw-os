use super::*;

#[tokio::test]
async fn attach_to_browser_target_returns_session_id() {
    let mut ctx = CdpContext::new();
    let result = handle("attachToBrowserTarget", &json!({}), &mut ctx)
        .await
        .expect("attachToBrowserTarget should succeed");

    assert_eq!(result["sessionId"], "browser-session");
    assert_eq!(
        ctx.sessions.get("browser-session").map(String::as_str),
        Some("browser")
    );

    // Playwright/Puppeteer expect a Target.attachedToTarget event before
    // they finish wiring up the session — without it the connect promise
    // hangs.
    let attached_evt = ctx
        .pending_events
        .iter()
        .find(|e| e.method == "Target.attachedToTarget")
        .expect("attachedToTarget event must be emitted");
    assert_eq!(attached_evt.params["sessionId"], "browser-session");
    assert_eq!(attached_evt.params["targetInfo"]["type"], "browser");
}

#[tokio::test]
async fn unknown_target_method_still_errors() {
    let mut ctx = CdpContext::new();
    let err = handle("notARealMethod", &json!({}), &mut ctx)
        .await
        .expect_err("unknown methods must surface as errors");
    assert!(err.contains("Unknown Target method"));
}

#[tokio::test]
async fn browser_contexts_are_distinct_and_targets_honor_context_id() {
    let mut ctx = CdpContext::new();
    let first = handle("createBrowserContext", &json!({}), &mut ctx)
        .await
        .unwrap()["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    let second = handle("createBrowserContext", &json!({}), &mut ctx)
        .await
        .unwrap()["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(first, second);

    let target = handle(
        "createTarget",
        &json!({"url": "about:blank", "browserContextId": first.clone()}),
        &mut ctx,
    )
    .await
    .unwrap();
    let target_id = target["targetId"].as_str().unwrap();
    assert_eq!(ctx.get_page(target_id).unwrap().context.id, first);

    let first_context = ctx.get_browser_context(Some(&first)).unwrap();
    let second_context = ctx.get_browser_context(Some(&second)).unwrap();
    let url = url::Url::parse("https://example.com/").unwrap();
    first_context
        .cookie_jar
        .set_cookie("isolated=yes; Path=/", &url);
    assert!(second_context.cookie_jar.get_cookie_header(&url).is_empty());
}

#[tokio::test]
async fn disposing_context_does_not_clear_default_context() {
    let mut ctx = CdpContext::new();
    let default_url = url::Url::parse("https://example.com/").unwrap();
    ctx.default_context
        .cookie_jar
        .set_cookie("default=yes; Path=/", &default_url);

    let context_id = handle("createBrowserContext", &json!({}), &mut ctx)
        .await
        .unwrap()["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    handle(
        "disposeBrowserContext",
        &json!({"browserContextId": context_id}),
        &mut ctx,
    )
    .await
    .unwrap();

    assert!(ctx
        .default_context
        .cookie_jar
        .get_cookie_header(&default_url)
        .contains("default=yes"));
}
