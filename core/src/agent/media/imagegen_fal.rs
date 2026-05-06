//! FAL.ai image generation provider.
//!
//! FAL routes by **model id** rather than a single endpoint:
//!   `POST https://fal.run/<owner>/<model>`
//!   Authorization: Key <token>
//!   Content-Type: application/json
//!   { "prompt": ..., "image_size": ..., "num_images": ..., ... }
//!
//! Response shape (sync endpoint):
//!   { "images": [
//!       { "url": "...", "content_type": "image/png",
//!         "width": w, "height": h }, ... ],
//!     "seed": 12345 }
//!
//! Because FAL returns URLs (not bytes), this provider follows
//! each URL with a second GET to materialise the bytes locally.
//! That keeps the [`ImageGenResponse`] contract uniform across
//! cloud backends — callers never have to special-case
//! "download this URL yourself."
//!
//! Per-model knob support is intentionally narrow: prompt,
//! image_size (`{w}x{h}` or named), num_images, seed,
//! negative_prompt, num_inference_steps. Callers who need a
//! model-specific extra field (LoRA scale, scheduler, ...)
//! configure it via [`FalImageGenConfig::extra_payload`] which
//! is merged into the request body verbatim.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::imagegen::{
    GeneratedImage, ImageFormat, ImageGenProvider, ImageGenRequest, ImageGenResponse,
};
use super::MediaError;

const DEFAULT_FAL_BASE: &str = "https://fal.run";

#[derive(Debug, Clone)]
pub struct FalImageGenConfig {
    /// Stable name for the registry. Use a short label like
    /// `fal-flux-dev` so the runtime can tell which FAL model
    /// the registry entry is bound to.
    pub alias: String,
    /// `https://fal.run` (default) — override only for staging
    /// or self-hosted FAL replicas.
    pub base_url: String,
    pub api_key: Option<String>,
    /// FAL model id, e.g. `fal-ai/flux/dev` or `fal-ai/recraft-v3`.
    pub model: String,
    /// Free-form extra payload merged into every request. Any
    /// keys collide with the standard fields (prompt, num_images,
    /// image_size, seed, ...) take priority over the standard
    /// values — this lets callers pin model-specific defaults.
    pub extra_payload: Map<String, Value>,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
}

impl FalImageGenConfig {
    pub fn new(alias: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            base_url: DEFAULT_FAL_BASE.to_string(),
            api_key: None,
            model: model.into(),
            extra_payload: Map::new(),
            extra_headers: HashMap::new(),
            request_timeout: Duration::from_secs(180),
        }
    }
}

pub struct FalImageGenProvider {
    cfg: FalImageGenConfig,
    client: reqwest::Client,
}

impl FalImageGenProvider {
    pub fn new(cfg: FalImageGenConfig) -> Self {
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
        let model = self.cfg.model.trim_start_matches('/');
        format!("{base}/{model}")
    }
}

