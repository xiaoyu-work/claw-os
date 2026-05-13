//! Embedding task — vector representations of text.
//!
//! Phase 1.5: cloud OpenAI-compatible backend (OpenAI / Azure OpenAI /
//! self-hosted vLLM / Ollama via `/embeddings` endpoint shape).
//!
//! Phase 0.5 originally scoped local ONNX (BGE / MiniLM / nomic-embed)
//! but local engines wait for the user to supply ONNX files. The cloud
//! path gives the agent an immediately usable embed surface.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::EmbedConfig;

/// Default embedding model name. Used when [`crate::config::EmbedConfig::model`]
/// is left empty; an explicit `model = "..."` in `[embed]` overrides this.
///
/// **Switching models invalidates every existing row in `semantic.db`.**
/// Vector spaces are model-specific — cosine similarity between vectors
/// from two different models is meaningless, and dimensionality usually
/// differs (`text-embedding-3-small` is 1536, `-large` is 3072).
/// `SemanticStore` enforces this with a stickiness check that returns
/// `ModelMismatch` on the first row from a new model. To migrate, run
/// `cos agent semantic clear-all --yes` and re-index.
pub const MODEL_NAME: &str = "text-embedding-3-small";

/// One embedding request — a batch of inputs to embed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedRequest {
    pub inputs: Vec<String>,
}

/// Result of an embedding call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedResponse {
    pub embeddings: Vec<Vec<f32>>,
    pub model: String,
    pub dim: usize,
    pub usage: EmbedUsage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedUsage {
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("not configured: set [embed] block in config.json")]
    NotConfigured,
    #[error("authentication failed: bad or missing API key")]
    Auth,
    #[error("rate limited (retry after {retry_after_ms}ms)")]
    RateLimited { retry_after_ms: u64 },
    #[error("provider returned error: {status} — {message}")]
    Provider { status: u16, message: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[async_trait]
pub trait Embedder: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn is_configured(&self) -> bool;
    async fn embed(&self, request: EmbedRequest) -> Result<EmbedResponse, EmbedError>;
}

// =====================================================================
// Factory
// =====================================================================

/// Build the configured embedder, if any. Returns `Ok(None)` when
/// embedding is disabled (provider="none"). Returns an error if the
/// config block names a provider that does not exist.
pub fn build_default() -> Result<Option<Box<dyn Embedder>>, String> {
    let cfg = &crate::config::get().embed;
    build_from(cfg)
}

pub fn build_from(cfg: &EmbedConfig) -> Result<Option<Box<dyn Embedder>>, String> {
    match cfg.provider.as_str() {
        "none" | "" => Ok(None),
        // Every OpenAI-API-shape backend (OpenAI / Azure / Ollama / vLLM /
        // TGI / LMStudio) shares one impl — switch behaviour via base_url.
        // `azure` differs only in the auth header style (`api-key:` instead
        // of `Authorization: Bearer …`).
        "openai" | "azure" | "xai" | "deepseek" | "openrouter" | "ollama" => {
            Ok(Some(Box::new(OpenAICompatEmbedder::from_config(cfg))))
        }
        // Local Qwen3-Embedding-0.6B via onnxruntime-genai. Reads the
        // model directory from `cfg.model_dir` (or the default registry
        // slot). The model is loaded lazily on first call.
        "qwen3-local" | "local" => Ok(Some(Box::new(super::qwen3_genai::build_from_config(cfg)))),
        other => Err(format!("unknown embed provider: {other}")),
    }
}

// =====================================================================
// OpenAI-compatible cloud embedder
// =====================================================================

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_OLLAMA_BASE: &str = "http://localhost:11434/v1";

fn default_base_url_for(alias: &str) -> &'static str {
    match alias {
        "ollama" => DEFAULT_OLLAMA_BASE,
        // xAI / DeepSeek / OpenRouter generally don't run their own embed
        // models — fall through to a sane OpenAI default that the user
        // will almost always override via base_url.
        _ => DEFAULT_OPENAI_BASE,
    }
}

fn alias_is_local_default(alias: &str) -> bool {
    matches!(alias, "ollama")
}

pub struct OpenAICompatEmbedder {
    alias: String,
    base_url: String,
    api_key: Option<String>,
    model: String,
    extra_headers: HashMap<String, String>,
    client: reqwest::Client,
}

