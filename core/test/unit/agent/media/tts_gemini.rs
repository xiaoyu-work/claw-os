use super::*;

#[test]
fn endpoint_includes_model() {
    let cfg = GeminiTtsConfig::default();
    let p = GeminiTts::new(cfg);
    assert_eq!(
        p.endpoint(),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash-preview-tts:generateContent"
    );
}

#[test]
fn endpoint_strips_trailing_slash() {
    let mut cfg = GeminiTtsConfig::default();
    cfg.base_url = "https://example.com/".to_string();
    cfg.model = "m".to_string();
    let p = GeminiTts::new(cfg);
    assert_eq!(
        p.endpoint(),
        "https://example.com/v1beta/models/m:generateContent"
    );
}

#[test]
fn name_is_stable() {
    let p = GeminiTts::new(GeminiTtsConfig::default());
    assert_eq!(<GeminiTts as TtsProvider>::name(&p), "gemini-tts");
}

#[test]
fn is_configured_requires_api_key() {
    let mut cfg = GeminiTtsConfig::default();
    let p1 = GeminiTts::new(cfg.clone());
    assert!(!<GeminiTts as TtsProvider>::is_configured(&p1));
    cfg.api_key = Some("k".into());
    assert!(<GeminiTts as TtsProvider>::is_configured(&GeminiTts::new(
        cfg
    )));
}

#[tokio::test]
async fn synthesize_without_key_errors() {
    let p = GeminiTts::new(GeminiTtsConfig::default());
    let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
    assert!(matches!(err, MediaError::NotConfigured(_)));
}

#[tokio::test]
async fn synthesize_validates_text() {
    let mut cfg = GeminiTtsConfig::default();
    cfg.api_key = Some("k".into());
    let p = GeminiTts::new(cfg);
    let err = p.synthesize(TtsRequest::new("")).await.unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[test]
fn parse_sample_rate_extracts_rate() {
    assert_eq!(
        parse_sample_rate("audio/L16;codec=pcm;rate=24000"),
        Some(24_000)
    );
    assert_eq!(
        parse_sample_rate("audio/L16; codec=pcm; rate=16000"),
        Some(16_000)
    );
    assert_eq!(parse_sample_rate("audio/L16;codec=pcm"), None);
    assert_eq!(parse_sample_rate(""), None);
    assert_eq!(parse_sample_rate("rate=oops"), None);
}

#[test]
fn decode_base64_known_vectors() {
    assert_eq!(decode_base64("").unwrap(), Vec::<u8>::new());
    assert_eq!(decode_base64("Zm9v").unwrap(), b"foo");
    assert_eq!(decode_base64("Zm9vYg==").unwrap(), b"foob");
    assert_eq!(decode_base64("Zm9vYmE=").unwrap(), b"fooba");
    assert_eq!(decode_base64("Zm9vYmFy").unwrap(), b"foobar");
}

#[test]
fn decode_base64_rejects_bad_alphabet_and_length() {
    assert!(decode_base64("Zm9").is_err());
    assert!(decode_base64("Zm9v Yg==").is_err());
    assert!(decode_base64("Zm9*").is_err());
}

#[test]
fn wire_request_serialises_required_shape() {
    let body = WireRequest {
        contents: vec![Content {
            parts: vec![Part { text: "hi" }],
        }],
        generation_config: GenerationConfig {
            response_modalities: vec!["AUDIO"],
            speech_config: Some(SpeechConfig {
                voice_config: VoiceConfig {
                    prebuilt_voice_config: PrebuiltVoice { voice_name: "Kore" },
                },
            }),
        },
    };
    let json = serde_json::to_value(&body).unwrap();
    assert_eq!(json["contents"][0]["parts"][0]["text"], "hi");
    assert_eq!(json["generationConfig"]["responseModalities"][0], "AUDIO");
    assert_eq!(
        json["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
            ["voiceName"],
        "Kore"
    );
}

#[test]
fn wire_response_extracts_inline_data() {
    let raw = r#"{
        "candidates": [{
            "content": {
                "parts": [{
                    "inlineData": { "mimeType": "audio/L16;codec=pcm;rate=24000", "data": "Zm9v" }
                }]
            }
        }]
    }"#;
    let r: WireResponse = serde_json::from_str(raw).unwrap();
    let inline = r.candidates[0].content.as_ref().unwrap().parts[0]
        .inline_data
        .as_ref()
        .unwrap();
    assert_eq!(inline.mime_type, "audio/L16;codec=pcm;rate=24000");
    assert_eq!(inline.data, "Zm9v");
}

#[test]
fn wire_response_parses_error_payload() {
    let raw = r#"{"error":{"code":403,"message":"perm","status":"PERMISSION_DENIED"}}"#;
    let r: WireResponse = serde_json::from_str(raw).unwrap();
    let err = r.error.unwrap();
    assert_eq!(err.code, 403);
    assert_eq!(err.status, "PERMISSION_DENIED");
    assert_eq!(err.message, "perm");
}

#[test]
fn preview_truncates_long_payload() {
    let big = vec![b'x'; 600];
    let s = preview(&big);
    assert!(s.ends_with('…'));
}
