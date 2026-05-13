//! ElevenLabs TTS provider.
//!
//! Wire shape (from <https://elevenlabs.io/docs/api-reference/text-to-speech/convert>):
//!
//! ```text
//! POST /v1/text-to-speech/{voice_id}?output_format=mp3_44100_128
//! Headers:
//!   xi-api-key: <key>
//!   Content-Type: application/json
//! Body:
//!   {
//!     "text": "...",
//!     "model_id": "eleven_multilingual_v2",
//!     "voice_settings": { "stability": 0.5, "similarity_boost": 0.75 }
//!   }
//! ```
//!
//! Response: raw audio bytes in the requested container (mp3 by
//! default, wav also supported via `pcm_16000` + `wav` shape — we
//! map our [`AudioFormat`] enum to the `output_format` query value
//! per the ElevenLabs table).
//!
//! Configuration ([`ElevenLabsConfig`]):
//!   * `api_key`            sent as `xi-api-key`.
//!   * `default_voice_id`   used when the caller request omits one.
//!   * `model`              defaults to `eleven_multilingual_v2`.
//!   * `stability` / `similarity_boost`   voice settings.
//!   * `base_url`           override for self-hosted gateways.
//!   * `extra_headers`      pass-through.
//!   * `request_timeout`    per-call timeout (Duration::ZERO disables).

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use super::tts::{AudioFormat, TtsProvider, TtsRequest, TtsResponse};
use super::MediaError;

const DEFAULT_BASE: &str = "https://api.elevenlabs.io";
const DEFAULT_MODEL: &str = "eleven_multilingual_v2";
const PROVIDER_NAME: &str = "elevenlabs";

#[derive(Debug, Clone)]
pub struct ElevenLabsConfig {
    pub api_key: Option<String>,
    pub default_voice_id: Option<String>,
    pub model: String,
    pub stability: Option<f32>,
    pub similarity_boost: Option<f32>,
    pub base_url: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
}

impl Default for ElevenLabsConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            default_voice_id: None,
            model: DEFAULT_MODEL.to_string(),
            stability: None,
            similarity_boost: None,
            base_url: DEFAULT_BASE.to_string(),
            extra_headers: HashMap::new(),
            request_timeout: Duration::from_secs(60),
        }
    }
}

pub struct ElevenLabsProvider {
    cfg: ElevenLabsConfig,
    client: reqwest::Client,
}

impl ElevenLabsProvider {
    pub fn new(cfg: ElevenLabsConfig) -> Self {
        let mut builder =
            reqwest::Client::builder().user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")));
        if cfg.request_timeout > Duration::from_secs(0) {
            builder = builder.timeout(cfg.request_timeout);
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        Self { cfg, client }
    }

    fn endpoint(&self, voice_id: &str) -> String {
        let base = self.cfg.base_url.trim_end_matches('/');
        format!("{base}/v1/text-to-speech/{voice_id}")
    }
}

/// Map [`AudioFormat`] to the `output_format` query string the
/// ElevenLabs endpoint accepts. Defaults bias toward broadly
/// compatible 44.1 kHz mp3.
pub fn output_format_query(fmt: AudioFormat) -> &'static str {
    match fmt {
        AudioFormat::Mp3 => "mp3_44100_128",
        AudioFormat::Wav => "pcm_16000",
        AudioFormat::Ogg => "ogg_44100_64",
        AudioFormat::Pcm16 => "pcm_16000",
        AudioFormat::Other => "mp3_44100_128",
    }
}

/// Container container that the `output_format` query maps back to.
/// `pcm_16000` is raw little-endian PCM (no WAV header), so we
/// surface it as `Pcm16` regardless of caller request — keeps the
/// downstream contract honest.
fn response_container_for(out_fmt: &str) -> AudioFormat {
    if out_fmt.starts_with("mp3") {
        AudioFormat::Mp3
    } else if out_fmt.starts_with("ogg") {
        AudioFormat::Ogg
    } else if out_fmt.starts_with("pcm") {
        AudioFormat::Pcm16
    } else {
        AudioFormat::Other
    }
}

#[derive(Debug, Serialize)]
struct WireRequest<'a> {
    text: &'a str,
    model_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice_settings: Option<VoiceSettings>,
}

#[derive(Debug, Serialize, Clone, Copy)]
struct VoiceSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    stability: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    similarity_boost: Option<f32>,
}

impl VoiceSettings {
    fn from_cfg(cfg: &ElevenLabsConfig) -> Option<Self> {
        if cfg.stability.is_none() && cfg.similarity_boost.is_none() {
            None
        } else {
            Some(Self {
                stability: cfg.stability,
                similarity_boost: cfg.similarity_boost,
            })
        }
    }
}

#[async_trait]
impl TtsProvider for ElevenLabsProvider {
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
        let voice_id = request
            .voice
            .as_deref()
            .or(self.cfg.default_voice_id.as_deref())
            .ok_or_else(|| {
                MediaError::InvalidRequest(
                    "elevenlabs: voice_id required (set request.voice or default_voice_id)"
                        .to_string(),
                )
            })?;

        let requested = request.format.unwrap_or(AudioFormat::Mp3);
        let out_fmt = output_format_query(requested);

        let body = WireRequest {
            text: &request.text,
            model_id: &self.cfg.model,
            voice_settings: VoiceSettings::from_cfg(&self.cfg),
        };