#[derive(Debug, Serialize)]
struct WireRequest {
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    negative_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_images: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_inference_steps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_size: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct WireImage {
    url: String,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    #[serde(default)]
    images: Vec<WireImage>,
    #[serde(default)]
    seed: Option<u64>,
}

/// Build the `image_size` payload — FAL accepts either a named
/// preset string (`"square_hd"`, `"landscape_4_3"`, ...) or a
/// `{width, height}` object. We always emit the object form when
/// both dims are set; otherwise the field is omitted so the model
/// uses its own default.
pub fn derive_image_size(width: Option<u32>, height: Option<u32>) -> Option<Value> {
    match (width, height) {
        (Some(w), Some(h)) => Some(serde_json::json!({"width": w, "height": h})),
        _ => None,
    }
}

/// Map FAL's `content_type` string to our [`ImageFormat`] enum.
/// Anything outside the known PNG/JPEG/WEBP set falls through to
/// `Other` so callers can still write the bytes to disk with a
/// sensible `.bin` extension.
pub fn format_from_content_type(ct: Option<&str>) -> ImageFormat {
    match ct.unwrap_or("").to_ascii_lowercase().as_str() {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Other,
    }
}

/// Parse the JSON envelope into structured wire responses (no
/// IO). Pulled out for unit testing.
pub fn parse_envelope(bytes: &[u8]) -> Result<WireParsed, MediaError> {
    let parsed: WireResponse =
        serde_json::from_slice(bytes).map_err(|e| MediaError::Parse(e.to_string()))?;
    if parsed.images.is_empty() {
        return Err(MediaError::Parse(
            "fal: response had no images".to_string(),
        ));
    }
    Ok(WireParsed {
        images: parsed
            .images
            .into_iter()
            .map(|i| ParsedImage {
                url: i.url,
                format: format_from_content_type(i.content_type.as_deref()),
                width: i.width.unwrap_or(0),
                height: i.height.unwrap_or(0),
            })
            .collect(),
        seed: parsed.seed,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedImage {
    pub url: String,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireParsed {
    pub images: Vec<ParsedImage>,
    pub seed: Option<u64>,
}

#[async_trait]
impl ImageGenProvider for FalImageGenProvider {
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

        let std_body = WireRequest {
            prompt: request.prompt.clone(),
            negative_prompt: request.negative_prompt.clone(),
            num_images: Some(request.n),
            seed: request.seed,
            num_inference_steps: request.steps,
            image_size: derive_image_size(request.width, request.height),
        };
        // Merge extra_payload over the standard body.
        let mut body_value = serde_json::to_value(&std_body)
            .map_err(|e| MediaError::Internal(e.to_string()))?;
        if let Value::Object(ref mut map) = body_value {
            for (k, v) in &self.cfg.extra_payload {
                map.insert(k.clone(), v.clone());
            }
        }

        let mut http = self
            .client
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .json(&body_value);
        if let Some(key) = &self.cfg.api_key {
            http = http.header("Authorization", format!("Key {key}"));
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

        let envelope = parse_envelope(&bytes)?;
        let mut out_images = Vec::with_capacity(envelope.images.len());
        for img in envelope.images {
            let r = self
                .client
                .get(&img.url)
                .send()
                .await
                .map_err(|e| MediaError::Transport(format!("fal asset fetch: {e}")))?;
            if !r.status().is_success() {
                return Err(MediaError::Provider {
                    status: r.status().as_u16(),
                    message: format!("fal asset {} returned {}", img.url, r.status()),
                });
            }
            let asset = r
                .bytes()
                .await
                .map_err(|e| MediaError::Transport(format!("fal asset read: {e}")))?;
            out_images.push(GeneratedImage {
                bytes: asset.to_vec(),
                format: img.format,
                width: img.width,
                height: img.height,
            });
        }

        Ok(ImageGenResponse {
            images: out_images,
            model: Some(self.cfg.model.clone()),
            seed_used: envelope.seed,
        })
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

    #[test]
    fn endpoint_joins_base_and_model() {
        let mut cfg = FalImageGenConfig::new("fal-flux-dev", "fal-ai/flux/dev");
        cfg.api_key = Some("k".into());
        let p = FalImageGenProvider::new(cfg);
        assert_eq!(p.endpoint(), "https://fal.run/fal-ai/flux/dev");
    }

    #[test]
    fn endpoint_handles_trailing_slash_and_leading_slash() {
        let mut cfg = FalImageGenConfig::new("fal", "/fal-ai/flux/dev");
        cfg.base_url = "https://fal.run/".to_string();
        let p = FalImageGenProvider::new(cfg);
        assert_eq!(p.endpoint(), "https://fal.run/fal-ai/flux/dev");
    }

    #[test]
    fn name_reflects_alias() {
        let p = FalImageGenProvider::new(FalImageGenConfig::new("fal-flux", "fal-ai/flux/dev"));
        assert_eq!(<FalImageGenProvider as ImageGenProvider>::name(&p), "fal-flux");
    }

    #[test]
    fn is_configured_requires_api_key() {
        let mut c = FalImageGenConfig::new("fal", "fal-ai/flux/dev");
        let p1 = FalImageGenProvider::new(c.clone());
        assert!(!<FalImageGenProvider as ImageGenProvider>::is_configured(&p1));
        c.api_key = Some("k".into());
        let p2 = FalImageGenProvider::new(c);
        assert!(<FalImageGenProvider as ImageGenProvider>::is_configured(&p2));
    }

    #[tokio::test]
    async fn generate_without_key_errors_not_configured() {
        let p = FalImageGenProvider::new(FalImageGenConfig::new("fal", "fal-ai/flux/dev"));
        let err = p.generate(ImageGenRequest::new("cat")).await.unwrap_err();
        assert!(matches!(err, MediaError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn generate_validates_request() {
        let mut c = FalImageGenConfig::new("fal", "fal-ai/flux/dev");
        c.api_key = Some("k".into());
        let p = FalImageGenProvider::new(c);
        let err = p.generate(ImageGenRequest::new("")).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[test]
    fn derive_image_size_only_when_both_present() {
        assert_eq!(
            derive_image_size(Some(512), Some(768)),
            Some(serde_json::json!({"width": 512, "height": 768}))
        );
        assert!(derive_image_size(Some(512), None).is_none());
        assert!(derive_image_size(None, Some(768)).is_none());
        assert!(derive_image_size(None, None).is_none());
    }

    #[test]
    fn format_from_content_type_known_types() {
        assert_eq!(format_from_content_type(Some("image/png")), ImageFormat::Png);
        assert_eq!(format_from_content_type(Some("image/jpeg")), ImageFormat::Jpeg);
        assert_eq!(format_from_content_type(Some("image/jpg")), ImageFormat::Jpeg);
        assert_eq!(format_from_content_type(Some("image/webp")), ImageFormat::Webp);
        assert_eq!(format_from_content_type(Some("IMAGE/PNG")), ImageFormat::Png);
        assert_eq!(format_from_content_type(Some("application/octet-stream")), ImageFormat::Other);
        assert_eq!(format_from_content_type(None), ImageFormat::Other);
    }

    #[test]
    fn parse_envelope_basic() {
        let body = br#"{
            "images": [
                {"url": "https://x.example/a.png", "content_type": "image/png",
                 "width": 1024, "height": 1024}
            ],
            "seed": 42
        }"#;
        let parsed = parse_envelope(body).unwrap();
        assert_eq!(parsed.images.len(), 1);
        assert_eq!(parsed.images[0].url, "https://x.example/a.png");
        assert_eq!(parsed.images[0].format, ImageFormat::Png);
        assert_eq!(parsed.images[0].width, 1024);
        assert_eq!(parsed.images[0].height, 1024);
        assert_eq!(parsed.seed, Some(42));
    }

    #[test]
    fn parse_envelope_missing_dims_default_zero() {
        let body = br#"{"images":[{"url":"https://x.example/a.png"}]}"#;
        let parsed = parse_envelope(body).unwrap();
        assert_eq!(parsed.images[0].width, 0);
        assert_eq!(parsed.images[0].height, 0);
        assert_eq!(parsed.images[0].format, ImageFormat::Other);
        assert!(parsed.seed.is_none());
    }

    #[test]
    fn parse_envelope_empty_images_errors() {
        let body = br#"{"images":[]}"#;
        let err = parse_envelope(body).unwrap_err();
        assert!(matches!(err, MediaError::Parse(_)));
    }

    #[test]
    fn parse_envelope_garbage_errors() {
        let err = parse_envelope(b"oops").unwrap_err();
        assert!(matches!(err, MediaError::Parse(_)));
    }

    #[test]
    fn extra_payload_overrides_standard_fields() {
        let body = WireRequest {
            prompt: "cat".to_string(),
            negative_prompt: None,
            num_images: Some(1),
            seed: None,
            num_inference_steps: None,
            image_size: None,
        };
        let mut value = serde_json::to_value(&body).unwrap();
        let mut extra = Map::new();
        extra.insert("num_images".to_string(), serde_json::json!(4));
        extra.insert("scheduler".to_string(), serde_json::json!("dpmpp"));
        if let Value::Object(map) = &mut value {
            for (k, v) in &extra {
                map.insert(k.clone(), v.clone());
            }
        }
        assert_eq!(value["num_images"], 4);
        assert_eq!(value["scheduler"], "dpmpp");
        assert_eq!(value["prompt"], "cat");
    }

    #[test]
    fn body_preview_truncates_long() {
        let big = vec![b'x'; 600];
        let s = body_preview(&big);
        assert!(s.ends_with('…'));
    }
}
