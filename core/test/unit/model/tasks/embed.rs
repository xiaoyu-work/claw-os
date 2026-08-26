use super::*;

fn cfg() -> EmbedConfig {
    let mut c = EmbedConfig::default();
    c.provider = "openai".into();
    c.model = "text-embedding-3-small".into();
    c
}

#[test]
fn build_returns_none_when_disabled() {
    let mut c = EmbedConfig::default();
    c.provider = "none".into();
    assert!(build_from(&c).unwrap().is_none());
}

#[test]
fn build_auto_returns_none_when_local_missing_and_no_cloud_agent() {
    // provider="auto" + no local Qwen3 stack + an agent provider
    // that can't do embeddings (anthropic) → semantic recall stays
    // off (None). We never fall back to a low-quality local hash.
    let mut c = EmbedConfig::default();
    c.model_dir = Some(
        std::env::temp_dir()
            .join("cos-test-missing-qwen3-embedding-stack")
            .display()
            .to_string(),
    );
    let mut agent = crate::config::AgentConfig::default();
    agent.provider = "anthropic".into();
    assert!(build_from_with_agent(&c, &agent).unwrap().is_none());
}

#[test]
fn build_auto_falls_back_to_cloud_agent_when_local_missing() {
    // provider="auto" + no local Qwen3 stack + an OpenAI-shape agent
    // provider → derive a real cloud embedder rather than disabling
    // semantic recall (the AI system always wants true embeddings).
    let mut c = EmbedConfig::default();
    c.model_dir = Some(
        std::env::temp_dir()
            .join("cos-test-missing-qwen3-embedding-stack")
            .display()
            .to_string(),
    );
    let mut agent = crate::config::AgentConfig::default();
    agent.provider = "openai".into();
    agent.base_url = Some("https://api.openai.com/v1".into());
    agent.api_key_env = Some("OPENAI_API_KEY".into());

    let built = build_from_with_agent(&c, &agent)
        .expect("auto cloud fallback ok")
        .expect("openai agent → some");
    assert_eq!(built.name(), "openai");
}

#[test]
fn build_returns_err_for_unknown_provider() {
    let mut c = EmbedConfig::default();
    c.provider = "magic".into();
    assert!(build_from(&c).is_err());
}

#[test]
fn build_returns_some_for_openai() {
    assert!(build_from(&cfg()).unwrap().is_some());
}

#[test]
fn endpoint_builder_handles_query_string() {
    let mut c = cfg();
    c.base_url = Some(
        "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/text-embedding-3-small?api-version=2024-02-01".into(),
    );
    let e = OpenAICompatEmbedder::from_config(&c);
    assert_eq!(
        e.endpoint(),
        "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/text-embedding-3-small/embeddings?api-version=2024-02-01"
    );
}

#[test]
fn endpoint_builder_default_path() {
    let mut c = cfg();
    c.base_url = Some("https://api.openai.com/v1".into());
    let e = OpenAICompatEmbedder::from_config(&c);
    assert_eq!(e.endpoint(), "https://api.openai.com/v1/embeddings");
}

/// Agent auto-derive: `[embed].provider = "agent-auto"` reads
/// the main agent provider and builds an OpenAI-compat embedder
/// when the alias is compatible. Inherits base_url + credentials
/// from `[agent]`.
#[test]
fn build_agent_auto_derives_from_openai_main() {
    let mut embed_cfg = EmbedConfig::default();
    embed_cfg.provider = "agent-auto".into();
    embed_cfg.model = String::new();
    let mut agent = crate::config::AgentConfig::default();
    agent.provider = "openai".into();
    agent.base_url = Some("https://api.openai.com/v1".into());
    agent.api_key_env = Some("OPENAI_API_KEY".into());

    let built = build_from_with_agent(&embed_cfg, &agent)
        .expect("agent-auto derive ok")
        .expect("openai main → some");
    assert_eq!(built.name(), "openai");
    assert_eq!(built.model(), MODEL_NAME);
}

