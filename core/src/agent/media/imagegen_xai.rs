//! xAI Grok image generation provider.
//!
//! xAI's image API mirrors OpenAI's `/v1/images/generations`
//! shape, so this module is a thin convenience wrapper around
//! [`OpenAiImageGenProvider`] that pins the alias to `xai`, the
//! base URL to `https://api.x.ai/v1`, and supplies a couple of
//! xAI-specific defaults (default model `grok-2-image`, n cap of
//! 10 — xAI's documented per-call upper bound).
//!
//! If you need to talk to xAI Grok image gen, prefer this module
//! over instantiating `OpenAiImageGenProvider` with `alias = "xai"`
//! manually so the per-call validation stays consistent with the
//! upstream's actual constraints.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;

use super::imagegen::{ImageGenProvider, ImageGenRequest, ImageGenResponse};
use super::imagegen_openai::{OpenAiImageGenConfig, OpenAiImageGenProvider};
use super::MediaError;

pub const PROVIDER_NAME: &str = "xai";
pub const DEFAULT_MODEL: &str = "grok-2-image";
pub const XAI_BASE_URL: &str = "https://api.x.ai/v1";
pub const XAI_MAX_N: u32 = 10;

#[derive(Debug, Clone)]
pub struct XaiImageGenConfig {
    pub api_key: Option<String>,
    pub model: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
}

impl Default for XaiImageGenConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            model: DEFAULT_MODEL.to_string(),
            extra_headers: HashMap::new(),
            request_timeout: Duration::from_secs(120),
        }
    }
}

pub struct XaiImageGenProvider {
    inner: OpenAiImageGenProvider,
}

impl XaiImageGenProvider {
    pub fn new(cfg: XaiImageGenConfig) -> Self {
        let inner_cfg = OpenAiImageGenConfig {
            alias: PROVIDER_NAME.to_string(),
            base_url: XAI_BASE_URL.to_string(),
            api_key: cfg.api_key,
            model: cfg.model,
            extra_headers: cfg.extra_headers,
            request_timeout: cfg.request_timeout,
        };
        Self {
            inner: OpenAiImageGenProvider::new(inner_cfg),
        }
    }
}

#[async_trait]
impl ImageGenProvider for XaiImageGenProvider {
    fn name(&self) -> &str {
        // Pin to "xai" — we always alias-stamp the inner provider
        // with xai, but go through the trait impl so the borrow
        // lifetime matches.
        <OpenAiImageGenProvider as ImageGenProvider>::name(&self.inner)
    }

    fn is_configured(&self) -> bool {
        <OpenAiImageGenProvider as ImageGenProvider>::is_configured(&self.inner)
    }

    async fn generate(&self, request: ImageGenRequest) -> Result<ImageGenResponse, MediaError> {
        // xAI documents a per-call max of 10 images. Surface a
        // crisp validation error before round-tripping.
        if request.n > XAI_MAX_N {
            return Err(MediaError::InvalidRequest(format!(
                "xai imagegen: n {} exceeds xAI per-call cap of {XAI_MAX_N}",
                request.n
            )));
        }
        self.inner.generate(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pin_xai_model_and_url() {
        let cfg = XaiImageGenConfig::default();
        assert_eq!(cfg.model, DEFAULT_MODEL);
        let p = XaiImageGenProvider::new(cfg);
        assert_eq!(<XaiImageGenProvider as ImageGenProvider>::name(&p), "xai");
    }

    #[test]
    fn is_configured_requires_api_key() {
        let p1 = XaiImageGenProvider::new(XaiImageGenConfig::default());
        assert!(!<XaiImageGenProvider as ImageGenProvider>::is_configured(
            &p1
        ));
        let mut c = XaiImageGenConfig::default();
        c.api_key = Some("sk".to_string());
        let p2 = XaiImageGenProvider::new(c);
        assert!(<XaiImageGenProvider as ImageGenProvider>::is_configured(
            &p2
        ));
    }

    #[tokio::test]
    async fn rejects_n_above_xai_cap() {
        let mut c = XaiImageGenConfig::default();
        c.api_key = Some("sk".to_string());
        let p = XaiImageGenProvider::new(c);
        let mut req = ImageGenRequest::new("cat");
        req.n = XAI_MAX_N + 1;
        let err = p.generate(req).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn rejects_when_api_key_missing() {
        let p = XaiImageGenProvider::new(XaiImageGenConfig::default());
        let err = p.generate(ImageGenRequest::new("cat")).await.unwrap_err();
        assert!(matches!(err, MediaError::NotConfigured(_)));
    }

    #[test]
    fn xai_base_url_constant() {
        assert_eq!(XAI_BASE_URL, "https://api.x.ai/v1");
    }

    #[test]
    fn xai_max_n_constant() {
        assert_eq!(XAI_MAX_N, 10);
    }
}
