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
    client: reqwest::Client,
}

impl OpenAiImageGenProvider {
    pub fn new(cfg: OpenAiImageGenConfig) -> Self {
        let mut builder = reqwest::Client::builder().user_agent(concat!(
            "cos-agent/",
            env!("CARGO_PKG_VERSION")
        ));
        if cfg.request_timeout > Duration::from_secs(0) {
            builder = builder.timeout(cfg.request_timeout);
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        Self { cfg, client }
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

        let mut http = self
            .client
            .post(self.endpoint())
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
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| MediaError::Transport(e.to_string()))?;

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
    if text.len() > 512 {
        format!("{}…", &text[..512])
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;

    fn fake_png() -> Vec<u8> {
        // Single PNG signature byte block — encode/decode round-trip is what we test.
        b"\x89PNG\r\n\x1a\n".to_vec()
    }

    #[test]
    fn default_base_url_for_known_aliases() {
        assert_eq!(default_base_url_for("openai"), DEFAULT_OPENAI_BASE);
        assert_eq!(default_base_url_for("xai"), DEFAULT_XAI_BASE);
        assert_eq!(default_base_url_for("custom"), DEFAULT_OPENAI_BASE);
        assert_eq!(default_base_url_for("unknown"), DEFAULT_OPENAI_BASE);
    }

    #[test]
    fn for_alias_pulls_default_base_url() {
        let c = OpenAiImageGenConfig::for_alias("xai", "grok-2-image");
        assert_eq!(c.base_url, DEFAULT_XAI_BASE);
        assert_eq!(c.model, "grok-2-image");
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let mut c = OpenAiImageGenConfig::for_alias("openai", "gpt-image-1");
        c.base_url = "https://api.openai.com/v1/".to_string();
        let p = OpenAiImageGenProvider::new(c);
        assert_eq!(p.endpoint(), "https://api.openai.com/v1/images/generations");
    }

    #[test]
    fn name_reflects_alias() {
        let p = OpenAiImageGenProvider::new(OpenAiImageGenConfig::for_alias(
            "openai",
            "dall-e-3",
        ));
        assert_eq!(<OpenAiImageGenProvider as ImageGenProvider>::name(&p), "openai");
    }

    #[test]
    fn is_configured_requires_api_key() {
        let mut c = OpenAiImageGenConfig::for_alias("openai", "dall-e-3");
        let p1 = OpenAiImageGenProvider::new(c.clone());
        assert!(!<OpenAiImageGenProvider as ImageGenProvider>::is_configured(&p1));
        c.api_key = Some("sk".to_string());
        let p2 = OpenAiImageGenProvider::new(c);
        assert!(<OpenAiImageGenProvider as ImageGenProvider>::is_configured(&p2));
    }

    #[tokio::test]
    async fn generate_without_key_errors_not_configured() {
        let p = OpenAiImageGenProvider::new(OpenAiImageGenConfig::for_alias(
            "openai",
            "dall-e-3",
        ));
        let err = p.generate(ImageGenRequest::new("a cat")).await.unwrap_err();
        assert!(matches!(err, MediaError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn generate_validates_request() {
        let mut c = OpenAiImageGenConfig::for_alias("openai", "dall-e-3");
        c.api_key = Some("sk".to_string());
        let p = OpenAiImageGenProvider::new(c);
        let err = p.generate(ImageGenRequest::new("")).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[test]
    fn derive_size_only_when_both_present() {
        assert_eq!(derive_size(Some(1024), Some(1024)).as_deref(), Some("1024x1024"));
        assert!(derive_size(Some(1024), None).is_none());
        assert!(derive_size(None, Some(1024)).is_none());
        assert!(derive_size(None, None).is_none());
    }

    #[test]
    fn parse_response_decodes_b64_json() {
        let raw = fake_png();
        let b64 = B64.encode(&raw);
        let body = format!(r#"{{"data":[{{"b64_json":"{b64}"}}]}}"#);
        let r = parse_response(body.as_bytes(), "dall-e-3").unwrap();
        assert_eq!(r.images.len(), 1);
        assert_eq!(r.images[0].bytes, raw);
        assert_eq!(r.images[0].format, ImageFormat::Png);
        assert_eq!(r.model.as_deref(), Some("dall-e-3"));
    }

    #[test]
    fn parse_response_url_only_entry_errors() {
        let body = br#"{"data":[{"url":"https://cdn.example.com/x.png"}]}"#;
        let err = parse_response(body, "dall-e-3").unwrap_err();
        assert!(matches!(err, MediaError::Parse(_)));
    }

    #[test]
    fn parse_response_invalid_base64_errors() {
        let body = br#"{"data":[{"b64_json":"!!!not-base64!!!"}]}"#;
        let err = parse_response(body, "dall-e-3").unwrap_err();
        assert!(matches!(err, MediaError::Parse(_)));
    }

    #[test]
    fn parse_response_garbage_errors() {
        let err = parse_response(b"oops", "dall-e-3").unwrap_err();
        assert!(matches!(err, MediaError::Parse(_)));
    }

    #[test]
    fn wire_request_serialises_with_size() {
        let body = WireRequest {
            model: "dall-e-3",
            prompt: "cat",
            n: 1,
            size: Some("1024x1024".to_string()),
            response_format: "b64_json",
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["size"], "1024x1024");
        assert_eq!(json["response_format"], "b64_json");
    }

    #[test]
    fn wire_request_omits_size_when_none() {
        let body = WireRequest {
            model: "dall-e-3",
            prompt: "cat",
            n: 1,
            size: None,
            response_format: "b64_json",
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json.get("size").is_none());
    }

    #[test]
    fn provider_aliases_listed() {
        for a in ["openai", "xai", "custom"] {
            assert!(PROVIDER_ALIASES.contains(&a));
        }
    }
}
