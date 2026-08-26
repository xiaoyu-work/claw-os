use super::*;

#[test]
fn name_is_edge_tts() {
    let p = EdgeTtsProvider::new(EdgeTtsConfig::default());
    assert_eq!(p.name(), "edge-tts");
}

#[test]
fn always_configured_no_api_key_required() {
    let p = EdgeTtsProvider::new(EdgeTtsConfig::default());
    assert!(p.is_configured());
}

#[test]
fn xml_escape_replaces_specials() {
    assert_eq!(xml_escape("a < b"), "a &lt; b");
    assert_eq!(xml_escape("a > b"), "a &gt; b");
    assert_eq!(xml_escape("a & b"), "a &amp; b");
    assert_eq!(xml_escape("'q'"), "&apos;q&apos;");
    assert_eq!(xml_escape("\"q\""), "&quot;q&quot;");
    assert_eq!(xml_escape("safe text"), "safe text");
}

#[test]
fn xml_escape_handles_combo() {
    assert_eq!(
        xml_escape("<x>'a'&\"b\"</x>"),
        "&lt;x&gt;&apos;a&apos;&amp;&quot;b&quot;&lt;/x&gt;"
    );
}

#[test]
fn format_rate_zero_default() {
    assert_eq!(format_rate(None), "+0%");
    assert_eq!(format_rate(Some(1.0)), "+0%");
}

#[test]
fn format_rate_positive_speed() {
    assert_eq!(format_rate(Some(1.5)), "+50%");
    assert_eq!(format_rate(Some(2.0)), "+100%");
}

#[test]
fn format_rate_negative_speed() {
    assert_eq!(format_rate(Some(0.5)), "-50%");
    assert_eq!(format_rate(Some(0.75)), "-25%");
}

#[test]
fn output_format_known_formats() {
    let (f, sr) = output_format_for(AudioFormat::Mp3).unwrap();
    assert_eq!(f, "audio-24khz-48kbitrate-mono-mp3");
    assert_eq!(sr, 24_000);
    let (f, sr) = output_format_for(AudioFormat::Wav).unwrap();
    assert_eq!(f, "riff-24khz-16bit-mono-pcm");
    assert_eq!(sr, 24_000);
    let (f, sr) = output_format_for(AudioFormat::Ogg).unwrap();
    assert_eq!(f, "ogg-24khz-16bit-mono-opus");
    assert_eq!(sr, 24_000);
    let (f, sr) = output_format_for(AudioFormat::Pcm16).unwrap();
    assert_eq!(f, "raw-24khz-16bit-mono-pcm");
    assert_eq!(sr, 24_000);
}

