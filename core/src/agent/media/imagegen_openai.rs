//! OpenAI image generation provider — covers
//! `POST /v1/images/generations` for `dall-e-3`, `dall-e-2`,
//! `gpt-image-1`, and any OpenAI-compatible endpoint exposing the
//! same shape (xAI Grok image gen, custom gateways).
//!
//! Wire format:
//!   POST <base>/images/generations
//!   Authorization: Bearer <key>
//!   {
//!     "model":           "dall-e-3" | "gpt-image-1" | ...,
//!     "prompt":          "...",
//!     "n":               1..16,
//!     "size":            "1024x1024" (derived from width × height),
//!     "response_format": "b64_json"  (always — keeps the bytes
//!                                     in our hands; URL form would
//!                                     leak through ephemeral CDN
//!                                     links that expire fast)
//!   }
//!   ->
//!   { "data": [{ "b64_json": "..." }, ...] }
//!
//! gpt-image-1 ignores `response_format` and always returns
//! `b64_json`, so the field is harmless. dall-e-3 accepts only
//! `n=1`; we surface the upstream 4xx unmodified.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};

use super::imagegen::{
    GeneratedImage, ImageFormat, ImageGenProvider, ImageGenRequest, ImageGenResponse,
};
use super::MediaError;

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_XAI_BASE: &str = "https://api.x.ai/v1";

pub const PROVIDER_ALIASES: &[&str] = &["openai", "xai", "custom"];

pub fn default_base_url_for(alias: &str) -> &'static str {
    match alias {
        "xai" => DEFAULT_XAI_BASE,
        _ => DEFAULT_OPENAI_BASE,
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiImageGenConfig {
    pub alias: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
}

impl OpenAiImageGenConfig {
    pub fn for_alias(alias: &str, model: impl Into<String>) -> Self {
        Self {
            alias: alias.to_string(),
            base_url: default_base_url_for(alias).to_string(),
            api_key: None,
            model: model.into(),
            extra_headers: HashMap::new(),
            request_timeout: Duration::from_secs(120),
        }
    }
}

pub struct OpenAiImageGenProvider {
    cfg: OpenAiImageGenConfig,
}

impl OpenAiImageGenProvider {
    pub fn new(cfg: OpenAiImageGenConfig) -> Self {
        // Per-request safe client; see `media/util.rs`.
        Self { cfg }
    }

    fn endpoint(&self) -> String {
        let base = self.cfg.base_url.trim_end_matches('/');
        format!("{base}/images/generations")
    }
}

#[derive(Debug, Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    n: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
    response_format: &'a str,
}

#[derive(Debug, Deserialize)]
struct WireImage {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    data: Vec<WireImage>,
}

/// `1024x1024` style size string for the OpenAI image API. Only
/// emitted when both width and height are set — leaving both
/// unset lets the model use its default. If only one is set we
/// also drop the field so the model sees a single explicit value
/// rather than an asymmetric guess.
pub fn derive_size(width: Option<u32>, height: Option<u32>) -> Option<String> {
    match (width, height) {
        (Some(w), Some(h)) => Some(format!("{w}x{h}")),
        _ => None,
    }
}

pub fn parse_response(bytes: &[u8], model: &str) -> Result<ImageGenResponse, MediaError> {
    let parsed: WireResponse =
        serde_json::from_slice(bytes).map_err(|e| MediaError::Parse(e.to_string()))?;
    let mut images = Vec::with_capacity(parsed.data.len());
    for d in parsed.data {
        let b64 = d.b64_json.ok_or_else(|| {
            MediaError::Parse(format!(
                "openai image: data entry missing b64_json (url-mode unsupported, got url={:?})",
                d.url
            ))
        })?;
        let raw = BASE64
            .decode(b64.as_bytes())
            .map_err(|e| MediaError::Parse(format!("openai image: base64 decode failed: {e}")))?;
        images.push(GeneratedImage {
            bytes: raw,
            format: ImageFormat::Png,
            width: 0,
            height: 0,
        });
    }
    Ok(ImageGenResponse {
        images,
        model: Some(model.to_string()),
        seed_used: None,
    })
}

#[async_trait]
impl ImageGenProvider for OpenAiImageGenProvider {
    fn name(&self) -> &str {
        self.cfg.alias.as_str()
    }

    fn is_configured(&self) -> bool {
        self.cfg.api_key.is_some()
    }

    async fn generate(&self, request: ImageGenRequest) -> Result<ImageGenResponse, MediaError> {
        request.validate()?;
        if self.cfg.api_key.is_none() {
            return Err(MediaError::NotConfigured(self.cfg.alias.clone()));
        }

        let body = WireRequest {
            model: &self.cfg.model,
            prompt: &request.prompt,
            n: request.n,
            size: derive_size(request.width, request.height),
            response_format: "b64_json",
        };

        let endpoint = self.endpoint();
        let url = reqwest::Url::parse(&endpoint)
            .map_err(|e| MediaError::InvalidRequest(format!("invalid endpoint url: {e}")))?;
        let client = super::util::build_safe_client(&url, self.cfg.request_timeout).await?;
        let mut http = client
            .post(url)
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(key) = &self.cfg.api_key {
            http = http.bearer_auth(key);
        }
        for (k, v) in &self.cfg.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }

        let resp = http
            .send()
            .await
            .map_err(|e| MediaError::Transport(e.to_string()))?;
        let status = resp.status();
        let bytes = super::util::read_bytes_capped(
            resp,
            super::util::MAX_BINARY_BODY_BYTES,
            "imagegen_openai",
        )
        .await?;

        if !status.is_success() {
            let preview = body_preview(&bytes);
            return Err(MediaError::Provider {
                status: status.as_u16(),
                message: preview,
            });
        }

        parse_response(&bytes, &self.cfg.model)
    }
}

fn body_preview(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    super::util::preview(&text, 512)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/media/imagegen_openai.rs"
    ));
}
