use super::*;

use async_trait::async_trait;
use futures_util::stream::BoxStream;

use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, FinishReason, Provider, Result as LlmResult, Role,
    StreamEvent, Usage,
};

#[test]
fn media_type_for_known_mimes() {
    assert_eq!(media_type_for(ImageMime::Png), "image/png");
    assert_eq!(media_type_for(ImageMime::Jpeg), "image/jpeg");
    assert_eq!(media_type_for(ImageMime::Webp), "image/webp");
    assert_eq!(media_type_for(ImageMime::Gif), "image/gif");
    assert_eq!(media_type_for(ImageMime::Bmp), "image/bmp");
    assert_eq!(media_type_for(ImageMime::Tiff), "image/tiff");
    assert_eq!(media_type_for(ImageMime::Heic), "image/heic");
    assert_eq!(media_type_for(ImageMime::Other), "image/png");
}

#[test]
fn sniff_mime_recognises_common_formats() {
    assert_eq!(sniff_mime(b"\x89PNG\r\n\x1a\n..."), ImageMime::Png);
    assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), ImageMime::Jpeg);
    assert_eq!(sniff_mime(b"GIF89a..."), ImageMime::Gif);
    assert_eq!(sniff_mime(b"GIF87a..."), ImageMime::Gif);
    assert_eq!(sniff_mime(b"BMxxx"), ImageMime::Bmp);
}

#[test]
fn sniff_mime_recognises_webp() {
    let mut data = Vec::new();
    data.extend_from_slice(b"RIFF");
    data.extend_from_slice(&[0; 4]);
    data.extend_from_slice(b"WEBP");
    data.extend_from_slice(b"VP8 ");
    assert_eq!(sniff_mime(&data), ImageMime::Webp);
}

#[test]
fn sniff_mime_unknown_falls_to_other() {
    assert_eq!(sniff_mime(b"random data"), ImageMime::Other);
    assert_eq!(sniff_mime(b""), ImageMime::Other);
}

#[test]
fn validate_rejects_empty_prompt() {
    let req = VisionRequest::new(
        "  ",
        ImageInput::Url("https://example.com/x.png".to_string()),
    );
    assert!(req.validate().is_err());
}

#[test]
fn validate_rejects_empty_url() {
    let req = VisionRequest::new("describe", ImageInput::Url(String::new()));
    assert!(req.validate().is_err());
}