#[test]
fn output_format_other_rejected() {
    let err = output_format_for(AudioFormat::Other).unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[test]
fn request_id_is_32_lowercase_hex() {
    let id = new_request_id();
    assert_eq!(id.len(), 32);
    assert!(id
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
}

#[test]
fn config_frame_includes_format_and_path() {
    let f = build_config_frame(
        "Wed, 06 May 2026 19:00:00 GMT",
        "audio-24khz-48kbitrate-mono-mp3",
    );
    assert!(f.contains("Path:speech.config"));
    assert!(f.contains("X-Timestamp:Wed, 06 May 2026 19:00:00 GMT"));
    assert!(f.contains("Content-Type:application/json"));
    assert!(f.contains("\"outputFormat\":\"audio-24khz-48kbitrate-mono-mp3\""));
    // Body separated from headers by a blank CRLF line.
    assert!(f.contains("\r\n\r\n"));
}

#[test]
fn ssml_frame_has_namespace_and_voice() {
    let f = build_ssml_frame(
        "Wed, 06 May 2026 19:00:00 GMT",
        "abc123",
        "en-US-AriaNeural",
        "+0%",
        "Hello world",
    );
    assert!(f.contains("Path:ssml"));
    assert!(f.contains("X-RequestId:abc123"));
    assert!(f.contains("xmlns='http://www.w3.org/2001/10/synthesis'"));
    assert!(f.contains("name='en-US-AriaNeural'"));
    assert!(f.contains("rate='+0%'"));
    assert!(f.contains(">Hello world<"));
}

#[test]
fn ssml_frame_escapes_text() {
    let f = build_ssml_frame("T", "id", "v", "+0%", "Tom & Jerry <chase>");
    assert!(f.contains("Tom &amp; Jerry &lt;chase&gt;"));
    // Raw `<chase>` must not appear in the body.
    assert!(!f.contains(">Tom & Jerry <chase><"));
}

#[test]
fn parse_text_path_finds_path_line() {
    let frame = "X-RequestId:abc\r\nPath:turn.end\r\n\r\n";
    assert_eq!(parse_text_path(frame).as_deref(), Some("turn.end"));
}

#[test]
fn parse_text_path_no_path_returns_none() {
    let frame = "X-RequestId:abc\r\n\r\n";
    assert_eq!(parse_text_path(frame), None);
}

#[test]
fn parse_binary_audio_extracts_payload_after_audio_path() {
    let headers = b"X-RequestId:abc\r\nPath:audio\r\n\r\n";
    let mut frame = (headers.len() as u16).to_be_bytes().to_vec();
    frame.extend_from_slice(headers);
    let audio = b"\xff\xfb\x90\x00ID3";
    frame.extend_from_slice(audio);
    let payload = parse_binary_audio(&frame).unwrap();
    assert_eq!(payload, audio);
}

#[test]
fn parse_binary_audio_returns_empty_for_non_audio_path() {
    let headers = b"X-RequestId:abc\r\nPath:audio.metadata\r\n\r\n";
    let mut frame = (headers.len() as u16).to_be_bytes().to_vec();
    frame.extend_from_slice(headers);
    frame.extend_from_slice(&[0xde, 0xad]);
    let payload = parse_binary_audio(&frame).unwrap();
    assert!(payload.is_empty());
}

#[test]
fn parse_binary_audio_rejects_short_frame() {
    let err = parse_binary_audio(&[0u8]).unwrap_err();
    assert!(matches!(err, MediaError::Parse(_)));
}

#[test]
fn parse_binary_audio_rejects_header_overflow() {
    // header_len=0xFFFF but frame is only 5 bytes total.
    let frame = vec![0xff, 0xff, 1, 2, 3];
    let err = parse_binary_audio(&frame).unwrap_err();
    match err {
        MediaError::Parse(msg) => assert!(msg.contains("exceeds frame length")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_binary_audio_rejects_non_utf8_headers() {
    // header_len=4, headers contain invalid UTF-8.
    let frame = vec![0, 4, 0xff, 0xff, 0xff, 0xff, b'a'];
    let err = parse_binary_audio(&frame).unwrap_err();
    match err {
        MediaError::Parse(msg) => assert!(msg.contains("not utf-8")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn synthesize_rejects_empty_text() {
    let p = EdgeTtsProvider::new(EdgeTtsConfig::default());
    let err = p.synthesize(TtsRequest::new("")).await.unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[tokio::test]
async fn synthesize_rejects_audio_format_other() {
    let p = EdgeTtsProvider::new(EdgeTtsConfig::default());
    let mut r = TtsRequest::new("hi");
    r.format = Some(AudioFormat::Other);
    let err = p.synthesize(r).await.unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[tokio::test]
async fn synthesize_bad_base_url_is_invalid_request() {
    let mut cfg = EdgeTtsConfig::default();
    cfg.base_url = "http://[::1]:1?bad uri".to_string();
    let p = EdgeTtsProvider::new(cfg);
    let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
    match err {
        MediaError::InvalidRequest(msg) => assert!(msg.contains("bad base_url")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn synthesize_unreachable_base_url_is_transport_error() {
    let mut cfg = EdgeTtsConfig::default();
    // Routable but unreachable; tungstenite returns a transport
    // error rather than InvalidRequest.
    cfg.base_url = "ws://127.0.0.1:1/edge".to_string();
    cfg.request_timeout = Duration::from_millis(500);
    let p = EdgeTtsProvider::new(cfg);
    let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
    match err {
        MediaError::Transport(_) => {}
        other => panic!("expected Transport, got: {other:?}"),
    }
}

#[test]
fn sec_ms_gec_is_64_uppercase_hex() {
    // Pin a known timestamp to make this deterministic.
    let token = generate_sec_ms_gec(1_700_000_000);
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(token.chars().all(|c| !c.is_lowercase()));
}

#[test]
fn sec_ms_gec_is_5min_bucketed() {
    // Two timestamps inside the same 5-minute bucket → identical
    // tokens. (1_700_000_000 is at second 1_700_000_000 % 300 ==
    // 200 within its bucket, so adding 50 stays inside.)
    let a = generate_sec_ms_gec(1_700_000_000);
    let b = generate_sec_ms_gec(1_700_000_050);
    assert_eq!(a, b);
    // Crossing into the next bucket changes the token.
    let c = generate_sec_ms_gec(1_700_000_300);
    assert_ne!(a, c);
}

#[test]
fn build_endpoint_url_appends_with_ampersand_when_base_has_query() {
    let url = build_endpoint_url("wss://x?foo=bar", "abc", "DEAD", "1-1.0").unwrap();
    assert_eq!(
        url,
        "wss://x?foo=bar&ConnectionId=abc&Sec-MS-GEC=DEAD&Sec-MS-GEC-Version=1-1.0"
    );
}

#[test]
fn build_endpoint_url_appends_with_question_mark_when_base_has_no_query() {
    let url = build_endpoint_url("wss://x/edge/v1", "abc", "DEAD", "1-1.0").unwrap();
    assert_eq!(
        url,
        "wss://x/edge/v1?ConnectionId=abc&Sec-MS-GEC=DEAD&Sec-MS-GEC-Version=1-1.0"
    );
}

#[test]
fn build_endpoint_url_rejects_empty_base() {
    let err = build_endpoint_url("", "a", "b", "c").unwrap_err();
    match err {
        MediaError::InvalidRequest(msg) => assert!(msg.contains("base_url is empty")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn ssml_frame_x_timestamp_has_trailing_z() {
    // Edge server quirk — the SSML frame's X-Timestamp gets an
    // extra `Z` on the end. The speech.config frame does NOT.
    let f = build_ssml_frame(
        "Mon Jan 02 2006 15:04:05 GMT+0000 (UTC)",
        "rid",
        "v",
        "+0%",
        "x",
    );
    assert!(f.contains("X-Timestamp:Mon Jan 02 2006 15:04:05 GMT+0000 (UTC)Z\r\n"));
}

#[test]
fn config_frame_x_timestamp_has_no_trailing_z() {
    let f = build_config_frame(
        "Mon Jan 02 2006 15:04:05 GMT+0000 (UTC)",
        "audio-24khz-48kbitrate-mono-mp3",
    );
    assert!(f.contains("X-Timestamp:Mon Jan 02 2006 15:04:05 GMT+0000 (UTC)\r\n"));
    assert!(!f.contains("UTC)Z"));
}

#[test]
fn js_style_timestamp_format() {
    // Pinned: 2023-11-14 22:13:20 UTC.
    let s = utc_now_js_style(1_700_000_000);
    // Sanity checks — we don't pin the exact day-of-week to avoid
    // tz library drift, but the suffix must be exact.
    assert!(s.ends_with(" GMT+0000 (Coordinated Universal Time)"));
    assert!(s.contains("2023"));
    assert!(s.contains("Nov"));
}
