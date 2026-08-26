use super::*;

#[test]
fn output_format_query_mapping() {
    assert_eq!(output_format_query(AudioFormat::Mp3), "mp3_44100_128");
    assert_eq!(output_format_query(AudioFormat::Wav), "pcm_16000");
    assert_eq!(output_format_query(AudioFormat::Ogg), "ogg_44100_64");
    assert_eq!(output_format_query(AudioFormat::Pcm16), "pcm_16000");
    assert_eq!(output_format_query(AudioFormat::Other), "mp3_44100_128");
}

#[test]
fn response_container_for_known_formats() {
    assert_eq!(response_container_for("mp3_44100_128"), AudioFormat::Mp3);
    assert_eq!(response_container_for("ogg_44100_64"), AudioFormat::Ogg);
    assert_eq!(response_container_for("pcm_16000"), AudioFormat::Pcm16);
    assert_eq!(response_container_for("ulaw_8000"), AudioFormat::Other);
}

#[test]
fn endpoint_concatenates_voice_id() {
    let cfg = ElevenLabsConfig::default();
    let p = ElevenLabsProvider::new(cfg);
    assert_eq!(
        p.endpoint("21m00Tcm4TlvDq8ikWAM"),
        "https://api.elevenlabs.io/v1/text-to-speech/21m00Tcm4TlvDq8ikWAM"
    );
}

#[test]
fn endpoint_strips_trailing_slash_on_base() {
    let mut cfg = ElevenLabsConfig::default();
    cfg.base_url = "https://example.com/".to_string();
    let p = ElevenLabsProvider::new(cfg);
    assert_eq!(p.endpoint("v1"), "https://example.com/v1/text-to-speech/v1");
}

#[test]
fn provider_name_is_stable() {
    let p = ElevenLabsProvider::new(ElevenLabsConfig::default());
    assert_eq!(<ElevenLabsProvider as TtsProvider>::name(&p), "elevenlabs");
}

#[test]
fn is_configured_requires_api_key() {
    let mut cfg = ElevenLabsConfig::default();
    let p1 = ElevenLabsProvider::new(cfg.clone());
    assert!(!<ElevenLabsProvider as TtsProvider>::is_configured(&p1));
    cfg.api_key = Some("xi-test".to_string());
    let p2 = ElevenLabsProvider::new(cfg);
    assert!(<ElevenLabsProvider as TtsProvider>::is_configured(&p2));
}

#[tokio::test]
async fn synthesize_without_key_errors_not_configured() {
    let cfg = ElevenLabsConfig::default();
    let p = ElevenLabsProvider::new(cfg);
    let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
    assert!(matches!(err, MediaError::NotConfigured(_)));
}

#[tokio::test]
async fn synthesize_validates_request() {
    let mut cfg = ElevenLabsConfig::default();
    cfg.api_key = Some("xi-test".to_string());
    cfg.default_voice_id = Some("v1".to_string());
    let p = ElevenLabsProvider::new(cfg);
    let err = p.synthesize(TtsRequest::new("")).await.unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[tokio::test]
async fn synthesize_requires_voice_id() {
    let mut cfg = ElevenLabsConfig::default();
    cfg.api_key = Some("xi-test".to_string());
    let p = ElevenLabsProvider::new(cfg);
    let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
    match err {
        MediaError::InvalidRequest(msg) => assert!(msg.contains("voice_id")),
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
}

#[test]
fn voice_settings_omitted_when_unset() {
    let cfg = ElevenLabsConfig::default();
    assert!(VoiceSettings::from_cfg(&cfg).is_none());
}

#[test]
fn voice_settings_set_when_either_field_present() {
    let mut cfg = ElevenLabsConfig::default();
    cfg.stability = Some(0.5);
    let s = VoiceSettings::from_cfg(&cfg).unwrap();
    assert_eq!(s.stability, Some(0.5));
    assert_eq!(s.similarity_boost, None);
}

#[test]
fn wire_request_serializes_required_fields() {
    let body = WireRequest {
        text: "hi",
        model_id: "eleven_multilingual_v2",
        voice_settings: None,
    };
    let json = serde_json::to_value(&body).unwrap();
    assert_eq!(json["text"], "hi");
    assert_eq!(json["model_id"], "eleven_multilingual_v2");
    assert!(json.get("voice_settings").is_none());
}

#[test]
fn wire_request_with_voice_settings() {
    let body = WireRequest {
        text: "hi",
        model_id: "m",
        voice_settings: Some(VoiceSettings {
            stability: Some(0.5),
            similarity_boost: Some(0.75),
        }),
    };
    let json = serde_json::to_value(&body).unwrap();
    assert!((json["voice_settings"]["stability"].as_f64().unwrap() - 0.5).abs() < 1e-4);
    assert!((json["voice_settings"]["similarity_boost"].as_f64().unwrap() - 0.75).abs() < 1e-4);
}

#[test]
fn preview_truncates_long_payloads() {
    let big = vec![b'x'; 600];
    let s = preview(&big);
    assert!(s.ends_with('…'));
}

#[test]
fn preview_keeps_short_payload() {
    assert_eq!(preview(b"err"), "err");
}
