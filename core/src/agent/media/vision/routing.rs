//! Image input routing.
//!
//! When the agent encounters an image attachment, it has to decide:
//!
//!   * Send the image natively to a vision-capable LLM (best
//!     fidelity, but more tokens / not all providers support it).
//!   * OCR the image and pass the extracted text (cheaper, but
//!     loses spatial / non-text content).
//!   * Skip the image entirely (oversized, unsupported format,
//!     vision disabled).
//!
//! This module is the policy layer. Caller supplies
//! [`ImageDescriptor`] (size + mime + intent + provider
//! capabilities) and gets back a [`RoutingDecision`].
//!
//! The analyze tool (future) consumes the decision and either
//! shells out to a vision LLM or routes through `cos model` for
//! local OCR.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMime {
    Png,
    Jpeg,
    Webp,
    Gif,
    Bmp,
    Tiff,
    Heic,
    Other,
}

impl ImageMime {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "image/png" | "png" => Self::Png,
            "image/jpeg" | "image/jpg" | "jpeg" | "jpg" => Self::Jpeg,
            "image/webp" | "webp" => Self::Webp,
            "image/gif" | "gif" => Self::Gif,
            "image/bmp" | "bmp" => Self::Bmp,
            "image/tiff" | "tiff" | "tif" => Self::Tiff,
            "image/heic" | "image/heif" | "heic" | "heif" => Self::Heic,
            _ => Self::Other,
        }
    }

    /// Is this MIME widely accepted by major vision providers?
    /// Anthropic, OpenAI, Gemini all accept PNG/JPEG/WEBP/GIF.
    pub fn is_widely_supported(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg | Self::Webp | Self::Gif)
    }
}

#[derive(Debug, Clone)]
pub struct ImageDescriptor {
    pub bytes_len: usize,
    pub mime: ImageMime,
    /// Caller hint about why this image is in scope: caption,
    /// extract_text, identify, none. Affects routing.
    pub intent: ImageIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageIntent {
    /// Generic "look at this".
    General,
    /// Caller specifically wants text extracted (OCR-friendly path).
    ExtractText,
    /// Caller asks "what is this".
    Identify,
    /// Description for accessibility / alt-text.
    Caption,
}

#[derive(Debug, Clone)]
pub struct RoutingPolicy {
    /// True if the active LLM provider can natively accept images.
    pub provider_supports_vision: bool,
    /// True if vision is enabled by user config / approval.
    pub vision_enabled: bool,
    /// Maximum bytes to send natively (avoid blowing up token cost
    /// on multi-MB images).
    pub max_native_bytes: usize,
    /// True if a local OCR backend is available (cos model OCR or
    /// equivalent).
    pub ocr_available: bool,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            provider_supports_vision: false,
            vision_enabled: true,
            max_native_bytes: 5 * 1024 * 1024,
            ocr_available: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingDecision {
    Native,
    Ocr,
    Skip { reason: String },
}

/// Decide how to route an image to the LLM.
///
/// Decision tree (in order):
///
/// 1. Vision globally disabled or zero-byte image -> Skip.
/// 2. Unsupported MIME and no OCR backend -> Skip.
/// 3. Caller explicitly asked for text extraction AND OCR is
///    available -> Ocr (even if vision could do it, the user
///    said they want text).
/// 4. Provider supports vision AND payload fits AND MIME is
///    widely supported -> Native.
/// 5. OCR backend available -> Ocr.
/// 6. Otherwise -> Skip with a reason explaining what's missing.
pub fn route(descriptor: &ImageDescriptor, policy: &RoutingPolicy) -> RoutingDecision {
    if !policy.vision_enabled {
        return RoutingDecision::Skip {
            reason: "vision disabled by policy".to_string(),
        };
    }
    if descriptor.bytes_len == 0 {
        return RoutingDecision::Skip {
            reason: "image payload empty".to_string(),
        };
    }
    if !descriptor.mime.is_widely_supported() && !policy.ocr_available {
        return RoutingDecision::Skip {
            reason: format!(
                "mime {:?} not natively supported and no OCR backend available",
                descriptor.mime
            ),
        };
    }
    if descriptor.intent == ImageIntent::ExtractText && policy.ocr_available {
        return RoutingDecision::Ocr;
    }
    if policy.provider_supports_vision
        && descriptor.bytes_len <= policy.max_native_bytes
        && descriptor.mime.is_widely_supported()
    {
        return RoutingDecision::Native;
    }
    if policy.ocr_available {
        return RoutingDecision::Ocr;
    }
    RoutingDecision::Skip {
        reason: if !policy.provider_supports_vision {
            "provider has no vision and no OCR backend available".to_string()
        } else if descriptor.bytes_len > policy.max_native_bytes {
            format!(
                "image {} bytes exceeds native cap {}; no OCR backend available",
                descriptor.bytes_len, policy.max_native_bytes
            )
        } else {
            "no native or OCR path available".to_string()
        },
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/media/vision/routing.rs"
    ));
}