#[test]
fn validate_rejects_non_http_url() {
    let req = VisionRequest::new(
        "describe",
        ImageInput::Url("file:///etc/passwd".to_string()),
    );
    let err = req.validate().unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[test]
fn validate_rejects_plain_http_url() {
    // After the SSRF fix vision now refuses cleartext fetches so
    // a downgrade attack on a redirected provider response can't
    // pull a cleartext URL through this surface.
    let req = VisionRequest::new(
        "describe",
        ImageInput::Url("http://example.com/x.png".to_string()),
    );
    let err = req.validate().unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[tokio::test]
async fn ssrf_blocked() {
    // fetch_image must refuse loopback / RFC1918 / link-local /
    // private-v6 targets even when the URL passes the simple
    // scheme check. We hit each rejection class through the
    // real fetch entry point so any future refactor that
    // bypasses `assert_safe_outbound` fails this test.
    let timeout = std::time::Duration::from_secs(1);
    for url in &[
        "https://127.0.0.1/x.png",
        "https://10.0.0.1/x.png",
        "https://192.168.1.1/x.png",
        "https://169.254.169.254/latest/meta-data/",
        "https://[::1]/x.png",
    ] {
        let err = fetch_image(url, timeout).await.unwrap_err();
        assert!(
            matches!(err, MediaError::InvalidRequest(_)),
            "url={url} err={err:?}"
        );
    }
}

#[test]
fn validate_accepts_https_url() {
    let req = VisionRequest::new(
        "describe",
        ImageInput::Url("https://example.com/x.png".to_string()),
    );
    assert!(req.validate().is_ok());
}

#[test]
fn validate_rejects_empty_bytes() {
    let req = VisionRequest::new(
        "describe",
        ImageInput::Bytes {
            data: vec![],
            mime: ImageMime::Png,
        },
    );
    assert!(req.validate().is_err());
}

#[test]
fn validate_rejects_empty_base64() {
    let req = VisionRequest::new(
        "describe",
        ImageInput::Base64 {
            data: String::new(),
            mime: ImageMime::Png,
        },
    );
    assert!(req.validate().is_err());
}

#[test]
fn build_user_message_orders_image_then_text() {
    let m = build_user_message("describe", ImageMime::Png, "AAAA");
    assert_eq!(m.role, Role::User);
    assert_eq!(m.content.len(), 2);
    match &m.content[0] {
        ContentBlock::Image { media_type, data } => {
            assert_eq!(media_type, "image/png");
            assert_eq!(data, "AAAA");
        }
        _ => panic!("expected Image first"),
    }
    match &m.content[1] {
        ContentBlock::Text { text } => assert_eq!(text, "describe"),
        _ => panic!("expected Text second"),
    }
}

#[tokio::test]
async fn materialise_base64_passes_through() {
    let (mime, b64) = materialise(
        ImageInput::Base64 {
            data: "hello".to_string(),
            mime: ImageMime::Webp,
        },
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(mime, ImageMime::Webp);
    assert_eq!(b64, "hello");
}

#[tokio::test]
async fn materialise_bytes_encodes_base64() {
    let (mime, b64) = materialise(
        ImageInput::Bytes {
            data: b"hi".to_vec(),
            mime: ImageMime::Jpeg,
        },
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(mime, ImageMime::Jpeg);
    assert_eq!(b64, BASE64.encode(b"hi"));
}

// ---- analyze() exercised against an in-process Provider stub ----

struct StubVisionProvider {
    configured: bool,
    reply: String,
    captured: std::sync::Mutex<Option<ChatRequest>>,
}

#[async_trait]
impl Provider for StubVisionProvider {
    fn name(&self) -> &str {
        "stub-vision"
    }
    fn supported_models(&self) -> Vec<String> {
        vec!["stub-vision".to_string()]
    }
    fn is_configured(&self) -> bool {
        self.configured
    }
    async fn chat(&self, request: ChatRequest) -> LlmResult<ChatResponse> {
        *self.captured.lock().unwrap() = Some(request);
        Ok(ChatResponse {
            content: vec![ContentBlock::Text {
                text: self.reply.clone(),
            }],
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
            model: "stub-vision".to_string(),
        })
    }
    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> LlmResult<BoxStream<'static, LlmResult<StreamEvent>>> {
        unimplemented!()
    }
}

#[tokio::test]
async fn analyze_dispatches_with_image_block_and_returns_text() {
    let p = StubVisionProvider {
        configured: true,
        reply: "a small black cat".to_string(),
        captured: std::sync::Mutex::new(None),
    };
    let req = VisionRequest::new(
        "what's in this image?",
        ImageInput::Bytes {
            data: vec![0xFF, 0xD8, 0xFF, 0xE0],
            mime: ImageMime::Jpeg,
        },
    );
    let resp = analyze(&p, req, Duration::from_secs(5)).await.unwrap();
    assert_eq!(resp.text, "a small black cat");
    assert_eq!(resp.model.as_deref(), Some("stub-vision"));

    let captured = p.captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured.messages.len(), 1);
    let blocks = &captured.messages[0].content;
    assert!(matches!(blocks[0], ContentBlock::Image { .. }));
    assert!(matches!(blocks[1], ContentBlock::Text { .. }));
}

#[tokio::test]
async fn analyze_includes_system_prompt() {
    let p = StubVisionProvider {
        configured: true,
        reply: "ok".to_string(),
        captured: std::sync::Mutex::new(None),
    };
    let mut req = VisionRequest::new(
        "what is this?",
        ImageInput::Base64 {
            data: "AAAA".to_string(),
            mime: ImageMime::Png,
        },
    );
    req.system = Some("You are a helpful vision assistant.".to_string());
    analyze(&p, req, Duration::from_secs(1)).await.unwrap();

    let captured = p.captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured.messages.len(), 2);
    assert_eq!(captured.messages[0].role, Role::System);
    assert_eq!(captured.messages[1].role, Role::User);
}

#[tokio::test]
async fn analyze_skips_blank_system() {
    let p = StubVisionProvider {
        configured: true,
        reply: "ok".to_string(),
        captured: std::sync::Mutex::new(None),
    };
    let mut req = VisionRequest::new(
        "what is this?",
        ImageInput::Base64 {
            data: "AAAA".to_string(),
            mime: ImageMime::Png,
        },
    );
    req.system = Some("   ".to_string());
    analyze(&p, req, Duration::from_secs(1)).await.unwrap();

    let captured = p.captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured.messages.len(), 1);
    assert_eq!(captured.messages[0].role, Role::User);
}

#[tokio::test]
async fn analyze_errors_when_provider_unconfigured() {
    let p = StubVisionProvider {
        configured: false,
        reply: String::new(),
        captured: std::sync::Mutex::new(None),
    };
    let req = VisionRequest::new(
        "x",
        ImageInput::Base64 {
            data: "AAAA".to_string(),
            mime: ImageMime::Png,
        },
    );
    let err = analyze(&p, req, Duration::from_secs(1)).await.unwrap_err();
    assert!(matches!(err, MediaError::NotConfigured(_)));
}

#[tokio::test]
async fn analyze_validates_request_first() {
    let p = StubVisionProvider {
        configured: true,
        reply: String::new(),
        captured: std::sync::Mutex::new(None),
    };
    let req = VisionRequest::new(
        "",
        ImageInput::Base64 {
            data: "AAAA".to_string(),
            mime: ImageMime::Png,
        },
    );
    let err = analyze(&p, req, Duration::from_secs(1)).await.unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}
