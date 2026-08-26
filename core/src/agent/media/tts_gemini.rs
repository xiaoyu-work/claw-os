//! Gemini native TTS provider — `gemini-2.5-flash-preview-tts` and
//! its siblings expose audio synthesis through the same
//! `generateContent` shape as text generation, except the response
//! parts contain `inlineData` with `mimeType: audio/L16;...` plus
//! base64 PCM bytes.
//!
//! Endpoint:
//! `POST {base}/v1beta/models/{model}:generateContent`
//! Headers: `x-goog-api-key: <key>`, `Content-Type: application/json`
//!
//! Request:
//! ```json
//! {
//!   "contents": [{ "parts": [{ "text": "..." }] }],
//!   "generationConfig": {
//!     "responseModalities": ["AUDIO"],
//!     "speechConfig": {
//!       "voiceConfig": {
//!         "prebuiltVoiceConfig": { "voiceName": "Kore" }
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! Response:
//! ```json
//! {
//!   "candidates": [{
//!     "content": {
//!       "parts": [{
//!         "inlineData": {
//!           "mimeType": "audio/L16;codec=pcm;rate=24000",
//!           "data": "<base64 PCM-16LE>"
//!         }
//!       }]
//!     }
//!   }]
//! }
//! ```
//!
//! The provider returns `AudioFormat::Pcm16` with the parsed sample
//! rate; the bytes are raw little-endian PCM. Callers that need
//! WAV can wrap with `voice::wav::pcm16_to_wav` (TODO once the
//! WAV helper grows that signature).
//!
//! Configuration ([`GeminiTtsConfig`]):
//!   * `api_key`            sent as `x-goog-api-key`.
//!   * `model`              defaults to `gemini-2.5-flash-preview-tts`.
//!   * `default_voice`      preset voice name (e.g. "Kore", "Puck").
//!   * `base_url`           override; defaults to `https://generativelanguage.googleapis.com`.
//!   * `extra_headers`      pass-through.
//!   * `request_timeout`    per-call timeout.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::tts::{AudioFormat, TtsProvider, TtsRequest, TtsResponse};
use super::MediaError;

const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";
const DEFAULT_MODEL: &str = "gemini-2.5-flash-preview-tts";
const DEFAULT_VOICE: &str = "Kore";
const PROVIDER_NAME: &str = "gemini-tts";

#[derive(Debug, Clone)]
pub struct GeminiTtsConfig {
    pub api_key: Option<String>,
    pub model: String,
    pub default_voice: Option<String>,
    pub base_url: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
}

impl Default for GeminiTtsConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            model: DEFAULT_MODEL.to_string(),
            default_voice: Some(DEFAULT_VOICE.to_string()),
            base_url: DEFAULT_BASE.to_string(),
            extra_headers: HashMap::new(),
            request_timeout: Duration::from_secs(60),
        }
    }
}

pub struct GeminiTts {
    cfg: GeminiTtsConfig,
}

impl GeminiTts {
    pub fn new(cfg: GeminiTtsConfig) -> Self {
        // Per-request client built via `util::build_safe_client` so the
        // hostname is DNS-pinned to a vetted public IP, defeating
        // rebinding races. See `media/util.rs`.
        Self { cfg }
    }

    fn endpoint(&self) -> String {
        let base = self.cfg.base_url.trim_end_matches('/');
        let model = &self.cfg.model;
        format!("{base}/v1beta/models/{model}:generateContent")
    }
}

#[derive(Debug, Serialize)]
struct WireRequest<'a> {
    contents: Vec<Content<'a>>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig<'a>,
}

#[derive(Debug, Serialize)]
struct Content<'a> {
    parts: Vec<Part<'a>>,
}

#[derive(Debug, Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Debug, Serialize)]
struct GenerationConfig<'a> {
    #[serde(rename = "responseModalities")]
    response_modalities: Vec<&'a str>,
    #[serde(rename = "speechConfig", skip_serializing_if = "Option::is_none")]
    speech_config: Option<SpeechConfig<'a>>,
}

#[derive(Debug, Serialize)]
struct SpeechConfig<'a> {
    #[serde(rename = "voiceConfig")]
    voice_config: VoiceConfig<'a>,
}

#[derive(Debug, Serialize)]
struct VoiceConfig<'a> {
    #[serde(rename = "prebuiltVoiceConfig")]
    prebuilt_voice_config: PrebuiltVoice<'a>,
}

#[derive(Debug, Serialize)]
struct PrebuiltVoice<'a> {
    #[serde(rename = "voiceName")]
    voice_name: &'a str,
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default)]
    error: Option<ErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Option<RespContent>,
}

#[derive(Debug, Deserialize)]
struct RespContent {
    #[serde(default)]
    parts: Vec<RespPart>,
}

#[derive(Debug, Deserialize)]
struct RespPart {
    #[serde(default, rename = "inlineData")]
    inline_data: Option<InlineData>,
}

