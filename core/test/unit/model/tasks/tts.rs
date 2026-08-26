use super::*;

fn cfg() -> TtsConfig {
    let mut c = TtsConfig::default();
    c.provider = "openai".into();
    c.model = "tts-1".into();
    c
}

#[test]
fn build_returns_none_when_disabled() {
    let mut c = TtsConfig::default();
    c.provider = "none".into();
    assert!(build_from(&c).unwrap().is_none());
}

#[test]
fn build_returns_err_for_unknown_provider() {
    let mut c = TtsConfig::default();
    c.provider = "unknown".into();
    assert!(build_from(&c).is_err());
}

#[test]
fn build_edge_alias_returns_edge_task() {
    let mut c = TtsConfig::default();
    c.provider = "edge".into();
    let t = build_from(&c).unwrap().expect("edge task");
    assert_eq!(t.name(), "edge-tts");
    // Edge has no API key requirement.
    assert!(t.is_configured());
}

#[test]
fn build_edge_tts_alias_returns_edge_task() {
    let mut c = TtsConfig::default();
    c.provider = "edge-tts".into();
    let t = build_from(&c).unwrap().expect("edge-tts task");
    assert_eq!(t.name(), "edge-tts");
}

#[test]
fn edge_task_reports_default_voice_as_model() {
    let mut c = TtsConfig::default();
    c.provider = "edge".into();
    c.default_voice = "en-GB-RyanNeural".into();
    let t = EdgeTtsTask::from_config(&c);
    assert_eq!(t.model(), "en-GB-RyanNeural");
}

#[test]
fn edge_task_falls_back_to_aria_when_voice_empty() {
    let mut c = TtsConfig::default();
    c.provider = "edge".into();
    c.default_voice = "".into();
    let t = EdgeTtsTask::from_config(&c);
    assert_eq!(t.model(), "en-US-AriaNeural");
}

#[test]
fn parse_audio_format_accepts_known() {
    use crate::agent::media::tts::AudioFormat;
    assert!(matches!(parse_audio_format("mp3"), Ok(AudioFormat::Mp3)));
    assert!(matches!(parse_audio_format("MP3"), Ok(AudioFormat::Mp3)));
    assert!(matches!(parse_audio_format("wav"), Ok(AudioFormat::Wav)));
    assert!(matches!(parse_audio_format("ogg"), Ok(AudioFormat::Ogg)));
    assert!(matches!(parse_audio_format("pcm"), Ok(AudioFormat::Pcm16)));
}

#[test]
fn parse_audio_format_rejects_unsupported() {
    let err = parse_audio_format("opus").unwrap_err();
    match err {
        TtsError::InvalidInput(msg) => assert!(msg.contains("opus")),
        other => panic!("unexpected: {other:?}"),
    }
    // OpenAI-compat formats Edge doesn't speak natively are
    // rejected, not silently coerced.
    assert!(parse_audio_format("aac").is_err());
    assert!(parse_audio_format("flac").is_err());
}

#[tokio::test]
async fn edge_task_synthesize_rejects_empty_text() {
    let mut c = TtsConfig::default();
    c.provider = "edge".into();
    let t = EdgeTtsTask::from_config(&c);
    let err = t
        .synthesize(TtsRequest {
            text: "".into(),
            voice: None,
            format: None,
            speed: None,
            instructions: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, TtsError::InvalidInput(_)));
}

