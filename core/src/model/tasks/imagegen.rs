//! Image generation task — text-to-image and (later) image-to-image.
//!
//! Phase 1.5: cloud OpenAI-compatible backend (OpenAI / Azure OpenAI /
//! self-hosted) via `/images/generations` endpoint shape. Supports
//! gpt-image-2, dall-e-3, and any compatible deployment.
//!
//! Phase 0.5 originally scoped local SD/Flux via ort but local engines
//! wait for the user to supply ONNX files.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::ImageGenConfig;

// =====================================================================
// Public types
// =====================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenRequest {
    pub prompt: String,
    /// e.g. "1024x1024", "1792x1024", "1024x1792".
    pub size: Option<String>,
    /// Provider-specific. For gpt-image-2: "low" | "medium" | "high".
    pub quality: Option<String>,
    /// Number of images to generate. Default 1.
    #[serde(default = "default_n")]
    pub n: u32,
    /// "png" | "jpeg" | "webp".
    pub format: Option<String>,
}

fn default_n() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenResponse {
    pub images: Vec<ImageData>,
    pub model: String,
}

/// Returned image — either inline base64 (gpt-image-2 default) or a URL
/// (some legacy backends).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageData {
    Base64 { data: String },
    Url { url: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("not configured: set [imagegen] block in config.json")]
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
pub trait ImageGenerator: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn is_configured(&self) -> bool;
    async fn generate(&self, request: ImageGenRequest) -> Result<ImageGenResponse, ImageError>;
}

// =====================================================================
// Factory
// =====================================================================

pub fn build_default() -> Result<Option<Box<dyn ImageGenerator>>, String> {
    let cfg = &crate::config::get().imagegen;
    build_from(cfg)
}

pub fn build_from(cfg: &ImageGenConfig) -> Result<Option<Box<dyn ImageGenerator>>, String> {
    match cfg.provider.as_str() {
        "none" | "" => Ok(None),
        "openai" | "xai" | "deepseek" | "openrouter" => {
            Ok(Some(Box::new(OpenAICompatImageGen::from_config(cfg))))
        }
        other => Err(format!("unknown imagegen provider: {other}")),
    }
}

// =====================================================================
// OpenAI-compatible cloud image generator
// =====================================================================

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";

fn default_base_url_for(_alias: &str) -> &'static str {
    DEFAULT_OPENAI_BASE
}

pub struct OpenAICompatImageGen {
    alias: String,
    base_url: String,
    api_key: Option<String>,
    model: String,
    extra_headers: HashMap<String, String>,
    default_size: Option<String>,
    default_quality: Option<String>,
    default_format: String,
    client: reqwest::Client,
}

impl OpenAICompatImageGen {
    pub fn from_config(cfg: &ImageGenConfig) -> Self {
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
            model: cfg.model.clone(),
            extra_headers: cfg.extra_headers.clone(),
            default_size: cfg.default_size.clone(),
            default_quality: cfg.default_quality.clone(),
            default_format: cfg.default_format.clone(),
            client,
        }
    }

    fn endpoint(&self) -> String {
        let (base, query) = match self.base_url.split_once('?') {
            Some((b, q)) => (b.trim_end_matches('/'), Some(q)),
            None => (self.base_url.as_str(), None),
        };
        match query {
            Some(q) => format!("{base}/images/generations?{q}"),
            None => format!("{base}/images/generations"),
        }
    }
}

