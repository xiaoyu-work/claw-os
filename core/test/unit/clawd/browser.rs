use super::*;

fn request(value: Value) -> BrowserControl {
    serde_json::from_value(value).expect("valid browser request")
}

#[test]
fn navigation_derives_the_exact_canonical_host_capability() {
    let prepared = prepare_action(&request(json!({
        "session": "app-browser",
        "action": "nav.go",
        "tab_id": 7,
        "url": "HTTPS://Example.COM/path"
    })))
    .unwrap();

    assert_eq!(
        prepared.capability,
        Cap::new(Verb::BROWSER_NAV, Scope::host("example.com:443"))
    );
    assert_eq!(prepared.verb, "nav.go");
    assert_eq!(prepared.args["id"], 7);
    assert_eq!(prepared.args["url"], "https://example.com/path");
}

#[test]
fn page_actions_bind_authority_to_the_declared_origin() {
    let prepared = prepare_action(&request(json!({
        "session": "app-browser",
        "action": "dom.query",
        "tab_id": 11,
        "page_url": "http://127.0.0.1:8080/private",
        "selector": "main"
    })))
    .unwrap();

    assert_eq!(
        prepared.capability,
        Cap::new(Verb::BROWSER_DOM_READ, Scope::host("127.0.0.1:8080"))
    );
    assert_eq!(prepared.args["expected_origin"], "http://127.0.0.1:8080");
    assert!(prepared.args.get("page_url").is_none());
}

#[test]
fn page_origin_keeps_the_scheme_separate_from_the_host_capability() {
    let http = prepare_action(&request(json!({
        "session": "app-browser",
        "action": "dom.query",
        "tab_id": 11,
        "page_url": "http://example.com:443/",
        "selector": "main"
    })))
    .unwrap();
    let https = prepare_action(&request(json!({
        "session": "app-browser",
        "action": "dom.query",
        "tab_id": 11,
        "page_url": "https://example.com/",
        "selector": "main"
    })))
    .unwrap();

    assert_eq!(http.capability.scope, https.capability.scope);
    assert_eq!(http.args["expected_origin"], "http://example.com:443");
    assert_eq!(https.args["expected_origin"], "https://example.com:443");
}

#[test]
fn secret_and_eval_flags_are_injected_only_after_action_selection() {
    let secret = prepare_action(&request(json!({
        "session": "app-browser",
        "action": "dom.fill_secret",
        "tab_id": 2,
        "page_url": "https://example.com/login",
        "reference": "field-1",
        "value": "secret"
    })))
    .unwrap();
    assert_eq!(secret.verb, "dom.fill");
    assert_eq!(secret.args["allow_secret"], true);
    assert_eq!(secret.capability.verb, Verb::BROWSER_INPUT_SECRET);

    let eval = prepare_action(&request(json!({
        "session": "app-browser",
        "action": "eval",
        "tab_id": 2,
        "page_url": "https://example.com/",
        "expr": "document.title"
    })))
    .unwrap();
    assert_eq!(eval.args["allow_eval"], true);
    assert_eq!(eval.capability.verb, Verb::BROWSER_EVAL);

    assert!(serde_json::from_value::<BrowserControl>(json!({
        "session": "app-browser",
        "action": "eval",
        "tab_id": 2,
        "page_url": "https://example.com/",
        "expr": "document.title",
        "allow_eval": true
    }))
    .is_err());
}

#[test]
fn invalid_tab_ids_and_browser_schemes_are_refused() {
    let bad_tab = request(json!({
        "session": "app-browser",
        "action": "tabs.activate",
        "tab_id": 0
    }));
    assert!(prepare_action(&bad_tab).unwrap_err().contains("tab_id"));

    let ftp = request(json!({
        "session": "app-browser",
        "action": "nav.go",
        "tab_id": 1,
        "url": "ftp://example.com/file"
    }));
    assert!(prepare_action(&ftp).unwrap_err().contains("http or https"));

    let credentials = request(json!({
        "session": "app-browser",
        "action": "nav.go",
        "tab_id": 1,
        "url": "https://user:pass@example.com/"
    }));
    assert!(prepare_action(&credentials)
        .unwrap_err()
        .contains("credentials"));
}
