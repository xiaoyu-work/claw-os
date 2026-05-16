//! Vision analysis tool.
//!
//! Takes an image (URL, raw bytes, or already-base64) plus a
//! prompt, dispatches to a vision-capable LLM provider, and
//! returns the model's text response.
//!
//! Responsibilities split:
//!   * `routing` (sibling module) decides whether vision should
//!     be used at all (capability checks, MIME support, OCR
//!     fallback).
//!   * `analyze` (this module) is invoked once routing has
//!     concluded "yes, do native vision". It owns: URL fetch,
//!     base64 encoding, building the multimodal LLM message,
//!     and dispatching the chat call.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use crate::agent::llm::{ChatRequest, ContentBlock, Message, Provider, Role};

use super::super::MediaError;
use super::routing::ImageMime;

#[derive(Debug, Clone)]
pub enum ImageInput {
    /// Remote URL — provider will download + base64-encode.
    Url(String),
    /// Already-decoded raw bytes (e.g. read from disk).
    Bytes { data: Vec<u8>, mime: ImageMime },
    /// Pre-encoded base64 string with explicit MIME (cheap path
    /// when the caller already has the image in this form).
    Base64 { data: String, mime: ImageMime },
}

#[derive(Debug, Clone)]
pub struct VisionRequest {
    pub prompt: String,
    pub image: ImageInput,
    /// Optional system prompt prepended to the chat request.
    pub system: Option<String>,
    /// Cap on response length passed through to the LLM.
    pub max_tokens: Option<u32>,
}

impl VisionRequest {
    pub fn new(prompt: impl Into<String>, image: ImageInput) -> Self {
        Self {
            prompt: prompt.into(),
            image,
            system: None,
            max_tokens: None,
        }
    }

