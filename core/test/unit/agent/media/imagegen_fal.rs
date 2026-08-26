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
    assert_eq!(
        <FalImageGenProvider as ImageGenProvider>::name(&p),
        "fal-flux"
    );
}

#[test]
fn is_configured_requires_api_key() {
    let mut c = FalImageGenConfig::new("fal", "fal-ai/flux/dev");
    let p1 = FalImageGenProvider::new(c.clone());
    assert!(!<FalImageGenProvider as ImageGenProvider>::is_configured(
        &p1
    ));
    c.api_key = Some("k".into());
    let p2 = FalImageGenProvider::new(c);
    assert!(<FalImageGenProvider as ImageGenProvider>::is_configured(
        &p2
    ));
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
    assert_eq!(
        format_from_content_type(Some("image/png")),
        ImageFormat::Png
    );
    assert_eq!(
        format_from_content_type(Some("image/jpeg")),
        ImageFormat::Jpeg
    );
    assert_eq!(
        format_from_content_type(Some("image/jpg")),
        ImageFormat::Jpeg
    );
    assert_eq!(
        format_from_content_type(Some("image/webp")),
        ImageFormat::Webp
    );
    assert_eq!(
        format_from_content_type(Some("IMAGE/PNG")),
        ImageFormat::Png
    );
    assert_eq!(
        format_from_content_type(Some("application/octet-stream")),
        ImageFormat::Other
    );
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