#[derive(Debug, Deserialize)]
struct InlineData {
    #[serde(default, rename = "mimeType")]
    mime_type: String,
    #[serde(default)]
    data: String,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
    #[serde(default)]
    status: String,
}

#[async_trait]
impl TtsProvider for GeminiTts {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn is_configured(&self) -> bool {
        self.cfg.api_key.is_some()
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, MediaError> {
        request.validate()?;
        if self.cfg.api_key.is_none() {
            return Err(MediaError::NotConfigured(PROVIDER_NAME.to_string()));
        }
        let voice = request
            .voice
            .as_deref()
            .or(self.cfg.default_voice.as_deref())
            .unwrap_or(DEFAULT_VOICE);

        let body = WireRequest {
            contents: vec![Content {
                parts: vec![Part {
                    text: &request.text,
                }],
            }],
            generation_config: GenerationConfig {
                response_modalities: vec!["AUDIO"],
                speech_config: Some(SpeechConfig {
                    voice_config: VoiceConfig {
                        prebuilt_voice_config: PrebuiltVoice { voice_name: voice },
                    },
                }),
            },
        };

        let mut http = {
            let endpoint = self.endpoint();
            let url = reqwest::Url::parse(&endpoint)
                .map_err(|e| MediaError::InvalidRequest(format!("invalid endpoint url: {e}")))?;
            let client = super::util::build_safe_client(&url, self.cfg.request_timeout).await?;
            client
                .post(url)
                .header("Content-Type", "application/json")
                .json(&body)
        };
        if let Some(key) = &self.cfg.api_key {
            http = http.header("x-goog-api-key", key.as_str());
        }
        for (k, v) in &self.cfg.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }

        let resp = http
            .send()
            .await
            .map_err(|e| MediaError::Transport(e.to_string()))?;
        let status = resp.status();
        let bytes = super::util::read_bytes_capped(
            resp,
            super::util::MAX_BINARY_BODY_BYTES,
            "tts_gemini",
        )
        .await?;

        if !status.is_success() {
            return Err(MediaError::Provider {
                status: status.as_u16(),
                message: preview(&bytes),
            });
        }

        let parsed: WireResponse = serde_json::from_slice(&bytes)
            .map_err(|e| MediaError::Parse(format!("{PROVIDER_NAME}: {e}")))?;

        if let Some(err) = parsed.error {
            return Err(MediaError::Provider {
                status: err.code as u16,
                message: format!("{}: {}", err.status, err.message),
            });
        }

        let inline = parsed
            .candidates
            .into_iter()
            .filter_map(|c| c.content)
            .flat_map(|c| c.parts)
            .find_map(|p| p.inline_data)
            .ok_or_else(|| {
                MediaError::Parse(format!(
                    "{PROVIDER_NAME}: no candidates[].content.parts[].inlineData in response"
                ))
            })?;

        let audio = decode_base64(&inline.data)
            .map_err(|e| MediaError::Parse(format!("{PROVIDER_NAME}: base64 decode: {e}")))?;
        let sample_rate = parse_sample_rate(&inline.mime_type);
        Ok(TtsResponse {
            audio,
            format: AudioFormat::Pcm16,
            sample_rate,
        })
    }
}

/// Extract `rate=<int>` from a Gemini `audio/L16;codec=pcm;rate=24000`
/// MIME hint. Returns None if absent or unparseable so callers can
/// fall back to a sane default downstream.
pub fn parse_sample_rate(mime: &str) -> Option<u32> {
    for part in mime.split(';') {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix("rate=") {
            if let Ok(n) = value.parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

/// Minimal padded standard base64 decoder (RFC 4648). Accepts only
/// `[A-Za-z0-9+/=]`; rejects whitespace / URL-safe alphabet (Gemini
/// always returns standard base64). Padding is required.
pub fn decode_base64(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(format!("base64 length {} not multiple of 4", bytes.len()));
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut i = 0;
    while i < bytes.len() {
        let q = [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
        let mut vals = [0u8; 4];
        let mut pad = 0;
        for j in 0..4 {
            vals[j] = match q[j] {
                b'A'..=b'Z' => q[j] - b'A',
                b'a'..=b'z' => q[j] - b'a' + 26,
                b'0'..=b'9' => q[j] - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => {
                    pad += 1;
                    0
                }
                other => {
                    return Err(format!("invalid base64 char: {:?}", other as char));
                }
            };
        }
        out.push((vals[0] << 2) | (vals[1] >> 4));
        if pad < 2 {
            out.push((vals[1] << 4) | (vals[2] >> 2));
        }
        if pad < 1 {
            out.push((vals[2] << 6) | vals[3]);
        }
        i += 4;
    }
    Ok(out)
}

fn preview(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    super::util::preview(&text, 512)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/media/tts_gemini.rs"
    ));
}
