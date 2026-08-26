use super::*;
use base64::engine::general_purpose::STANDARD as B64;

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
    let p = OpenAiImageGenProvider::new(OpenAiImageGenConfig::for_alias("openai", "dall-e-3"));
    assert_eq!(
        <OpenAiImageGenProvider as ImageGenProvider>::name(&p),
        "openai"
    );
}

#[test]
fn is_configured_requires_api_key() {
    let mut c = OpenAiImageGenConfig::for_alias("openai", "dall-e-3");
    let p1 = OpenAiImageGenProvider::new(c.clone());
    assert!(!<OpenAiImageGenProvider as ImageGenProvider>::is_configured(&p1));
    c.api_key = Some("sk".to_string());
    let p2 = OpenAiImageGenProvider::new(c);
    assert!(<OpenAiImageGenProvider as ImageGenProvider>::is_configured(
        &p2
    ));
}

#[tokio::test]
async fn generate_without_key_errors_not_configured() {
    let p = OpenAiImageGenProvider::new(OpenAiImageGenConfig::for_alias("openai", "dall-e-3"));
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
    assert_eq!(
        derive_size(Some(1024), Some(1024)).as_deref(),
        Some("1024x1024")
    );
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
