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
    let config = crate::config::current_snapshot();
    build_from(&config.imagegen)
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

        let api_key = crate::agent::llm::construction::resolve_process_api_key(
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
                obj.insert(
                    "model".into(),
                    serde_json::Value::String(self.model.clone()),
                );
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
                    ImageData::Base64 {
                        data: String::new(),
                    }
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/model/tasks/imagegen.rs"
    ));
}
