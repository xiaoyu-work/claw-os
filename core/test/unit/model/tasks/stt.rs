use super::*;

fn cfg() -> SttConfig {
    let mut c = SttConfig::default();
    c.provider = "openai".into();
    c.model = "whisper-1".into();
    c
}

#[test]
fn build_returns_none_when_disabled() {
    let mut c = SttConfig::default();
    c.provider = "none".into();
    assert!(build_from(&c).unwrap().is_none());
}

#[test]
fn build_returns_err_for_unknown_provider() {
    let mut c = SttConfig::default();
    c.provider = "unknown".into();
    assert!(build_from(&c).is_err());
}

#[test]
fn advertised_stt_providers_build_with_their_default_endpoints() {
    for (provider, expected) in [
        ("openai", DEFAULT_OPENAI_BASE),
        ("groq", DEFAULT_GROQ_BASE),
        ("mistral", DEFAULT_MISTRAL_BASE),
    ] {
        let mut c = cfg();
        c.provider = provider.into();
        c.base_url = None;
        assert!(build_from(&c).is_ok(), "{provider} should be supported");
        assert_eq!(OpenAICompatStt::from_config(&c).base_url, expected);
    }
}

#[test]
fn endpoint_path_changes_with_mode() {
    let mut c = cfg();
    c.base_url = Some("https://api.openai.com/v1".into());
    let s = OpenAICompatStt::from_config(&c);
    assert_eq!(
        s.endpoint(SttMode::Transcribe),
        "https://api.openai.com/v1/audio/transcriptions"
    );
    assert_eq!(
        s.endpoint(SttMode::Translate),
        "https://api.openai.com/v1/audio/translations"
    );
}

#[test]
fn endpoint_handles_azure_query_string() {
    let mut c = cfg();
    c.base_url = Some(
        "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/whisper?api-version=2024-02-01".into(),
    );
    let s = OpenAICompatStt::from_config(&c);
    assert_eq!(
        s.endpoint(SttMode::Transcribe),
        "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/whisper/audio/transcriptions?api-version=2024-02-01"
    );
    assert_eq!(
        s.endpoint(SttMode::Translate),
        "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/whisper/audio/translations?api-version=2024-02-01"
    );
}

#[test]
fn guess_mime_covers_common_audio_types() {
    assert_eq!(guess_mime("clip.mp3"), "audio/mpeg");
    assert_eq!(guess_mime("clip.wav"), "audio/wav");
    assert_eq!(guess_mime("clip.m4a"), "audio/mp4");
    assert_eq!(guess_mime("clip.flac"), "audio/flac");
    assert_eq!(guess_mime("clip.UNKNOWN"), "application/octet-stream");
}

#[test]
fn classify_http_error_maps_codes() {
    assert!(matches!(classify_http_error(401, b"{}"), SttError::Auth));
    assert!(matches!(
        classify_http_error(429, b"{}"),
        SttError::RateLimited { .. }
    ));
    let prov = classify_http_error(500, br#"{"error":{"message":"oops"}}"#);
    if let SttError::Provider { status, message } = prov {
        assert_eq!(status, 500);
        assert!(message.contains("oops"));
    }
}

#[tokio::test]
async fn transcribe_rejects_empty_audio() {
    let s = OpenAICompatStt::from_config(&cfg());
    let err = s
        .transcribe(SttRequest {
            audio: vec![],
            filename: "x.mp3".into(),
            language: None,
            prompt: None,
            response_format: None,
            temperature: None,
            mode: SttMode::Transcribe,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, SttError::InvalidInput(_)));
}

async fn spawn_one_shot_mock(
    response_body: String,
    status_line: &'static str,
) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}/v1");
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 64 * 1024];
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
                if cl > 0 && total.len() - body_start >= cl {
                    break;
                }
            }
        }
        let body = response_body.as_bytes();
        let response = format!(
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.write_all(body).await;
        let _ = sock.shutdown().await;
        total
    });
    (url, handle)
}

#[tokio::test]
async fn end_to_end_transcribe_round_trip() {
    std::env::set_var("COS_TEST_STT_KEY", "sk-stt");
    let body = serde_json::json!({"text": "hello world"}).to_string();
    let (base_url, handle) = spawn_one_shot_mock(body, "HTTP/1.1 200 OK").await;
    let mut c = cfg();
    c.base_url = Some(base_url);
    c.api_key_env = Some("COS_TEST_STT_KEY".into());
    let s = OpenAICompatStt::from_config(&c);
    let resp = s
        .transcribe(SttRequest {
            audio: b"fake-mp3-bytes".to_vec(),
            filename: "clip.mp3".into(),
            language: Some("en".into()),
            prompt: None,
            response_format: Some("json".into()),
            temperature: None,
            mode: SttMode::Transcribe,
        })
        .await
        .expect("transcribe");
    assert_eq!(resp.text, "hello world");

    let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
    assert!(req.contains("post /v1/audio/transcriptions"));
    assert!(req.contains("authorization: bearer sk-stt"));
    assert!(req.contains("content-type: multipart/form-data"));
    // Multipart body contains the model, language, file part.
    assert!(req.contains("name=\"model\""));
    assert!(req.contains("whisper-1"));
    assert!(req.contains("name=\"language\""));
    assert!(req.contains("name=\"file\""));
    assert!(req.contains("filename=\"clip.mp3\""));

    std::env::remove_var("COS_TEST_STT_KEY");
}

#[tokio::test]
async fn end_to_end_transcribe_text_format_returns_plain_text() {
    std::env::set_var("COS_TEST_STT_KEY_2", "sk-stt2");
    let body = "Just a transcript line.".to_string();
    let (base_url, _h) = spawn_one_shot_mock(body, "HTTP/1.1 200 OK").await;
    let mut c = cfg();
    c.base_url = Some(base_url);
    c.api_key_env = Some("COS_TEST_STT_KEY_2".into());
    let s = OpenAICompatStt::from_config(&c);
    let resp = s
        .transcribe(SttRequest {
            audio: b"fake".to_vec(),
            filename: "x.wav".into(),
            language: None,
            prompt: None,
            response_format: Some("text".into()),
            temperature: None,
            mode: SttMode::Transcribe,
        })
        .await
        .expect("transcribe");
    assert_eq!(resp.text, "Just a transcript line.");
    std::env::remove_var("COS_TEST_STT_KEY_2");
}

#[tokio::test]
async fn azure_deployment_omits_model_field_in_multipart() {
    std::env::set_var("COS_TEST_STT_KEY_3", "sk-stt3");
    let body = serde_json::json!({"text": "ok"}).to_string();
    let (base_url, handle) = spawn_one_shot_mock(body, "HTTP/1.1 200 OK").await;
    // Force the URL to look like an Azure deployment URL.
    let azure_url = format!("{base_url}/deployments/whisper");
    let mut c = cfg();
    c.base_url = Some(azure_url);
    c.api_key_env = Some("COS_TEST_STT_KEY_3".into());
    let s = OpenAICompatStt::from_config(&c);
    let _ = s
        .transcribe(SttRequest {
            audio: b"x".to_vec(),
            filename: "x.mp3".into(),
            language: None,
            prompt: None,
            response_format: Some("json".into()),
            temperature: None,
            mode: SttMode::Transcribe,
        })
        .await
        .expect("transcribe");
    let raw = handle.await.unwrap();
    let req = String::from_utf8_lossy(&raw);
    assert!(
        !req.contains("name=\"model\""),
        "Azure deployment shape must not send model field in multipart"
    );
    std::env::remove_var("COS_TEST_STT_KEY_3");
}