    pub fn validate(&self) -> Result<(), MediaError> {
        if self.prompt.trim().is_empty() {
            return Err(MediaError::InvalidRequest(
                "vision: prompt must be non-empty".to_string(),
            ));
        }
        match &self.image {
            ImageInput::Url(u) => {
                if u.trim().is_empty() {
                    return Err(MediaError::InvalidRequest(
                        "vision: image url must be non-empty".to_string(),
                    ));
                }
                if !u.starts_with("https://") {
                    return Err(MediaError::InvalidRequest(format!(
                        "vision: image url must be https: {u}"
                    )));
                }
            }
            ImageInput::Bytes { data, .. } => {
                if data.is_empty() {
                    return Err(MediaError::InvalidRequest(
                        "vision: image bytes must be non-empty".to_string(),
                    ));
                }
            }
            ImageInput::Base64 { data, .. } => {
                if data.is_empty() {
                    return Err(MediaError::InvalidRequest(
                        "vision: base64 image must be non-empty".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VisionResponse {
    pub text: String,
    pub model: Option<String>,
}

/// Map our [`ImageMime`] to the wire `media_type` string used in
/// [`ContentBlock::Image`]. Unknown MIMEs default to `image/png`
/// because every major vision provider accepts PNG.
pub fn media_type_for(mime: ImageMime) -> &'static str {
    match mime {
        ImageMime::Png => "image/png",
        ImageMime::Jpeg => "image/jpeg",
        ImageMime::Webp => "image/webp",
        ImageMime::Gif => "image/gif",
        ImageMime::Bmp => "image/bmp",
        ImageMime::Tiff => "image/tiff",
        ImageMime::Heic => "image/heic",
        ImageMime::Other => "image/png",
    }
}

/// Sniff the MIME from the leading magic bytes. PNG, JPEG, GIF,
/// WEBP cover ~all real-world web images. Used when the caller
/// has fetched a URL but not parsed the Content-Type header.
pub fn sniff_mime(bytes: &[u8]) -> ImageMime {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ImageMime::Png
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ImageMime::Jpeg
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        ImageMime::Gif
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        ImageMime::Webp
    } else if bytes.starts_with(b"BM") {
        ImageMime::Bmp
    } else {
        ImageMime::Other
    }
}

/// Build the user [`Message`] containing the prompt + image
/// content blocks. Order matters: image first, then text — that's
/// the layout Anthropic and OpenAI both prefer.
pub fn build_user_message(prompt: &str, mime: ImageMime, base64_data: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![
            ContentBlock::Image {
                media_type: media_type_for(mime).to_string(),
                data: base64_data.to_string(),
            },
            ContentBlock::Text {
                text: prompt.to_string(),
            },
        ],
    }
}

/// Download a remote image and return (bytes, sniffed_mime).
///
/// SSRF-hardened: rejects non-public targets (loopback / RFC1918 /
/// link-local / IPv6 unique-local) and caps the response body so a
/// hostile content server can't OOM the agent by streaming an
/// unbounded payload. Forces HTTPS — agent vision is an opt-in
/// outbound surface and there's no legitimate reason to allow
/// cleartext image fetches.
pub async fn fetch_image(url: &str, timeout: Duration) -> Result<(Vec<u8>, ImageMime), MediaError> {
    super::super::util::assert_safe_outbound(url, true)?;
    let mut builder =
        reqwest::Client::builder().user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")));
    if timeout > Duration::from_secs(0) {
        builder = builder.timeout(timeout);
    }
    let client = builder
        .build()
        .map_err(|e| MediaError::Internal(e.to_string()))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| MediaError::Transport(e.to_string()))?;
    let status = resp.status();
    let ct_hint = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(|s| ImageMime::from_str(s));
    let bytes = super::super::util::read_bytes_capped(
        resp,
        super::super::util::MAX_BINARY_BODY_BYTES,
        "vision::fetch_image",
    )
    .await?;
    if !status.is_success() {
        let preview = String::from_utf8_lossy(&bytes);
        let preview = super::super::util::preview(&preview, 256);
        return Err(MediaError::Provider {
            status: status.as_u16(),
            message: preview,
        });
    }
    let mime = ct_hint
        .filter(|m| !matches!(m, ImageMime::Other))
        .unwrap_or_else(|| sniff_mime(&bytes));
    Ok((bytes.to_vec(), mime))
}

/// Materialise an [`ImageInput`] into (mime, base64). Performs
/// the URL fetch + encode when needed; passes pre-encoded data
/// through unchanged.
pub async fn materialise(
    input: ImageInput,
    fetch_timeout: Duration,
) -> Result<(ImageMime, String), MediaError> {
    match input {
        ImageInput::Base64 { data, mime } => Ok((mime, data)),
        ImageInput::Bytes { data, mime } => Ok((mime, BASE64.encode(&data))),
        ImageInput::Url(u) => {
            let (bytes, mime) = fetch_image(&u, fetch_timeout).await?;
            Ok((mime, BASE64.encode(&bytes)))
        }
    }
}

/// End-to-end analysis. Resolves the image into base64, builds
/// a multimodal chat request, dispatches via the supplied
/// [`Provider`], and returns the assistant's text response.
pub async fn analyze(
    provider: &dyn Provider,
    request: VisionRequest,
    fetch_timeout: Duration,
) -> Result<VisionResponse, MediaError> {
    request.validate()?;
    if !provider.is_configured() {
        return Err(MediaError::NotConfigured(provider.name().to_string()));
    }

    let (mime, b64) = materialise(request.image, fetch_timeout).await?;
    let user = build_user_message(&request.prompt, mime, &b64);

    let mut messages = Vec::new();
    if let Some(sys) = request.system.as_ref() {
        if !sys.trim().is_empty() {
            messages.push(Message::system_text(sys.clone()));
        }
    }
    messages.push(user);

    let chat = ChatRequest {
        model: provider
            .supported_models()
            .into_iter()
            .next()
            .unwrap_or_else(|| provider.name().to_string()),
        messages,
        system: None,
        tools: vec![],
        tool_choice: Default::default(),
        max_tokens: request.max_tokens,
        temperature: None,
        top_p: None,
        stop_sequences: vec![],
        extra: serde_json::Value::Null,
    };

    let resp = provider
        .chat(chat)
        .await
        .map_err(|e| MediaError::Internal(format!("vision chat failed: {e}")))?;

    let text = resp
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(VisionResponse {
        text,
        model: Some(provider.name().to_string()),
    })
}

#[cfg(test)]
mod tests {
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
}