/// Agent auto-derive against an Azure agent without an explicit
/// `[embed].model` is now an explicit error (audit fix): the
/// OpenAI canonical name `text-embedding-3-small` does not exist
/// as an Azure deployment, so silently substituting it would
/// produce a confusing 404 at runtime rather than a clear
/// configuration error.
#[test]
fn build_agent_auto_derives_from_azure_main() {
    let mut embed_cfg = EmbedConfig::default();
    embed_cfg.provider = "agent-auto".into();
    embed_cfg.model = String::new();
    let mut agent = crate::config::AgentConfig::default();
    agent.provider = "azure".into();
    agent.base_url =
        Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview".into());
    agent.api_key_credential = Some("azure_api_key".into());

    let err = match build_from_with_agent(&embed_cfg, &agent) {
        Err(e) => e,
        Ok(_) => panic!("azure without explicit [embed].model must error"),
    };
    assert!(
        err.contains("[embed].model is required"),
        "unexpected error message: {err}",
    );

    // Setting an explicit model name resolves the error.
    embed_cfg.model = "my-azure-embed-deployment".into();
    let built = build_from_with_agent(&embed_cfg, &agent)
        .expect("with explicit model: ok")
        .expect("azure main → some");
    assert_eq!(built.name(), "azure");
    assert_eq!(built.model(), "my-azure-embed-deployment");
}

/// Agent auto-derive against a non-OpenAI-shape provider (mock /
/// anthropic / gemini / bedrock) silently returns `None` —
/// embedding stays off and the runtime continues without
/// semantic memory.
#[test]
fn build_agent_auto_returns_none_for_mock_main() {
    let mut embed_cfg = EmbedConfig::default();
    embed_cfg.provider = "agent-auto".into();
    let agent = crate::config::AgentConfig::default(); // provider == "mock"
    assert!(build_from_with_agent(&embed_cfg, &agent).unwrap().is_none());
}

/// Agent auto-derive against an unsupported main provider also returns
/// `None` (rather than erroring) so misconfigured users still
/// get a working agent.
#[test]
fn build_agent_auto_returns_none_for_anthropic_main() {
    let mut embed_cfg = EmbedConfig::default();
    embed_cfg.provider = "agent-auto".into();
    let mut agent = crate::config::AgentConfig::default();
    agent.provider = "anthropic".into();
    assert!(build_from_with_agent(&embed_cfg, &agent).unwrap().is_none());
}

/// Explicit `provider = "none"` always wins over agent auto-derive:
/// users who want embeddings off get them off.
#[test]
fn build_explicit_none_still_wins_over_main() {
    let mut embed_cfg = EmbedConfig::default();
    embed_cfg.provider = "none".into();
    let mut agent = crate::config::AgentConfig::default();
    agent.provider = "openai".into();
    agent.base_url = Some("https://api.openai.com/v1".into());
    assert!(build_from_with_agent(&embed_cfg, &agent).unwrap().is_none());
}

/// Auto-derive against an Azure agent assembles the deployment
/// path on demand (when base_url is the resource root rather
/// than a full deployment URL).
#[test]
fn azure_endpoint_assembled_from_resource_root() {
    let mut c = EmbedConfig::default();
    c.provider = "azure".into();
    c.model = "text-embedding-3-small".into();
    c.base_url =
        Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview".into());
    let e = OpenAICompatEmbedder::from_config(&c);
    assert_eq!(
        e.endpoint(),
        "https://acme.openai.azure.com/openai/deployments/text-embedding-3-small/embeddings?api-version=2024-12-01-preview"
    );
}

#[test]
fn is_configured_for_ollama_without_key() {
    let mut c = EmbedConfig::default();
    c.provider = "ollama".into();
    let e = OpenAICompatEmbedder::from_config(&c);
    assert!(e.is_configured());
}

