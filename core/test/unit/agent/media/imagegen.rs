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