#[tokio::test]
async fn edge_task_synthesize_rejects_unsupported_format() {
    let mut c = TtsConfig::default();
    c.provider = "edge".into();
    let t = EdgeTtsTask::from_config(&c);
    let err = t
        .synthesize(TtsRequest {
            text: "hi".into(),
            voice: None,
            format: Some("flac".into()),
            speed: None,
            instructions: None,
        })
        .await
        .unwrap_err();
    match err {
        TtsError::InvalidInput(msg) => assert!(msg.contains("flac")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn endpoint_default_path() {
    let mut c = cfg();
    c.base_url = Some("https://api.openai.com/v1".into());
    let t = OpenAICompatTts::from_config(&c);
    assert_eq!(t.endpoint(), "https://api.openai.com/v1/audio/speech");
}

#[test]
fn endpoint_handles_azure_query_string() {
    let mut c = cfg();
    c.base_url = Some(
        "https://account.openai.azure.com/openai/deployments/tts-1?api-version=2024-02-01"
            .into(),
    );
    let t = OpenAICompatTts::from_config(&c);
    assert_eq!(
        t.endpoint(),
        "https://account.openai.azure.com/openai/deployments/tts-1/audio/speech?api-version=2024-02-01"
    );
}

#[test]
fn classify_http_error_maps_codes() {
    assert!(matches!(classify_http_error(401, b"{}"), TtsError::Auth));
    assert!(matches!(
        classify_http_error(429, b"{}"),
        TtsError::RateLimited { .. }
    ));
    let p = classify_http_error(500, br#"{"error":{"message":"oops"}}"#);
    if let TtsError::Provider { status, message } = p {
        assert_eq!(status, 500);
        assert!(message.contains("oops"));
    }
}

#[tokio::test]
async fn synthesize_rejects_empty_text() {
    let t = OpenAICompatTts::from_config(&cfg());
    let err = t
        .synthesize(TtsRequest {
            text: "  ".into(),
            voice: None,
            format: None,
            speed: None,
            instructions: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, TtsError::InvalidInput(_)));
}

async fn spawn_one_shot_mock_binary(
    response_body: Vec<u8>,
    status_line: &'static str,
    content_type: &'static str,
) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}/v1");
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 16 * 1024];
        let mut total = Vec::new();
        loop {
            let n = sock.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            total.extend_from_slice(&buf[..n]);
            if total.windows(4).any(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&total);
                let body_start = total.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                let cl = head
                    .lines()
                    .find_map(|l| {
                        let l = l.to_ascii_lowercase();
                        l.strip_prefix("content-length:")
                            .map(|s| s.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if total.len() - body_start >= cl {
                    break;
                }
            }
        }
        let response = format!(
            "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response_body.len()
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.write_all(&response_body).await;
        let _ = sock.shutdown().await;
        total
    });
    (url, handle)
}

#[tokio::test]
async fn end_to_end_synthesize_round_trip() {
    std::env::set_var("COS_TEST_TTS_KEY", "sk-tts");
    // Pretend the response is an MP3. (4 bytes of fake audio.)
    let fake_audio = vec![0xff, 0xfb, 0x90, 0x44];
    let (base_url, handle) =
        spawn_one_shot_mock_binary(fake_audio.clone(), "HTTP/1.1 200 OK", "audio/mpeg").await;
    let mut c = cfg();
    c.base_url = Some(base_url);
    c.api_key_env = Some("COS_TEST_TTS_KEY".into());
    let t = OpenAICompatTts::from_config(&c);
    let resp = t
        .synthesize(TtsRequest {
            text: "Hello there.".into(),
            voice: Some("alloy".into()),
            format: Some("mp3".into()),
            speed: Some(1.0),
            instructions: None,
        })
        .await
        .expect("synthesize");
    assert_eq!(resp.audio, fake_audio);
    assert_eq!(resp.format, "mp3");

    let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
    assert!(req.contains("post /v1/audio/speech"));
    assert!(req.contains("authorization: bearer sk-tts"));
    assert!(req.contains("\"input\":\"hello there.\""));
    assert!(req.contains("\"voice\":\"alloy\""));
    assert!(req.contains("\"response_format\":\"mp3\""));
    assert!(req.contains("\"model\":\"tts-1\""));

    std::env::remove_var("COS_TEST_TTS_KEY");
}

#[tokio::test]
async fn azure_deployment_omits_model_field() {
    std::env::set_var("COS_TEST_TTS_KEY_2", "sk-tts2");
    let (base_url, handle) =
        spawn_one_shot_mock_binary(vec![1, 2, 3], "HTTP/1.1 200 OK", "audio/mpeg").await;
    let azure = format!("{base_url}/deployments/tts");
    let mut c = cfg();
    c.base_url = Some(azure);
    c.api_key_env = Some("COS_TEST_TTS_KEY_2".into());
    let t = OpenAICompatTts::from_config(&c);
    let _ = t
        .synthesize(TtsRequest {
            text: "x".into(),
            voice: None,
            format: None,
            speed: None,
            instructions: None,
        })
        .await
        .expect("synthesize");
    let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
    assert!(
        !req.contains("\"model\""),
        "Azure deployment shape must not send model field"
    );
    std::env::remove_var("COS_TEST_TTS_KEY_2");
}
