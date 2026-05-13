//! Image generation provider trait + registry.
//!
//! Concrete backends (FAL.ai, OpenAI DALL-E / gpt-image, xAI image
//! gen, future local SD/Flux via cos model) implement
//! [`ImageGenProvider`] and register with [`ImageGenRegistry`].
//!
//! This commit ships the trait, registry, request/response types,
//! and a `noop` reference implementation. Real backends arrive in
//! follow-up commits.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::MediaError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Other,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Webp => "webp",
            ImageFormat::Other => "bin",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Webp => "image/webp",
            ImageFormat::Other => "application/octet-stream",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageGenRequest {
    pub prompt: String,
    pub negative_prompt: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub steps: Option<u32>,
    pub seed: Option<u64>,
    pub format: Option<ImageFormat>,
    /// Number of images to return. Must be >= 1.
    pub n: u32,
}

impl ImageGenRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            negative_prompt: None,
            width: None,
            height: None,
            steps: None,
            seed: None,
            format: None,
            n: 1,
        }
    }

    pub fn validate(&self) -> Result<(), MediaError> {
        if self.prompt.trim().is_empty() {
            return Err(MediaError::InvalidRequest(
                "imagegen: prompt must be non-empty".to_string(),
            ));
        }
        if self.n == 0 {
            return Err(MediaError::InvalidRequest(
                "imagegen: n must be >= 1".to_string(),
            ));
        }
        if self.n > 16 {
            return Err(MediaError::InvalidRequest(format!(
                "imagegen: n {} exceeds reasonable cap of 16",
                self.n
            )));
        }
        if let Some(w) = self.width {
            if w == 0 {
                return Err(MediaError::InvalidRequest(
                    "imagegen: width must be > 0".to_string(),
                ));
            }
        }
        if let Some(h) = self.height {
            if h == 0 {
                return Err(MediaError::InvalidRequest(
                    "imagegen: height must be > 0".to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageGenResponse {
    pub images: Vec<GeneratedImage>,
    pub model: Option<String>,
    pub seed_used: Option<u64>,
}

#[async_trait]
pub trait ImageGenProvider: Send + Sync {
    fn name(&self) -> &str;
    fn is_configured(&self) -> bool;
    async fn generate(&self, request: ImageGenRequest) -> Result<ImageGenResponse, MediaError>;
}

#[derive(Default, Clone)]
pub struct ImageGenRegistry {
    inner: Arc<BTreeMap<String, Arc<dyn ImageGenProvider>>>,
}

impl ImageGenRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_providers() -> Self {
        let mut map = BTreeMap::new();
        let noop: Arc<dyn ImageGenProvider> = Arc::new(NoopImageGen);
        map.insert(noop.name().to_string(), noop);
        Self {
            inner: Arc::new(map),
        }
    }

    pub fn register(&mut self, provider: Arc<dyn ImageGenProvider>) {
        let name = provider.name().to_string();
        let mut map = (*self.inner).clone();
        map.insert(name, provider);
        self.inner = Arc::new(map);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ImageGenProvider>> {
        self.inner.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.inner.keys().cloned().collect()
    }
}

/// Reference impl: returns N tiny 1x1 PNGs so callers can exercise
/// the call path before any real backend is configured.
pub struct NoopImageGen;

#[async_trait]
impl ImageGenProvider for NoopImageGen {
    fn name(&self) -> &str {
        "noop"
    }
    fn is_configured(&self) -> bool {
        true
    }
    async fn generate(&self, request: ImageGenRequest) -> Result<ImageGenResponse, MediaError> {
        request.validate()?;
        let format = request.format.unwrap_or(ImageFormat::Png);
        let images = (0..request.n)
            .map(|_| GeneratedImage {
                bytes: minimal_png_1x1(),
                format,
                width: request.width.unwrap_or(1),
                height: request.height.unwrap_or(1),
            })
            .collect();
        Ok(ImageGenResponse {
            images,
            model: Some("noop".to_string()),
            seed_used: request.seed,
        })
    }
}

/// Minimal valid 1x1 PNG (8-bit RGBA, single transparent pixel).
/// Constants taken from a hand-built PNG; the bytes are the
/// canonical "smallest valid PNG" sequence widely cited for
/// placeholder use.
fn minimal_png_1x1() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_extensions_and_mime() {
        assert_eq!(ImageFormat::Png.extension(), "png");
        assert_eq!(ImageFormat::Jpeg.extension(), "jpg");
        assert_eq!(ImageFormat::Webp.extension(), "webp");
        assert_eq!(ImageFormat::Other.extension(), "bin");
        assert_eq!(ImageFormat::Png.mime(), "image/png");
        assert_eq!(ImageFormat::Jpeg.mime(), "image/jpeg");
    }

    #[test]
    fn request_rejects_empty_prompt() {
        assert!(ImageGenRequest::new("   ").validate().is_err());
    }

    #[test]
    fn request_rejects_zero_n() {
        let mut r = ImageGenRequest::new("p");
        r.n = 0;
        assert!(r.validate().is_err());
    }

    #[test]
    fn request_rejects_excessive_n() {
        let mut r = ImageGenRequest::new("p");
        r.n = 17;
        assert!(r.validate().is_err());
    }

    #[test]
    fn request_rejects_zero_dimension() {
        let mut r = ImageGenRequest::new("p");
        r.width = Some(0);
        assert!(r.validate().is_err());
        r.width = Some(512);
        r.height = Some(0);
        assert!(r.validate().is_err());
        r.height = Some(512);
        assert!(r.validate().is_ok());
    }

    #[tokio::test]
    async fn noop_returns_n_pngs() {
        let p = NoopImageGen;
        let mut r = ImageGenRequest::new("hi");
        r.n = 3;
        let resp = p.generate(r).await.unwrap();
        assert_eq!(resp.images.len(), 3);
        for img in &resp.images {
            assert_eq!(img.format, ImageFormat::Png);
            assert_eq!(
                &img.bytes[..8],
                &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
            );
        }
    }

    #[tokio::test]
    async fn noop_passes_through_seed() {
        let p = NoopImageGen;
        let mut r = ImageGenRequest::new("hi");
        r.seed = Some(42);
        let resp = p.generate(r).await.unwrap();
        assert_eq!(resp.seed_used, Some(42));
    }

    #[tokio::test]
    async fn noop_validates_request() {
        let p = NoopImageGen;
        let err = p.generate(ImageGenRequest::new("")).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[test]
    fn registry_default_has_noop() {
        let r = ImageGenRegistry::with_default_providers();
        assert!(r.get("noop").is_some());
    }

    #[test]
    fn registry_register_and_lookup() {
        struct Custom;
        #[async_trait]
        impl ImageGenProvider for Custom {
            fn name(&self) -> &str {
                "custom"
            }
            fn is_configured(&self) -> bool {
                false
            }
            async fn generate(&self, _: ImageGenRequest) -> Result<ImageGenResponse, MediaError> {
                Err(MediaError::NotConfigured("custom".to_string()))
            }
        }
        let mut r = ImageGenRegistry::new();
        r.register(Arc::new(Custom));
        assert!(r.get("custom").is_some());
    }

    #[test]
    fn registry_clone_independent_after_mutation() {
        let r1 = ImageGenRegistry::with_default_providers();
        let mut r2 = r1.clone();
        struct Extra;
        #[async_trait]
        impl ImageGenProvider for Extra {
            fn name(&self) -> &str {
                "extra"
            }
            fn is_configured(&self) -> bool {
                true
            }
            async fn generate(&self, _: ImageGenRequest) -> Result<ImageGenResponse, MediaError> {
                Ok(ImageGenResponse {
                    images: Vec::new(),
                    model: None,
                    seed_used: None,
                })
            }
        }
        r2.register(Arc::new(Extra));
        assert!(r1.get("extra").is_none());
        assert!(r2.get("extra").is_some());
    }

    #[test]
    fn registry_unknown_name_returns_none() {
        let r = ImageGenRegistry::with_default_providers();
        assert!(r.get("nope").is_none());
    }
}
