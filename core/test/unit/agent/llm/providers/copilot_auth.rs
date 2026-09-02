use super::*;

#[test]
fn derive_base_url_picks_proxy_ep() {
    let tok =
        "tid=abc;exp=1700000000;proxy-ep=proxy.business.githubcopilot.com;sku=enterprise";
    assert_eq!(
        derive_base_url_from_token(tok),
        "https://api.business.githubcopilot.com"
    );
}

#[test]
fn derive_base_url_strips_scheme_prefix() {
    let tok = "proxy-ep=https://proxy.individual.githubcopilot.com";
    assert_eq!(
        derive_base_url_from_token(tok),
        "https://api.individual.githubcopilot.com"
    );
}

#[test]
fn derive_base_url_passthrough_non_proxy_host() {
    // If the token doesn't follow the `proxy.<region>` convention we
    // honour whatever it provides rather than guessing.
    let tok = "proxy-ep=custom.copilot.example.com";
    assert_eq!(
        derive_base_url_from_token(tok),
        "https://custom.copilot.example.com"
    );
}

#[test]
fn derive_base_url_falls_back_when_proxy_ep_missing() {
    assert_eq!(
        derive_base_url_from_token("tid=abc;exp=1700000000"),
        DEFAULT_COPILOT_BASE_URL
    );
    assert_eq!(
        derive_base_url_from_token(""),
        DEFAULT_COPILOT_BASE_URL
    );
}

#[test]
fn fingerprint_is_stable_and_sensitive() {
    let a = token_fingerprint("ghu_aaaaaa");
    let b = token_fingerprint("ghu_aaaaaa");
    let c = token_fingerprint("ghu_bbbbbb");
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn cache_is_isolated_per_token() {
    let fp_a = token_fingerprint("cachetest_token_aaaaa");
    let fp_b = token_fingerprint("cachetest_token_bbbbb");
    store_cached(
        fp_a,
        CopilotToken {
            bearer: "tok_a".into(),
            base_url: "https://api.individual.githubcopilot.com".into(),
            expires_at_unix: u64::MAX,
        },
    )
    .unwrap();
    assert!(lookup_cached(fp_a).unwrap().is_some());
    assert!(lookup_cached(fp_b).unwrap().is_none());
}

fn model(value: serde_json::Value) -> CopilotModel {
    serde_json::from_value(value).expect("valid Copilot model fixture")
}

#[test]
fn model_wire_api_uses_advertised_endpoints() {
    let legacy = model(serde_json::json!({"id": "gpt-4o"}));
    assert_eq!(
        legacy.wire_api(),
        Some(CopilotWireApi::ChatCompletions),
        "missing endpoint metadata means legacy chat completions"
    );
    let null_endpoints = model(serde_json::json!({
        "id": "gpt-4.1",
        "supported_endpoints": null
    }));
    assert_eq!(
        null_endpoints.wire_api(),
        Some(CopilotWireApi::ChatCompletions)
    );

    let dual = model(serde_json::json!({
        "id": "gpt-4.1",
        "supported_endpoints": ["/chat/completions", "/responses"]
    }));
    assert_eq!(
        dual.wire_api(),
        Some(CopilotWireApi::Responses),
        "dual-protocol models prefer the newer Responses API"
    );

    let responses = model(serde_json::json!({
        "id": "gpt-5.6-sol",
        "supported_endpoints": ["/responses"]
    }));
    assert_eq!(responses.wire_api(), Some(CopilotWireApi::Responses));

    let messages = model(serde_json::json!({
        "id": "messages-only",
        "supported_endpoints": ["/v1/messages"]
    }));
    assert_eq!(messages.wire_api(), None);
}

#[test]
fn model_picker_excludes_non_chat_and_disabled_models() {
    let embedding = model(serde_json::json!({
        "id": "text-embedding-3-small",
        "capabilities": {"type": "embeddings"},
        "supported_endpoints": ["/embeddings"]
    }));
    assert!(!embedding.is_selectable_chat_model());

    let hidden = model(serde_json::json!({
        "id": "trajectory-compaction",
        "model_picker_enabled": false
    }));
    assert!(!hidden.is_selectable_chat_model());

    let disabled = model(serde_json::json!({
        "id": "disabled-chat",
        "model_picker_enabled": true,
        "policy": {"state": "disabled"},
        "capabilities": {"type": "chat"},
        "supported_endpoints": ["/chat/completions"]
    }));
    assert!(!disabled.is_selectable_chat_model());

    let pending = model(serde_json::json!({
        "id": "consent-required",
        "model_picker_enabled": true,
        "policy": {"state": "requires_consent"},
        "capabilities": {"type": "chat"},
        "supported_endpoints": ["/responses"]
    }));
    assert!(!pending.is_selectable_chat_model());

    let responses = model(serde_json::json!({
        "id": "gpt-5.6-sol",
        "model_picker_enabled": true,
        "capabilities": {"type": "chat"},
        "supported_endpoints": ["/responses"]
    }));
    assert!(responses.is_selectable_chat_model());
}

#[test]
fn needs_refresh_when_close_to_expiry() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stale = CopilotToken {
        bearer: "x".into(),
        base_url: "https://api.individual.githubcopilot.com".into(),
        expires_at_unix: now + 60, // less than the 5-min margin
    };
    let fresh = CopilotToken {
        bearer: "x".into(),
        base_url: "https://api.individual.githubcopilot.com".into(),
        expires_at_unix: now + 60 * 60, // 1h ahead
    };
    assert!(needs_refresh(&stale));
    assert!(!needs_refresh(&fresh));
}

