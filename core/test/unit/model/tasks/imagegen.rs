use super::*;

fn cfg() -> ImageGenConfig {
    let mut c = ImageGenConfig::default();
    c.provider = "openai".into();
    c.model = "gpt-image-2".into();
    c
}

#[test]
fn build_returns_none_when_disabled() {
    let mut c = ImageGenConfig::default();
    c.provider = "none".into();
    assert!(build_from(&c).unwrap().is_none());
}

#[test]
fn build_returns_err_for_unknown_provider() {
    let mut c = ImageGenConfig::default();
    c.provider = "unknown".into();
    assert!(build_from(&c).is_err());
}

#[test]
fn endpoint_handles_query_string() {
    let mut c = cfg();
    c.base_url = Some(
        "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/gpt-image-2?api-version=2024-02-01".into(),
    );
    let g = OpenAICompatImageGen::from_config(&c);
    assert_eq!(
        g.endpoint(),
        "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/gpt-image-2/images/generations?api-version=2024-02-01"
    );
}

#[test]
fn endpoint_default_path() {
    let mut c = cfg();
    c.base_url = Some("https://api.openai.com/v1".into());
    let g = OpenAICompatImageGen::from_config(&c);
    assert_eq!(g.endpoint(), "https://api.openai.com/v1/images/generations");
}

#[test]
fn classify_http_error_maps_codes() {
    assert!(matches!(classify_http_error(401, b"{}"), ImageError::Auth));
    assert!(matches!(
        classify_http_error(429, b"{}"),
        ImageError::RateLimited { .. }
    ));
    let prov = classify_http_error(500, br#"{"error":{"message":"oops"}}"#);
    if let ImageError::Provider { status, message } = prov {
        assert_eq!(status, 500);
        assert!(message.contains("oops"));
    } else {
        panic!("expected Provider");
    }
}

#[tokio::test]
async fn generate_rejects_empty_prompt() {
    let g = OpenAICompatImageGen::from_config(&cfg());
    let err = g
        .generate(ImageGenRequest {
            prompt: String::new(),
            size: None,
            quality: None,
            n: 1,
            format: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, ImageError::InvalidInput(_)));
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
async fn end_to_end_image_generation_round_trip() {
    std::env::set_var("COS_TEST_IMAGE_KEY", "sk-img");
    let body = serde_json::json!({
        "data": [{"b64_json": "iVBORw0KGgo="}]
    })
    .to_string();
    let (base_url, handle) = spawn_one_shot_mock(body, "HTTP/1.1 200 OK").await;

    let mut c = cfg();
    c.base_url = Some(base_url);
    c.api_key_env = Some("COS_TEST_IMAGE_KEY".into());
    let g = OpenAICompatImageGen::from_config(&c);
    let resp = g
        .generate(ImageGenRequest {
            prompt: "a red fox in autumn".into(),
            size: Some("1024x1024".into()),
            quality: Some("medium".into()),
            n: 1,
            format: Some("png".into()),
        })
        .await
        .expect("generate");
    assert_eq!(resp.images.len(), 1);
    match &resp.images[0] {
        ImageData::Base64 { data } => assert_eq!(data, "iVBORw0KGgo="),
        other => panic!("expected base64, got {other:?}"),
    }

    let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
    assert!(req.contains("post /v1/images/generations"));
    assert!(req.contains("authorization: bearer sk-img"));
    assert!(req.contains("\"prompt\":\"a red fox in autumn\""));
    assert!(req.contains("\"size\":\"1024x1024\""));
    assert!(req.contains("\"quality\":\"medium\""));
    assert!(req.contains("\"output_format\":\"png\""));
    // Mock URL has no /deployments/ → model field IS sent (stock OpenAI shape).
    assert!(req.contains("\"model\":\"gpt-image-2\""));
    // png/jpeg → output_compression added.
    assert!(req.contains("\"output_compression\":100"));

    std::env::remove_var("COS_TEST_IMAGE_KEY");
}

#[tokio::test]
async fn end_to_end_image_generation_azure_omits_model() {
    std::env::set_var("COS_TEST_IMAGE_KEY_2", "sk-img-az");
    let body = serde_json::json!({
        "data": [{"b64_json": "iVBORw0KGgo="}]
    })
    .to_string();
    // Force the URL to look like an Azure deployment URL.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let azure_style_base = format!("http://{addr}/openai/deployments/gpt-image-2");
    let handle = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        let body_bytes = body.as_bytes();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body_bytes.len()
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.write_all(body_bytes).await;
        let _ = sock.shutdown().await;
        total
    });

    let mut c = cfg();
    c.base_url = Some(azure_style_base);
    c.api_key_env = Some("COS_TEST_IMAGE_KEY_2".into());
    let g = OpenAICompatImageGen::from_config(&c);
    let _ = g
        .generate(ImageGenRequest {
            prompt: "test".into(),
            size: Some("1024x1024".into()),
            quality: Some("medium".into()),
            n: 1,
            format: Some("png".into()),
        })
        .await
        .expect("generate");

    let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
    // Azure deployment shape → no `model` key in body.
    assert!(
        !req.contains("\"model\""),
        "Azure deployment shape must not send model field"
    );
    assert!(req.contains("\"prompt\":\"test\""));

    std::env::remove_var("COS_TEST_IMAGE_KEY_2");
}
