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
    let parsed_url = reqwest::Url::parse(url)
        .map_err(|e| MediaError::InvalidRequest(format!("invalid image url: {e}")))?;
    let client = super::super::util::build_safe_client(&parsed_url, timeout).await?;
    let resp = client
        .get(parsed_url)
        .send()
        .await
        .map_err(|e| MediaError::Transport(e.to_string()))?;
    let status = resp.status();
    let ct_hint = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(ImageMime::from_str);
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
        extra: serde_json::json!({"_cos_initiator": "agent"}),
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/media/vision/analyze.rs"
    ));
}