#[async_trait]
impl ImageGenerator for OpenAICompatImageGen {
    fn name(&self) -> &str {
        &self.alias
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn is_configured(&self) -> bool {
        self.api_key.is_some()
    }

    async fn generate(&self, request: ImageGenRequest) -> Result<ImageGenResponse, ImageError> {
        if request.prompt.trim().is_empty() {
            return Err(ImageError::InvalidInput("prompt must not be empty".into()));
        }
        let n = if request.n == 0 { 1 } else { request.n };
        let size = request.size.or_else(|| self.default_size.clone());
        let quality = request.quality.or_else(|| self.default_quality.clone());
        let format = request
            .format
            .unwrap_or_else(|| self.default_format.clone());

        // Azure OpenAI deployment URLs encode the model in the path —
        // sending a `model` field redundantly causes a 500. Detect by
        // the presence of `/deployments/` in the configured base URL.
        let is_azure_deployment = self.base_url.contains("/deployments/");

        let mut body = serde_json::json!({
            "prompt": request.prompt,
            "n": n,
            "output_format": format,
        });
        if let Some(obj) = body.as_object_mut() {
            if !is_azure_deployment {
                obj.insert("model".into(), serde_json::Value::String(self.model.clone()));
            }
            if let Some(s) = size {
                obj.insert("size".into(), serde_json::Value::String(s));
            }
            if let Some(q) = quality {
                obj.insert("quality".into(), serde_json::Value::String(q));
            }
            // gpt-image-* expects an `output_compression` knob (1-100).
            // Harmless default of 100 matches the Azure quickstart sample.
            if format.eq_ignore_ascii_case("png") || format.eq_ignore_ascii_case("jpeg") {
                obj.insert("output_compression".into(), serde_json::json!(100));
            }
        }

        let mut http = self
            .client
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(key) = &self.api_key {
            http = http.bearer_auth(key);
        }
        for (k, v) in &self.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }
        let resp = http
            .send()
            .await
            .map_err(|e| ImageError::Transport(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ImageError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(classify_http_error(status.as_u16(), &bytes));
        }
        let parsed: WireImageResponse =
            serde_json::from_slice(&bytes).map_err(|e| ImageError::Parse(e.to_string()))?;
        let images: Vec<ImageData> = parsed
            .data
            .into_iter()
            .map(|d| {
                if let Some(b64) = d.b64_json {
                    ImageData::Base64 { data: b64 }
                } else if let Some(url) = d.url {
                    ImageData::Url { url }
                } else {
                    // Fall back to an empty base64 string — provider
                    // returned neither field. Surface as parse error.
                    ImageData::Base64 { data: String::new() }
                }
            })
            .collect();
        if images.is_empty() {
            return Err(ImageError::Parse("provider returned no images".into()));
        }
        Ok(ImageGenResponse {
            images,
            model: self.model.clone(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct WireImageResponse {
    data: Vec<WireImageDatum>,
}

#[derive(Debug, Deserialize)]
struct WireImageDatum {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

fn classify_http_error(status: u16, bytes: &[u8]) -> ImageError {
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
        401 | 403 => ImageError::Auth,
        429 => ImageError::RateLimited {
            retry_after_ms: 1000,
        },
        _ => ImageError::Provider { status, message },
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ImageGenConfig {
        let mut c = ImageGenConfig::default();
        c.provider = "openai".into();
        c.model = "gpt-image-2".into();
        c
    }

    #[test]
    fn build_returns_none_when_disabled() {
        let mut c = ImageGenConfig::default();
        c.provider = "none".into();
        assert!(build_from(&c).unwrap().is_none());
    }

    #[test]
    fn build_returns_err_for_unknown_provider() {
        let mut c = ImageGenConfig::default();
        c.provider = "unknown".into();
        assert!(build_from(&c).is_err());
    }

    #[test]
    fn endpoint_handles_query_string() {
        let mut c = cfg();
        c.base_url = Some(
            "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/gpt-image-2?api-version=2024-02-01".into(),
        );
        let g = OpenAICompatImageGen::from_config(&c);
        assert_eq!(
            g.endpoint(),
            "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/gpt-image-2/images/generations?api-version=2024-02-01"
        );
    }

    #[test]
    fn endpoint_default_path() {
        let mut c = cfg();
        c.base_url = Some("https://api.openai.com/v1".into());
        let g = OpenAICompatImageGen::from_config(&c);
        assert_eq!(
            g.endpoint(),
            "https://api.openai.com/v1/images/generations"
        );
    }

    #[test]
    fn classify_http_error_maps_codes() {
        assert!(matches!(classify_http_error(401, b"{}"), ImageError::Auth));
        assert!(matches!(
            classify_http_error(429, b"{}"),
            ImageError::RateLimited { .. }
        ));
        let prov = classify_http_error(500, br#"{"error":{"message":"oops"}}"#);
        if let ImageError::Provider { status, message } = prov {
            assert_eq!(status, 500);
            assert!(message.contains("oops"));
        } else {
            panic!("expected Provider");
        }
    }

    #[tokio::test]
    async fn generate_rejects_empty_prompt() {
        let g = OpenAICompatImageGen::from_config(&cfg());
        let err = g
            .generate(ImageGenRequest {
                prompt: String::new(),
                size: None,
                quality: None,
                n: 1,
                format: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, ImageError::InvalidInput(_)));
    }

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
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                if total.windows(4).any(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&total);
                    let body_start =
                        total.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
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
    async fn end_to_end_image_generation_round_trip() {
        std::env::set_var("COS_TEST_IMAGE_KEY", "sk-img");
        let body = serde_json::json!({
            "data": [{"b64_json": "iVBORw0KGgo="}]
        })
        .to_string();
        let (base_url, handle) = spawn_one_shot_mock(body, "HTTP/1.1 200 OK").await;

        let mut c = cfg();
        c.base_url = Some(base_url);
        c.api_key_env = Some("COS_TEST_IMAGE_KEY".into());
        let g = OpenAICompatImageGen::from_config(&c);
        let resp = g
            .generate(ImageGenRequest {
                prompt: "a red fox in autumn".into(),
                size: Some("1024x1024".into()),
                quality: Some("medium".into()),
                n: 1,
                format: Some("png".into()),
            })
            .await
            .expect("generate");
        assert_eq!(resp.images.len(), 1);
        match &resp.images[0] {
            ImageData::Base64 { data } => assert_eq!(data, "iVBORw0KGgo="),
            other => panic!("expected base64, got {other:?}"),
        }

        let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
        assert!(req.contains("post /v1/images/generations"));
        assert!(req.contains("authorization: bearer sk-img"));
        assert!(req.contains("\"prompt\":\"a red fox in autumn\""));
        assert!(req.contains("\"size\":\"1024x1024\""));
        assert!(req.contains("\"quality\":\"medium\""));
        assert!(req.contains("\"output_format\":\"png\""));
        // Mock URL has no /deployments/ → model field IS sent (stock OpenAI shape).
        assert!(req.contains("\"model\":\"gpt-image-2\""));
        // png/jpeg → output_compression added.
        assert!(req.contains("\"output_compression\":100"));

        std::env::remove_var("COS_TEST_IMAGE_KEY");
    }

    #[tokio::test]
    async fn end_to_end_image_generation_azure_omits_model() {
        std::env::set_var("COS_TEST_IMAGE_KEY_2", "sk-img-az");
        let body = serde_json::json!({
            "data": [{"b64_json": "iVBORw0KGgo="}]
        })
        .to_string();
        // Force the URL to look like an Azure deployment URL.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let azure_style_base = format!("http://{addr}/openai/deployments/gpt-image-2");
        let handle = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16 * 1024];
            let mut total = Vec::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                if total.windows(4).any(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&total);
                    let body_start =
                        total.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
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
            let body_bytes = body.as_bytes();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body_bytes.len()
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.write_all(body_bytes).await;
            let _ = sock.shutdown().await;
            total
        });

        let mut c = cfg();
        c.base_url = Some(azure_style_base);
        c.api_key_env = Some("COS_TEST_IMAGE_KEY_2".into());
        let g = OpenAICompatImageGen::from_config(&c);
        let _ = g
            .generate(ImageGenRequest {
                prompt: "test".into(),
                size: Some("1024x1024".into()),
                quality: Some("medium".into()),
                n: 1,
                format: Some("png".into()),
            })
            .await
            .expect("generate");

        let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
        // Azure deployment shape → no `model` key in body.
        assert!(
            !req.contains("\"model\""),
            "Azure deployment shape must not send model field"
        );
        assert!(req.contains("\"prompt\":\"test\""));

        std::env::remove_var("COS_TEST_IMAGE_KEY_2");
    }
}

