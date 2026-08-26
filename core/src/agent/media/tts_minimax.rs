//! MiniMax TTS provider (Speech-02 family — `t2a_v2`).
//!
//! Endpoint: `POST {base}/v1/t2a_v2?GroupId=<group_id>`
//! Headers:  `Authorization: Bearer <api_key>`, `Content-Type: application/json`
//! Body:
//! ```json
//! {
//!   "model": "speech-02-hd",
//!   "text": "...",
//!   "stream": false,
//!   "voice_setting": {
//!     "voice_id": "...",
//!     "speed": 1.0,
//!     "vol": 1.0,
//!     "pitch": 0
//!   },
//!   "audio_setting": {
//!     "sample_rate": 32000,
//!     "format": "mp3",
//!     "channel": 1
//!   }
//! }
//! ```
//!
//! Response wrap:
//! ```json
//! {
//!   "data": { "audio": "<hex-encoded bytes>", "status": 2 },
//!   "trace_id": "...",
//!   "extra_info": { "audio_format": "mp3", "audio_sample_rate": 32000 },
//!   "base_resp": { "status_code": 0, "status_msg": "success" }
//! }
//! ```
//!
//! `data.audio` is hex-encoded (NOT base64) per the public docs.
//! `base_resp.status_code != 0` is a provider-level error even when
//! the HTTP status is 200 — this provider surfaces those as
//! [`MediaError::Provider`] with `status: 200` and the upstream
//! `status_msg`.
//!
//! Configuration ([`MiniMaxConfig`]):
//!   * `api_key`            sent as bearer.
//!   * `group_id`           appended as `?GroupId=...`.
//!   * `model`              defaults to `speech-02-hd`.
//!   * `default_voice_id`   used when the request omits voice.
//!   * `base_url`           override for self-hosted / regional endpoints.
//!   * `extra_headers`      pass-through.
//!   * `request_timeout`    per-call timeout (Duration::ZERO disables).

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::tts::{AudioFormat, TtsProvider, TtsRequest, TtsResponse};
use super::MediaError;

const DEFAULT_BASE: &str = "https://api.minimax.chat";
const DEFAULT_MODEL: &str = "speech-02-hd";
const PROVIDER_NAME: &str = "minimax";

#[derive(Debug, Clone)]
pub struct MiniMaxConfig {
    pub api_key: Option<String>,
    pub group_id: Option<String>,
    pub model: String,
    pub default_voice_id: Option<String>,
    pub base_url: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
}

impl Default for MiniMaxConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            group_id: None,
            model: DEFAULT_MODEL.to_string(),
            default_voice_id: None,
            base_url: DEFAULT_BASE.to_string(),
            extra_headers: HashMap::new(),
            request_timeout: Duration::from_secs(60),
        }
    }
}

pub struct MiniMaxTts {
    cfg: MiniMaxConfig,
}

impl MiniMaxTts {
    pub fn new(cfg: MiniMaxConfig) -> Self {
        // Per-request client via `util::build_safe_client` so the
        // endpoint host is DNS-pinned to a vetted public IP. See
        // `media/util.rs`.
        Self { cfg }
    }

    fn endpoint(&self) -> String {
        let base = self.cfg.base_url.trim_end_matches('/');
        format!("{base}/v1/t2a_v2")
    }
}

/// Map our AudioFormat to MiniMax `audio_setting.format`. The wire
/// values are lower-case container names; `Other` falls back to mp3.
pub fn audio_format_wire(fmt: AudioFormat) -> &'static str {
    match fmt {
        AudioFormat::Mp3 => "mp3",
        AudioFormat::Wav => "wav",
        AudioFormat::Ogg => "ogg",
        AudioFormat::Pcm16 => "pcm",
        AudioFormat::Other => "mp3",
    }
}

/// Default sample rate per format (mp3 32 kHz is MiniMax's default;
/// wav goes higher). Caller can override via extra_headers if a
/// regional endpoint demands it.
fn default_sample_rate(fmt: AudioFormat) -> u32 {
    match fmt {
        AudioFormat::Wav => 44_100,
        _ => 32_000,
    }
}

