use super::*;

#[test]
fn audio_format_wire_mapping() {
    assert_eq!(audio_format_wire(AudioFormat::Mp3), "mp3");
    assert_eq!(audio_format_wire(AudioFormat::Wav), "wav");
    assert_eq!(audio_format_wire(AudioFormat::Ogg), "ogg");
    assert_eq!(audio_format_wire(AudioFormat::Pcm16), "pcm");
    assert_eq!(audio_format_wire(AudioFormat::Other), "mp3");
}

#[test]
fn default_sample_rate_per_format() {
    assert_eq!(default_sample_rate(AudioFormat::Wav), 44_100);
    assert_eq!(default_sample_rate(AudioFormat::Mp3), 32_000);
    assert_eq!(default_sample_rate(AudioFormat::Ogg), 32_000);
    assert_eq!(default_sample_rate(AudioFormat::Pcm16), 32_000);
    assert_eq!(default_sample_rate(AudioFormat::Other), 32_000);
}

#[test]
fn endpoint_strips_trailing_slash() {
    let mut cfg = MiniMaxConfig::default();
    cfg.base_url = "https://api.example.com/".to_string();
    let p = MiniMaxTts::new(cfg);
    assert_eq!(p.endpoint(), "https://api.example.com/v1/t2a_v2");
}

#[test]
fn name_is_stable() {
    let p = MiniMaxTts::new(MiniMaxConfig::default());
    assert_eq!(<MiniMaxTts as TtsProvider>::name(&p), "minimax");
}

#[test]
fn is_configured_requires_both_key_and_group() {
    let mut cfg = MiniMaxConfig::default();
    let p = MiniMaxTts::new(cfg.clone());
    assert!(!<MiniMaxTts as TtsProvider>::is_configured(&p));
    cfg.api_key = Some("k".into());
    assert!(!<MiniMaxTts as TtsProvider>::is_configured(
        &MiniMaxTts::new(cfg.clone())
    ));
    cfg.group_id = Some("g".into());
    assert!(<MiniMaxTts as TtsProvider>::is_configured(
        &MiniMaxTts::new(cfg)
    ));
}

#[tokio::test]
async fn synthesize_without_key_errors() {
    let p = MiniMaxTts::new(MiniMaxConfig::default());
    let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
    assert!(matches!(err, MediaError::NotConfigured(_)));
}

#[tokio::test]
async fn synthesize_without_group_errors() {
    let mut cfg = MiniMaxConfig::default();
    cfg.api_key = Some("k".into());
    let p = MiniMaxTts::new(cfg);
    let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
    match err {
        MediaError::NotConfigured(msg) => assert!(msg.contains("group_id")),
        other => panic!("expected NotConfigured(group_id), got {other:?}"),
    }
}

#[tokio::test]
async fn synthesize_validates_text() {
    let mut cfg = MiniMaxConfig::default();
    cfg.api_key = Some("k".into());
    cfg.group_id = Some("g".into());
    cfg.default_voice_id = Some("v".into());
    let p = MiniMaxTts::new(cfg);
    let err = p.synthesize(TtsRequest::new("")).await.unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[tokio::test]
async fn synthesize_requires_voice_id() {
    let mut cfg = MiniMaxConfig::default();
    cfg.api_key = Some("k".into());
    cfg.group_id = Some("g".into());
    let p = MiniMaxTts::new(cfg);
    let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
    match err {
        MediaError::InvalidRequest(msg) => assert!(msg.contains("voice_id")),
        other => panic!("expected InvalidRequest(voice_id), got {other:?}"),
    }
}

#[test]
fn decode_hex_roundtrip_simple() {
    let bytes = vec![0x00u8, 0xff, 0xa0, 0x12, 0x34];
    let s = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(decode_hex(&s).unwrap(), bytes);
}

#[test]
fn decode_hex_accepts_uppercase() {
    assert_eq!(
        decode_hex("DEADBEEF").unwrap(),
        vec![0xde, 0xad, 0xbe, 0xef]
    );
}

#[test]
fn decode_hex_rejects_odd_length() {
    assert!(decode_hex("abc").is_err());
}

#[test]
fn decode_hex_rejects_bad_nibble() {
    assert!(decode_hex("zz").is_err());
}

#[test]
fn wire_request_serialises_required_fields() {
    let body = WireRequest {
        model: "speech-02-hd",
        text: "hi",
        stream: false,
        voice_setting: VoiceSetting {
            voice_id: "v1",
            speed: None,
        },
        audio_setting: AudioSetting {
            sample_rate: 32_000,
            format: "mp3",
            channel: 1,
        },
    };
    let json = serde_json::to_value(&body).unwrap();
    assert_eq!(json["model"], "speech-02-hd");
    assert_eq!(json["text"], "hi");
    assert_eq!(json["stream"], false);
    assert_eq!(json["voice_setting"]["voice_id"], "v1");
    assert_eq!(json["audio_setting"]["format"], "mp3");
    assert_eq!(json["audio_setting"]["sample_rate"], 32_000);
    assert_eq!(json["audio_setting"]["channel"], 1);
    assert!(json["voice_setting"].get("speed").is_none());
}

#[test]
fn wire_response_parses_success() {
    let raw = r#"{"data":{"audio":"abcd","status":2},"base_resp":{"status_code":0,"status_msg":"success"}}"#;
    let r: WireResponse = serde_json::from_str(raw).unwrap();
    assert_eq!(r.data.unwrap().audio.unwrap(), "abcd");
    assert_eq!(r.base_resp.unwrap().status_code, 0);
}

#[test]
fn wire_response_parses_failure() {
    let raw = r#"{"base_resp":{"status_code":1004,"status_msg":"insufficient quota"}}"#;
    let r: WireResponse = serde_json::from_str(raw).unwrap();
    assert!(r.data.is_none());
    let br = r.base_resp.unwrap();
    assert_eq!(br.status_code, 1004);
    assert_eq!(br.status_msg, "insufficient quota");
}

#[test]
fn preview_truncates_long_payload() {
    let big = vec![b'x'; 600];
    let s = preview(&big);
    assert!(s.ends_with('…'));
}