/// Regression: `truncate` used to do `&s[..n]`, which panics
/// when `n` lands inside a multi-byte UTF-8 sequence. A 240-byte
/// truncation of an error body that happens to contain CJK
/// characters around byte 240 would crash the LLM request path
/// instead of surfacing the upstream error.
#[test]
fn truncate_handles_non_ascii() {
    // Each '配' is 3 bytes in UTF-8. With max=4 the old impl would
    // try to slice at byte index 4 (mid-character) and panic.
    let s = "配额不足配额不足"; // 24 bytes, 8 chars
    let out = truncate(s, 4);
    // Exactly 4 chars + ellipsis.
    assert_eq!(out.chars().count(), 5);
    assert!(out.ends_with('…'));
    // And the prefix is the first four characters intact.
    assert!(out.starts_with("配额不足"));

    // Boundary cases that previously panicked.
    for n in [1usize, 2, 3, 5, 7] {
        // Must not panic.
        let _ = truncate(s, n);
    }

    // ASCII fast path still works.
    assert_eq!(truncate("hello", 100), "hello");
    assert!(truncate("hello world", 5).starts_with("hello"));
}

/// Sanity-check the exchange-mutex helper: two acquisitions for
/// the same fingerprint return the same underlying Arc, while
/// different fingerprints get different locks.
#[tokio::test]
async fn exchange_lock_is_per_fingerprint() {
    let a1 = exchange_lock_for(1).unwrap();
    let a2 = exchange_lock_for(1).unwrap();
    let b = exchange_lock_for(2).unwrap();
    assert!(Arc::ptr_eq(&a1, &a2), "same fingerprint must share lock");
    assert!(!Arc::ptr_eq(&a1, &b), "different fingerprints must NOT share lock");

    // Holding the lock for fingerprint 1 must not block fingerprint 2.
    let g = a1.lock().await;
    let started = std::time::Instant::now();
    let _g2 = b.try_lock().expect("different lock must be free");
    assert!(started.elapsed() < std::time::Duration::from_millis(50));
    drop(g);
}

#[tokio::test]
async fn rejected_token_refresh_invalidates_token_and_model_catalog_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let github_token = "github_issue_17_refresh_once";
    let rejected = CopilotToken {
        bearer: "copilot_issue_17_rejected".into(),
        base_url: "https://api.individual.githubcopilot.com".into(),
        expires_at_unix: u64::MAX,
    };
    let fresh = CopilotToken {
        bearer: "copilot_issue_17_fresh".into(),
        base_url: "https://api.business.githubcopilot.com".into(),
        expires_at_unix: u64::MAX,
    };
    store_cached(token_fingerprint(github_token), rejected.clone()).unwrap();
    let rejected_fingerprint = token_fingerprint(&rejected.bearer);
    store_model_catalog(
        rejected_fingerprint,
        Arc::new(vec![model(serde_json::json!({"id": "stale-model"}))]),
    )
    .unwrap();

    let exchanges = Arc::new(AtomicUsize::new(0));
    let exchanges_for_call = exchanges.clone();
    let fresh_for_call = fresh.clone();
    let actual = refresh_rejected_copilot_token_with(
        github_token,
        &rejected,
        move |seen_github_token| async move {
            assert_eq!(seen_github_token, github_token);
            exchanges_for_call.fetch_add(1, Ordering::SeqCst);
            Ok(fresh_for_call)
        },
    )
    .await
    .unwrap();

    assert_eq!(actual.bearer, fresh.bearer);
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(
        lookup_cached(token_fingerprint(github_token))
            .unwrap()
            .unwrap()
            .bearer,
        fresh.bearer
    );
    assert!(
        lookup_model_catalog(rejected_fingerprint).unwrap().is_none(),
        "the catalogue belongs to the rejected short-lived token"
    );
}

#[tokio::test]
async fn rejected_token_refresh_preserves_concurrent_replacement() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let github_token = "github_issue_17_concurrent_replacement";
    let rejected = CopilotToken {
        bearer: "copilot_issue_17_old".into(),
        base_url: "https://api.individual.githubcopilot.com".into(),
        expires_at_unix: u64::MAX,
    };
    let replacement = CopilotToken {
        bearer: "copilot_issue_17_already_refreshed".into(),
        base_url: "https://api.business.githubcopilot.com".into(),
        expires_at_unix: u64::MAX,
    };
    let rejected_fingerprint = token_fingerprint(&rejected.bearer);
    let replacement_fingerprint = token_fingerprint(&replacement.bearer);
    store_model_catalog(
        rejected_fingerprint,
        Arc::new(vec![model(serde_json::json!({"id": "stale-model"}))]),
    )
    .unwrap();
    store_model_catalog(
        replacement_fingerprint,
        Arc::new(vec![model(serde_json::json!({"id": "fresh-model"}))]),
    )
    .unwrap();
    store_cached(token_fingerprint(github_token), replacement.clone()).unwrap();

    let exchanges = Arc::new(AtomicUsize::new(0));
    let exchanges_for_call = exchanges.clone();
    let fallback = replacement.clone();
    let actual =
        refresh_rejected_copilot_token_with(github_token, &rejected, move |_| async move {
            exchanges_for_call.fetch_add(1, Ordering::SeqCst);
            Ok(fallback)
        })
        .await
        .unwrap();

    assert_eq!(actual.bearer, replacement.bearer);
    assert_eq!(exchanges.load(Ordering::SeqCst), 0);
    assert!(lookup_model_catalog(rejected_fingerprint).unwrap().is_none());
    assert!(
        lookup_model_catalog(replacement_fingerprint).unwrap().is_some(),
        "only the rejected token's catalogue should be invalidated"
    );
}