impl OpenAICompatEmbedder {
    pub fn from_config(cfg: &EmbedConfig) -> Self {
        let alias = cfg.provider.clone();
        let base_url = cfg
            .base_url
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_base_url_for(&alias).to_string());
        let base_url = base_url.trim_end_matches('/').to_string();

        let api_key = crate::agent::llm::providers::openai_compat::resolve_api_key(
            cfg.api_key_credential.as_deref(),
            cfg.api_key_env.as_deref(),
        )
        .ok()
        .flatten();

        let timeout = if cfg.request_timeout == 0 {
            Duration::from_secs(0)
        } else {
            Duration::from_secs(cfg.request_timeout)
        };
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            alias,
            base_url,
            api_key,
            // Honor cfg.model when set; fall back to MODEL_NAME if empty.
            // Switching this value invalidates the existing semantic.db —
            // see MODEL_NAME doc for the migration story (`semantic clear-all`).
            model: if cfg.model.trim().is_empty() {
                MODEL_NAME.to_string()
            } else {
                cfg.model.clone()
            },
            extra_headers: cfg.extra_headers.clone(),
            client,
        }
    }

    fn endpoint(&self) -> String {
        // Preserve any query string the user attached to base_url
        // (Azure OpenAI requires `?api-version=...`).
        let (base, query) = match self.base_url.split_once('?') {
            Some((b, q)) => (b.trim_end_matches('/'), Some(q)),
            None => (self.base_url.as_str(), None),
        };
        match query {
            Some(q) => format!("{base}/embeddings?{q}"),
            None => format!("{base}/embeddings"),
        }
    }
}

#[async_trait]
impl Embedder for OpenAICompatEmbedder {
    fn name(&self) -> &str {
        &self.alias
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn is_configured(&self) -> bool {
        self.api_key.is_some() || alias_is_local_default(&self.alias)
    }

    async fn embed(&self, request: EmbedRequest) -> Result<EmbedResponse, EmbedError> {
        if request.inputs.is_empty() {
            return Err(EmbedError::InvalidInput("inputs must not be empty".into()));
        }
        // OpenAI embeddings accept either a string or an array of strings;
        // pass the array shape always — works for all backends.
        let body = serde_json::json!({
            "model": self.model,
            "input": request.inputs,
        });

        let mut http = self
            .client
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(key) = &self.api_key {
            // Azure OpenAI authenticates with `api-key: <key>` rather than
            // `Authorization: Bearer …`. Other OpenAI-shape providers all
            // accept the bearer form.
            if self.alias == "azure" {
                http = http.header("api-key", key.as_str());
            } else {
                http = http.bearer_auth(key);
            }
        }
        for (k, v) in &self.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }
        let resp = http
            .send()
            .await
            .map_err(|e| EmbedError::Transport(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| EmbedError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(classify_http_error(status.as_u16(), &bytes));
        }
        let parsed: WireEmbedResponse =
            serde_json::from_slice(&bytes).map_err(|e| EmbedError::Parse(e.to_string()))?;
        // OpenAI returns data sorted by the index field — but be defensive
        // and re-sort to keep the order matching the request.
        let mut data = parsed.data;
        data.sort_by_key(|r| r.index);
        let embeddings: Vec<Vec<f32>> = data.into_iter().map(|r| r.embedding).collect();
        if embeddings.len() != request.inputs.len() {
            return Err(EmbedError::Parse(format!(
                "expected {} embeddings, got {}",
                request.inputs.len(),
                embeddings.len()
            )));
        }
        let dim = embeddings.first().map(|v| v.len()).unwrap_or(0);
        Ok(EmbedResponse {
            embeddings,
            model: parsed.model.unwrap_or_else(|| self.model.clone()),
            dim,
            usage: EmbedUsage {
                prompt_tokens: parsed.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                total_tokens: parsed.usage.as_ref().map(|u| u.total_tokens).unwrap_or(0),
            },
        })
    }
}

#[derive(Debug, Deserialize)]
struct WireEmbedResponse {
    data: Vec<WireEmbedDatum>,
    model: Option<String>,
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct WireEmbedDatum {
    embedding: Vec<f32>,
    #[serde(default)]
    index: u32,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

fn classify_http_error(status: u16, bytes: &[u8]) -> EmbedError {
    let message = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).chars().take(400).collect());
    match status {
        401 | 403 => EmbedError::Auth,
        429 => EmbedError::RateLimited {
            retry_after_ms: 1000,
        },
        _ => EmbedError::Provider { status, message },
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
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
}
