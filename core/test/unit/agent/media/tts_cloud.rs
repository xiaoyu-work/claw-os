use super::*;

#[test]
fn default_base_url_for_known_aliases() {
    assert_eq!(default_base_url_for("openai"), DEFAULT_OPENAI_BASE);
    assert_eq!(default_base_url_for("xai"), DEFAULT_XAI_BASE);
    assert_eq!(default_base_url_for("custom"), DEFAULT_OPENAI_BASE);
    assert_eq!(default_base_url_for("unknown"), DEFAULT_OPENAI_BASE);
}

#[test]
fn audio_format_wire_mapping() {
    assert_eq!(audio_format_wire(AudioFormat::Mp3), "mp3");
    assert_eq!(audio_format_wire(AudioFormat::Wav), "wav");
    assert_eq!(audio_format_wire(AudioFormat::Ogg), "opus");
    assert_eq!(audio_format_wire(AudioFormat::Pcm16), "pcm");
    assert_eq!(audio_format_wire(AudioFormat::Other), "mp3");
}

#[test]
fn for_alias_pulls_default_base_url() {
    let c = CloudTtsConfig::for_alias("xai", "tts-1");
    assert_eq!(c.base_url, DEFAULT_XAI_BASE);
    assert_eq!(c.model, "tts-1");
    assert!(c.api_key.is_none());
}

#[test]
fn endpoint_strips_trailing_slash() {
    let mut c = CloudTtsConfig::for_alias("openai", "tts-1");
    c.base_url = "https://example.com/v1/".to_string();
    let p = CloudTtsProvider::new(c);
    assert_eq!(p.endpoint(), "https://example.com/v1/audio/speech");
}

#[test]
fn provider_aliases_listed() {
    assert!(PROVIDER_ALIASES.contains(&"openai"));
    assert!(PROVIDER_ALIASES.contains(&"xai"));
    assert!(PROVIDER_ALIASES.contains(&"custom"));
}

#[test]
fn name_reflects_alias() {
    let cfg = CloudTtsConfig::for_alias("xai", "tts-1");
    let p = CloudTtsProvider::new(cfg);
    assert_eq!(<CloudTtsProvider as TtsProvider>::name(&p), "xai");
}

#[test]
fn is_configured_requires_api_key() {
    let mut cfg = CloudTtsConfig::for_alias("openai", "tts-1");
    let p1 = CloudTtsProvider::new(cfg.clone());
    assert!(!<CloudTtsProvider as TtsProvider>::is_configured(&p1));
    cfg.api_key = Some("sk-test".to_string());
    let p2 = CloudTtsProvider::new(cfg);
    assert!(<CloudTtsProvider as TtsProvider>::is_configured(&p2));
}

#[tokio::test]
async fn synthesize_without_key_errors_not_configured() {
    let cfg = CloudTtsConfig::for_alias("openai", "tts-1");
    let p = CloudTtsProvider::new(cfg);
    let err = p.synthesize(TtsRequest::new("hello")).await.unwrap_err();
    assert!(matches!(err, MediaError::NotConfigured(_)));
}

#[tokio::test]
async fn synthesize_validates_request() {
    let mut cfg = CloudTtsConfig::for_alias("openai", "tts-1");
    cfg.api_key = Some("sk-test".to_string());
    let p = CloudTtsProvider::new(cfg);
    let err = p.synthesize(TtsRequest::new("")).await.unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[test]
fn body_preview_truncates_long_payloads() {
    let big = vec![b'x'; 600];
    let s = body_preview(&big);
    assert!(s.ends_with('…'));
    assert!(s.chars().count() <= 513);
}

#[test]
fn body_preview_keeps_short_payload() {
    assert_eq!(body_preview(b"oops"), "oops");
}

#[test]
fn wire_request_serialises_required_fields() {
    let body = WireRequest {
        model: "tts-1",
        input: "hello",
        voice: "alloy",
        response_format: "mp3",
        speed: None,
    };
    let json = serde_json::to_value(&body).unwrap();
    assert_eq!(json["model"], "tts-1");
    assert_eq!(json["input"], "hello");
    assert_eq!(json["voice"], "alloy");
    assert_eq!(json["response_format"], "mp3");
    assert!(json.get("speed").is_none());
}

#[test]
fn wire_request_includes_speed_when_set() {
    let body = WireRequest {
        model: "tts-1",
        input: "hi",
        voice: "alloy",
        response_format: "mp3",
        speed: Some(1.25),
    };
    let json = serde_json::to_value(&body).unwrap();
    assert_eq!(json["speed"], 1.25);
}
