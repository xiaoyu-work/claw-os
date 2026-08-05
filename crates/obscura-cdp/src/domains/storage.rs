use serde_json::{json, Value};

use crate::dispatch::CdpContext;
use obscura_browser::BrowserContext;
use std::sync::Arc;

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    _session_id: &Option<String>,
) -> Result<Value, String> {
    let context = context_for_request(params, ctx, _session_id)?;
    match method {
        "getCookies" => {
            let cookies = context.cookie_jar.get_all_cookies();
            let cdp_cookies: Vec<Value> = cookies
                .iter()
                .map(|c| {
                    json!({
                        "name": c.name,
                        "value": c.value,
                        "domain": c.domain,
                        "path": c.path,
                        "expires": -1,
                        "size": c.name.len() + c.value.len(),
                        "httpOnly": c.http_only,
                        "secure": c.secure,
                        "session": true,
                        "priority": "Medium",
                        "sameParty": false,
                        "sourceScheme": if c.secure { "Secure" } else { "NonSecure" },
                        "sourcePort": 80,
                    })
                })
                .collect();
            Ok(json!({ "cookies": cdp_cookies }))
        }
        "setCookies" => {
            if let Some(cookies) = params.get("cookies").and_then(|v| v.as_array()) {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();

                for c in cookies {
                    let name = match c.get("name").and_then(|v| v.as_str()) {
                        Some(n) => n.to_string(),
                        None => continue,
                    };
                    let value = c
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let domain = c
                        .get("domain")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            c.get("url")
                                .and_then(|v| v.as_str())
                                .and_then(|u| url::Url::parse(u).ok())
                                .and_then(|u| u.host_str().map(|h| h.to_string()))
                        })
                        .unwrap_or_default();

                    let expires = c.get("expires").and_then(|v| v.as_f64());
                    if let Some(exp) = expires {
                        if exp > 0.0 && exp < now {
                            context.cookie_jar.delete_cookie(&name, &domain);
                            continue;
                        }
                    }

                    let path = c
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("/")
                        .to_string();
                    let secure = c.get("secure").and_then(|v| v.as_bool()).unwrap_or(false);
                    let http_only = c.get("httpOnly").and_then(|v| v.as_bool()).unwrap_or(false);

                    context
                        .cookie_jar
                        .set_cookies_from_cdp(vec![obscura_net::CookieInfo {
                            name,
                            value,
                            domain,
                            path,
                            secure,
                            http_only,
                        }]);
                }
            }
            Ok(json!({}))
        }
        "deleteCookies" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let domain_from_url = params
                .get("url")
                .and_then(|v| v.as_str())
                .and_then(|u| url::Url::parse(u).ok())
                .and_then(|u| u.host_str().map(|h| h.to_string()));
            let domain = params
                .get("domain")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or(domain_from_url)
                .unwrap_or_default();

            if !name.is_empty() {
                context.cookie_jar.delete_cookie(name, &domain);
            }
            Ok(json!({}))
        }
        _ => Ok(json!({})),
    }
}

fn context_for_request(
    params: &Value,
    ctx: &CdpContext,
    session_id: &Option<String>,
) -> Result<Arc<BrowserContext>, String> {
    if let Some(context_id) = params
        .get("browserContextId")
        .and_then(|value| value.as_str())
    {
        return ctx
            .get_browser_context(Some(context_id))
            .ok_or_else(|| format!("Browser context not found: {context_id}"));
    }
    if let Some(page) = ctx.get_session_page(session_id) {
        return Ok(page.context.clone());
    }
    Ok(ctx.default_context.clone())
}
