use super::*;

#[test]
fn default_base_url_for_known_aliases() {
    assert_eq!(default_base_url_for("openai"), DEFAULT_OPENAI_BASE);
    assert_eq!(default_base_url_for("groq"), DEFAULT_GROQ_BASE);
    assert_eq!(default_base_url_for("xai"), DEFAULT_XAI_BASE);
    assert_eq!(default_base_url_for("mistral"), DEFAULT_MISTRAL_BASE);
    assert_eq!(default_base_url_for("custom"), DEFAULT_OPENAI_BASE);
}

#[test]
fn audio_format_filename_extensions() {
    assert_eq!(audio_format_filename(AudioFormat::Wav).0, "audio.wav");
    assert_eq!(audio_format_filename(AudioFormat::Mp3).0, "audio.mp3");
    assert_eq!(audio_format_filename(AudioFormat::Ogg).0, "audio.ogg");
    assert_eq!(audio_format_filename(AudioFormat::Pcm16).0, "audio.pcm");
    assert_eq!(audio_format_filename(AudioFormat::Other).0, "audio.bin");
}

#[test]
fn for_alias_pulls_default_base_url() {
    let c = CloudSttConfig::for_alias("groq", "whisper-large-v3");
    assert_eq!(c.base_url, DEFAULT_GROQ_BASE);
    assert_eq!(c.model, "whisper-large-v3");
    assert!(c.api_key.is_none());
}

#[test]
fn endpoint_strips_trailing_slash() {
    let mut c = CloudSttConfig::for_alias("openai", "whisper-1");
    c.base_url = "https://api.openai.com/v1/".to_string();
    let p = CloudSttProvider::new(c);
    assert_eq!(
        p.endpoint(),
        "https://api.openai.com/v1/audio/transcriptions"
    );
}

#[test]
fn name_reflects_alias() {
    let cfg = CloudSttConfig::for_alias("groq", "whisper-large-v3");
    let p = CloudSttProvider::new(cfg);
    assert_eq!(<CloudSttProvider as SttProvider>::name(&p), "groq");
}

#[test]
fn is_configured_requires_api_key() {
    let mut cfg = CloudSttConfig::for_alias("openai", "whisper-1");
    let p1 = CloudSttProvider::new(cfg.clone());
    assert!(!<CloudSttProvider as SttProvider>::is_configured(&p1));
    cfg.api_key = Some("sk".to_string());
    let p2 = CloudSttProvider::new(cfg);
    assert!(<CloudSttProvider as SttProvider>::is_configured(&p2));
}

#[tokio::test]
async fn transcribe_without_key_errors_not_configured() {
    let cfg = CloudSttConfig::for_alias("openai", "whisper-1");
    let p = CloudSttProvider::new(cfg);
    let req = SttRequest::new(vec![1, 2, 3], AudioFormat::Wav);
    let err = p.transcribe(req).await.unwrap_err();
    assert!(matches!(err, MediaError::NotConfigured(_)));
}

#[tokio::test]
async fn transcribe_validates_request() {
    let mut cfg = CloudSttConfig::for_alias("openai", "whisper-1");
    cfg.api_key = Some("sk".to_string());
    let p = CloudSttProvider::new(cfg);
    let req = SttRequest::new(vec![], AudioFormat::Wav);
    let err = p.transcribe(req).await.unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[test]
fn parse_response_minimal_text_only() {
    let r = parse_response(br#"{"text":"hello world"}"#).unwrap();
    assert_eq!(r.text, "hello world");
    assert!(r.language.is_none());
    assert!(r.segments.is_empty());
}

#[test]
fn parse_response_verbose_with_segments() {
    let body = br#"{
        "text": "hello world",
        "language": "en",
        "segments": [
            {"start": 0.0, "end": 0.5, "text": "hello"},
            {"start": 0.5, "end": 1.0, "text": "world"}
        ]
    }"#;
    let r = parse_response(body).unwrap();
    assert_eq!(r.text, "hello world");
    assert_eq!(r.language.as_deref(), Some("en"));
    assert_eq!(r.segments.len(), 2);
    assert_eq!(r.segments[0].start_ms, 0);
    assert_eq!(r.segments[0].end_ms, 500);
    assert_eq!(r.segments[1].start_ms, 500);
    assert_eq!(r.segments[1].end_ms, 1000);
}

#[test]
fn parse_response_negative_starts_clamp_to_zero() {
    let body = br#"{
        "text": "x",
        "segments": [{"start": -0.001, "end": 0.1, "text": "x"}]
    }"#;
    let r = parse_response(body).unwrap();
    assert_eq!(r.segments[0].start_ms, 0);
}

#[test]
fn parse_response_rejects_garbage() {
    let err = parse_response(b"{not json").unwrap_err();
    assert!(matches!(err, MediaError::Parse(_)));
}

#[test]
fn body_preview_truncates_long() {
    let big = vec![b'x'; 600];
    let s = body_preview(&big);
    assert!(s.ends_with('…'));
}

#[test]
fn provider_aliases_listed() {
    for a in ["openai", "groq", "xai", "mistral", "custom"] {
        assert!(PROVIDER_ALIASES.contains(&a));
    }
}