#[derive(Debug, Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    text: &'a str,
    stream: bool,
    voice_setting: VoiceSetting<'a>,
    audio_setting: AudioSetting<'a>,
}

#[derive(Debug, Serialize)]
struct VoiceSetting<'a> {
    voice_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f32>,
}

#[derive(Debug, Serialize)]
struct AudioSetting<'a> {
    sample_rate: u32,
    format: &'a str,
    channel: u32,
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    #[serde(default)]
    data: Option<DataField>,
    #[serde(default)]
    base_resp: Option<BaseResp>,
}

#[derive(Debug, Deserialize)]
struct DataField {
    #[serde(default)]
    audio: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BaseResp {
    #[serde(default)]
    status_code: i64,
    #[serde(default)]
    status_msg: String,
}

#[async_trait]
impl TtsProvider for MiniMaxTts {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn is_configured(&self) -> bool {
        self.cfg.api_key.is_some() && self.cfg.group_id.is_some()
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, MediaError> {
        request.validate()?;
        if self.cfg.api_key.is_none() {
            return Err(MediaError::NotConfigured(format!(
                "{PROVIDER_NAME}: api_key missing"
            )));
        }
        let group = self.cfg.group_id.as_deref().ok_or_else(|| {
            MediaError::NotConfigured(format!("{PROVIDER_NAME}: group_id missing"))
        })?;
        let voice_id = request
            .voice
            .as_deref()
            .or(self.cfg.default_voice_id.as_deref())
            .ok_or_else(|| {
                MediaError::InvalidRequest(format!(
                    "{PROVIDER_NAME}: voice_id required (set request.voice or default_voice_id)"
                ))
            })?;

        let format = request.format.unwrap_or(AudioFormat::Mp3);

        let body = WireRequest {
            model: &self.cfg.model,
            text: &request.text,
            stream: false,
            voice_setting: VoiceSetting {
                voice_id,
                speed: request.speed,
            },
            audio_setting: AudioSetting {
                sample_rate: default_sample_rate(format),
                format: audio_format_wire(format),
                channel: 1,
            },
        };

        let mut http = {
            let endpoint = self.endpoint();
            let url = reqwest::Url::parse(&endpoint)
                .map_err(|e| MediaError::InvalidRequest(format!("invalid endpoint url: {e}")))?;
            let client = super::util::build_safe_client(&url, self.cfg.request_timeout).await?;
            client
                .post(url)
                .query(&[("GroupId", group)])
                .header("Content-Type", "application/json")
                .json(&body)
        };
        if let Some(key) = &self.cfg.api_key {
            http = http.bearer_auth(key);
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
            "tts_minimax",
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

        if let Some(br) = &parsed.base_resp {
            if br.status_code != 0 {
                return Err(MediaError::Provider {
                    status: 200,
                    message: format!("{}: {}", br.status_code, br.status_msg),
                });
            }
        }

        let hex_audio = parsed
            .data
            .and_then(|d| d.audio)
            .ok_or_else(|| MediaError::Parse(format!("{PROVIDER_NAME}: data.audio missing")))?;
        let audio = decode_hex(&hex_audio)
            .map_err(|e| MediaError::Parse(format!("{PROVIDER_NAME}: hex decode: {e}")))?;

        Ok(TtsResponse {
            audio,
            format,
            sample_rate: Some(default_sample_rate(format)),
        })
    }
}

/// Decode a hex string (lower or upper case, no whitespace) into
/// raw bytes. Returns a clear error for odd length or bad nibbles
/// — MiniMax's payload can be ~1 MB so use a tight inner loop and
/// avoid an alloc for the iterator state.
pub fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err(format!("hex length must be even, got {}", s.len()));
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = nibble(bytes[i])?;
        let lo = nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn nibble(c: u8) -> Result<u8, String> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(10 + (c - b'a')),
        b'A'..=b'F' => Ok(10 + (c - b'A')),
        other => Err(format!("invalid hex char: {:?}", other as char)),
    }
}

fn preview(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    super::util::preview(&text, 512)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/media/tts_minimax.rs"
    ));
}
