use serde_json::{json, Value};

use crate::dispatch::CdpContext;
use crate::types::CdpEvent;

pub async fn handle(method: &str, params: &Value, ctx: &mut CdpContext) -> Result<Value, String> {
    match method {
        "setDiscoverTargets" => {
            ctx.pending_events.push(CdpEvent::new(
                "Target.targetCreated",
                json!({
                    "targetInfo": {
                        "targetId": "browser",
                        "type": "browser",
                        "title": "",
                        "url": "",
                        "attached": true,
                        "browserContextId": "",
                    }
                }),
            ));
            for page in &ctx.pages {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.targetCreated",
                    json!({
                        "targetInfo": {
                            "targetId": page.id,
                            "type": "page",
                            "title": page.title,
                            "url": page.url_string(),
                            "attached": false,
                            "browserContextId": page.context.id,
                        }
                    }),
                ));
            }
            Ok(json!({}))
        }
        "getTargets" => {
            let targets: Vec<Value> = ctx
                .pages
                .iter()
                .map(|page| {
                    json!({
                        "targetId": page.id,
                        "type": "page",
                        "title": page.title,
                        "url": page.url_string(),
                        "attached": true,
                        "browserContextId": page.context.id,
                    })
                })
                .collect();
            Ok(json!({ "targetInfos": targets }))
        }
        "createTarget" => {
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("about:blank");
            let browser_context_id = params
                .get("browserContextId")
                .and_then(|value| value.as_str());
            let page_id = ctx.create_page_in_context(browser_context_id)?;
            let session_id = format!("{}-session", page_id);

            if let Some(page) = ctx.get_page_mut(&page_id) {
                if url == "about:blank" || url.is_empty() {
                    page.navigate_blank();
                } else {
                    let _ = page.navigate(url).await;
                }
            }

            ctx.sessions.insert(session_id.clone(), page_id.clone());

            if let Some(page) = ctx.get_page(&page_id) {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.targetCreated",
                    json!({
                        "targetInfo": {
                            "targetId": page_id,
                            "type": "page",
                            "title": page.title,
                            "url": page.url_string(),
                            "attached": false,
                            "browserContextId": page.context.id,
                        }
                    }),
                ));
            }

            if let Some(page) = ctx.get_page(&page_id) {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.attachedToTarget",
                    json!({
                        "sessionId": session_id,
                        "targetInfo": {
                            "targetId": page_id,
                            "type": "page",
                            "title": page.title,
                            "url": page.url_string(),
                            "attached": true,
                            "browserContextId": page.context.id,
                        },
                        "waitingForDebugger": false,
                    }),
                ));
            }

            Ok(json!({ "targetId": page_id }))
        }
        "attachToBrowserTarget" => {
            // Playwright calls this on connect to obtain a session for the
            // implicit "browser" target. Returning Unknown method aborts
            // the connect handshake before any user code runs.
            let session_id = "browser-session".to_string();
            ctx.sessions
                .insert(session_id.clone(), "browser".to_string());

            ctx.pending_events.push(CdpEvent::new(
                "Target.attachedToTarget",
                json!({
                    "sessionId": session_id,
                    "targetInfo": {
                        "targetId": "browser",
                        "type": "browser",
                        "title": "",
                        "url": "",
                        "attached": true,
                        "browserContextId": "",
                    },
                    "waitingForDebugger": false,
                }),
            ));

            Ok(json!({ "sessionId": session_id }))
        }
        "attachToTarget" => {
            let target_id = params
                .get("targetId")
                .and_then(|v| v.as_str())
                .ok_or("targetId required")?;
            let session_id = format!("{}-session", target_id);
            ctx.sessions
                .insert(session_id.clone(), target_id.to_string());

            if let Some(page) = ctx.get_page(target_id) {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.attachedToTarget",
                    json!({
                        "sessionId": session_id,
                        "targetInfo": {
                            "targetId": target_id,
                            "type": "page",
                            "title": page.title,
                            "url": page.url_string(),
                            "attached": true,
                            "browserContextId": page.context.id,
                        },
                        "waitingForDebugger": false,
                    }),
                ));
            }

            Ok(json!({ "sessionId": session_id }))
        }
        "closeTarget" => {
            let target_id = params
                .get("targetId")
                .and_then(|v| v.as_str())
                .ok_or("targetId required")?;
            let session_id = format!("{}-session", target_id);

            ctx.pending_events.push(CdpEvent::new(
                "Target.detachedFromTarget",
                json!({
                    "sessionId": session_id,
                    "targetId": target_id,
                }),
            ));
            ctx.pending_events.push(CdpEvent::new(
                "Target.targetDestroyed",
                json!({ "targetId": target_id }),
            ));

            ctx.remove_page(target_id);
            Ok(json!({ "success": true }))
        }
        "setAutoAttach" => Ok(json!({})),
        "getBrowserContexts" => Ok(json!({ "browserContextIds": ctx.browser_context_ids() })),
        "createBrowserContext" => {
            let proxy = params
                .get("proxyServer")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned);
            let context_id = ctx.create_browser_context(proxy);
            Ok(json!({ "browserContextId": context_id }))
        }
        "disposeBrowserContext" => {
            let context_id = params
                .get("browserContextId")
                .and_then(|value| value.as_str())
                .ok_or("browserContextId required")?;
            let removed_pages = ctx.dispose_browser_context(context_id)?;
            for target_id in removed_pages {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.targetDestroyed",
                    json!({ "targetId": target_id }),
                ));
            }
            Ok(json!({}))
        }
        "getTargetInfo" => {
            let target_id = params.get("targetId").and_then(|v| v.as_str());
            match target_id {
                Some(id) => {
                    let page = ctx.get_page(id).ok_or("Target not found")?;
                    Ok(json!({
                        "targetInfo": {
                            "targetId": id,
                            "type": "page",
                            "title": page.title,
                            "url": page.url_string(),
                            "attached": true,
                            "browserContextId": page.context.id,
                        }
                    }))
                }
                None => Ok(json!({
                        "targetInfo": {
                            "targetId": "browser",
                            "type": "browser",
                            "title": "",
                            "url": "",
                            "attached": true,
                        }
                })),
            }
        }
        _ => Err(format!("Unknown Target method: {}", method)),
    }
}

#[cfg(test)]
mod tests {
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
}