#[test]
fn is_configured_false_without_key_for_openai() {
    let mut c = EmbedConfig::default();
    c.provider = "openai".into();
    let e = OpenAICompatEmbedder::from_config(&c);
    assert!(!e.is_configured());
}

#[test]
fn classify_http_error_maps_codes() {
    let auth = classify_http_error(401, b"{}");
    assert!(matches!(auth, EmbedError::Auth));
    let rate = classify_http_error(429, b"{}");
    assert!(matches!(rate, EmbedError::RateLimited { .. }));
    let prov = classify_http_error(500, br#"{"error":{"message":"the model is overloaded"}}"#);
    if let EmbedError::Provider { status, message } = prov {
        assert_eq!(status, 500);
        assert!(message.contains("overloaded"));
    } else {
        panic!("expected Provider error");
    }
}

// ----- inline TCP-listener end-to-end ---------------------------------

async fn spawn_one_shot_mock(
    response_body: String,
    status_line: &'static str,
) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}/v1");
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 16 * 1024];
        let mut total = Vec::new();
        // Read until headers complete then enough bytes for the body.
        // A shallow read loop is fine — request bodies are small here.
        loop {
            let n = sock.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            total.extend_from_slice(&buf[..n]);
            if total.windows(4).any(|w| w == b"\r\n\r\n") {
                // Need to also consume the body. Find the
                // Content-Length to know when we have it all.
                let head = String::from_utf8_lossy(&total);
                let body_start = total.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                let cl = head
                    .lines()
                    .find_map(|l| {
                        let l = l.to_ascii_lowercase();
                        l.strip_prefix("content-length:")
                            .map(|s| s.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if total.len() - body_start >= cl {
                    break;
                }
            }
        }
        let body = response_body.as_bytes();
        let response = format!(
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.write_all(body).await;
        let _ = sock.shutdown().await;
        total
    });
    (url, handle)
}

#[tokio::test]
async fn end_to_end_embed_round_trip() {
    std::env::set_var("COS_TEST_EMBED_KEY", "sk-test-embed");
    let body = serde_json::json!({
        "object": "list",
        "model": "text-embedding-3-small",
        "data": [
            {"object": "embedding", "index": 0, "embedding": [0.1, 0.2, 0.3]},
            {"object": "embedding", "index": 1, "embedding": [0.4, 0.5, 0.6]},
        ],
        "usage": {"prompt_tokens": 4, "total_tokens": 4}
    })
    .to_string();
    let (base_url, handle) = spawn_one_shot_mock(body, "HTTP/1.1 200 OK").await;
    let mut c = EmbedConfig::default();
    c.provider = "openai".into();
    c.model = "text-embedding-3-small".into();
    c.base_url = Some(base_url);
    c.api_key_env = Some("COS_TEST_EMBED_KEY".into());

    let embedder = OpenAICompatEmbedder::from_config(&c);
    let resp = embedder
        .embed(EmbedRequest {
            inputs: vec!["alpha".into(), "beta".into()],
        })
        .await
        .expect("embed");
    assert_eq!(resp.embeddings.len(), 2);
    assert_eq!(resp.dim, 3);
    assert_eq!(resp.embeddings[0], vec![0.1, 0.2, 0.3]);
    assert_eq!(resp.usage.prompt_tokens, 4);

    // Verify request-side
    let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
    assert!(req.contains("post /v1/embeddings"));
    assert!(req.contains("authorization: bearer sk-test-embed"));
    assert!(req.contains("\"model\":\"text-embedding-3-small\""));
    assert!(req.contains("alpha"));

    std::env::remove_var("COS_TEST_EMBED_KEY");
}

#[tokio::test]
async fn end_to_end_embed_401_maps_to_auth() {
    let body = r#"{"error":{"message":"bad key"}}"#.to_string();
    let (base_url, _h) = spawn_one_shot_mock(body, "HTTP/1.1 401 Unauthorized").await;
    let mut c = EmbedConfig::default();
    c.provider = "openai".into();
    c.base_url = Some(base_url);
    c.api_key_env = Some("COS_NONEXISTENT_KEY_1933".into());
    let embedder = OpenAICompatEmbedder::from_config(&c);
    let err = embedder
        .embed(EmbedRequest {
            inputs: vec!["x".into()],
        })
        .await
        .unwrap_err();
    assert!(matches!(err, EmbedError::Auth));
}

#[tokio::test]
async fn azure_alias_sends_api_key_header_not_bearer() {
    std::env::set_var("COS_TEST_AZURE_KEY", "azure-secret-1234");
    let body = serde_json::json!({
        "object": "list",
        "model": "text-embedding-3-small",
        "data": [{"object": "embedding", "index": 0, "embedding": [0.7, 0.8]}],
        "usage": {"prompt_tokens": 1, "total_tokens": 1}
    })
    .to_string();
    let (base_url, handle) = spawn_one_shot_mock(body, "HTTP/1.1 200 OK").await;
    let mut c = EmbedConfig::default();
    c.provider = "azure".into();
    c.model = "text-embedding-3-small".into();
    c.base_url = Some(base_url);
    c.api_key_env = Some("COS_TEST_AZURE_KEY".into());

    let e = OpenAICompatEmbedder::from_config(&c);
    let resp = e
        .embed(EmbedRequest {
            inputs: vec!["hello".into()],
        })
        .await
        .expect("embed");
    assert_eq!(resp.dim, 2);

    let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
    assert!(
        req.contains("api-key: azure-secret-1234"),
        "expected `api-key:` header for azure alias, got:\n{req}"
    );
    assert!(
        !req.contains("authorization: bearer"),
        "azure alias must NOT send bearer auth, got:\n{req}"
    );

    std::env::remove_var("COS_TEST_AZURE_KEY");
}

#[test]
fn azure_endpoint_assembly_preserves_api_version_query() {
    let mut c = EmbedConfig::default();
    c.provider = "azure".into();
    c.model = "text-embedding-3-small".into();
    c.base_url = Some(
        "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/text-embedding-3-small?api-version=2024-02-01".into(),
    );
    let e = OpenAICompatEmbedder::from_config(&c);
    assert_eq!(
        e.endpoint(),
        "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/text-embedding-3-small/embeddings?api-version=2024-02-01"
    );
}

#[test]
fn azure_provider_is_accepted_by_factory() {
    let mut c = EmbedConfig::default();
    c.provider = "azure".into();
    c.model = "text-embedding-3-small".into();
    assert!(build_from(&c).unwrap().is_some());
}

#[test]
fn cfg_model_overrides_default_for_openai_alias() {
    // `text-embedding-3-large` is a real OpenAI model with
    // different dimensionality — verify it round-trips.
    let mut c = cfg();
    c.model = "text-embedding-3-large".into();
    let e = OpenAICompatEmbedder::from_config(&c);
    assert_eq!(e.model, "text-embedding-3-large");
    assert_eq!(e.model(), "text-embedding-3-large");
}

#[test]
fn cfg_model_overrides_default_for_azure_alias() {
    let mut c = cfg();
    c.model = "ada-002-deployment".into();
    c.provider = "azure".into();
    let e = OpenAICompatEmbedder::from_config(&c);
    assert_eq!(e.model, "ada-002-deployment");
}

#[test]
fn cfg_model_empty_falls_back_to_default() {
    let mut c = cfg();
    c.model = "".into();
    let e = OpenAICompatEmbedder::from_config(&c);
    assert_eq!(e.model, MODEL_NAME);
    assert_eq!(MODEL_NAME, "text-embedding-3-small");
}

#[test]
fn cfg_model_whitespace_is_treated_as_empty() {
    let mut c = cfg();
    c.model = "   ".into();
    let e = OpenAICompatEmbedder::from_config(&c);
    assert_eq!(e.model, MODEL_NAME);
}