        let url = self.endpoint(voice_id);
        let mut http = self
            .client
            .post(&url)
            .query(&[("output_format", out_fmt)])
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(key) = &self.cfg.api_key {
            http = http.header("xi-api-key", key.as_str());
        }
        for (k, v) in &self.cfg.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }

        let resp = http
            .send()
            .await
            .map_err(|e| MediaError::Transport(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| MediaError::Transport(e.to_string()))?;

        if !status.is_success() {
            let preview = preview(&bytes);
            return Err(MediaError::Provider {
                status: status.as_u16(),
                message: preview,
            });
        }

        let format = response_container_for(out_fmt);
        Ok(TtsResponse {
            audio: bytes.to_vec(),
            format,
            sample_rate: if matches!(format, AudioFormat::Pcm16) {
                Some(16_000)
            } else {
                None
            },
        })
    }
}

fn preview(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if text.len() > 512 {
        format!("{}…", &text[..512])
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_query_mapping() {
        assert_eq!(output_format_query(AudioFormat::Mp3), "mp3_44100_128");
        assert_eq!(output_format_query(AudioFormat::Wav), "pcm_16000");
        assert_eq!(output_format_query(AudioFormat::Ogg), "ogg_44100_64");
        assert_eq!(output_format_query(AudioFormat::Pcm16), "pcm_16000");
        assert_eq!(output_format_query(AudioFormat::Other), "mp3_44100_128");
    }

    #[test]
    fn response_container_for_known_formats() {
        assert_eq!(response_container_for("mp3_44100_128"), AudioFormat::Mp3);
        assert_eq!(response_container_for("ogg_44100_64"), AudioFormat::Ogg);
        assert_eq!(response_container_for("pcm_16000"), AudioFormat::Pcm16);
        assert_eq!(response_container_for("ulaw_8000"), AudioFormat::Other);
    }

    #[test]
    fn endpoint_concatenates_voice_id() {
        let cfg = ElevenLabsConfig::default();
        let p = ElevenLabsProvider::new(cfg);
        assert_eq!(
            p.endpoint("21m00Tcm4TlvDq8ikWAM"),
            "https://api.elevenlabs.io/v1/text-to-speech/21m00Tcm4TlvDq8ikWAM"
        );
    }

    #[test]
    fn endpoint_strips_trailing_slash_on_base() {
        let mut cfg = ElevenLabsConfig::default();
        cfg.base_url = "https://example.com/".to_string();
        let p = ElevenLabsProvider::new(cfg);
        assert_eq!(p.endpoint("v1"), "https://example.com/v1/text-to-speech/v1");
    }

    #[test]
    fn provider_name_is_stable() {
        let p = ElevenLabsProvider::new(ElevenLabsConfig::default());
        assert_eq!(<ElevenLabsProvider as TtsProvider>::name(&p), "elevenlabs");
    }

    #[test]
    fn is_configured_requires_api_key() {
        let mut cfg = ElevenLabsConfig::default();
        let p1 = ElevenLabsProvider::new(cfg.clone());
        assert!(!<ElevenLabsProvider as TtsProvider>::is_configured(&p1));
        cfg.api_key = Some("xi-test".to_string());
        let p2 = ElevenLabsProvider::new(cfg);
        assert!(<ElevenLabsProvider as TtsProvider>::is_configured(&p2));
    }

    #[tokio::test]
    async fn synthesize_without_key_errors_not_configured() {
        let cfg = ElevenLabsConfig::default();
        let p = ElevenLabsProvider::new(cfg);
        let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
        assert!(matches!(err, MediaError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn synthesize_validates_request() {
        let mut cfg = ElevenLabsConfig::default();
        cfg.api_key = Some("xi-test".to_string());
        cfg.default_voice_id = Some("v1".to_string());
        let p = ElevenLabsProvider::new(cfg);
        let err = p.synthesize(TtsRequest::new("")).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn synthesize_requires_voice_id() {
        let mut cfg = ElevenLabsConfig::default();
        cfg.api_key = Some("xi-test".to_string());
        let p = ElevenLabsProvider::new(cfg);
        let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
        match err {
            MediaError::InvalidRequest(msg) => assert!(msg.contains("voice_id")),
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn voice_settings_omitted_when_unset() {
        let cfg = ElevenLabsConfig::default();
        assert!(VoiceSettings::from_cfg(&cfg).is_none());
    }

    #[test]
    fn voice_settings_set_when_either_field_present() {
        let mut cfg = ElevenLabsConfig::default();
        cfg.stability = Some(0.5);
        let s = VoiceSettings::from_cfg(&cfg).unwrap();
        assert_eq!(s.stability, Some(0.5));
        assert_eq!(s.similarity_boost, None);
    }

    #[test]
    fn wire_request_serializes_required_fields() {
        let body = WireRequest {
            text: "hi",
            model_id: "eleven_multilingual_v2",
            voice_settings: None,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["text"], "hi");
        assert_eq!(json["model_id"], "eleven_multilingual_v2");
        assert!(json.get("voice_settings").is_none());
    }

    #[test]
    fn wire_request_with_voice_settings() {
        let body = WireRequest {
            text: "hi",
            model_id: "m",
            voice_settings: Some(VoiceSettings {
                stability: Some(0.5),
                similarity_boost: Some(0.75),
            }),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!((json["voice_settings"]["stability"].as_f64().unwrap() - 0.5).abs() < 1e-4);
        assert!((json["voice_settings"]["similarity_boost"].as_f64().unwrap() - 0.75).abs() < 1e-4);
    }

    #[test]
    fn preview_truncates_long_payloads() {
        let big = vec![b'x'; 600];
        let s = preview(&big);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn preview_keeps_short_payload() {
        assert_eq!(preview(b"err"), "err");
    }
}
