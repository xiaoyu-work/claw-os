//! Generic cloud TTS provider — covers OpenAI's
//! `POST /v1/audio/speech` shape and any backend that mimics it
//! (xAI Grok, custom OpenAI-compatible TTS gateways, self-hosted
//! TTS proxies fronting a `/audio/speech` route).
//!
//! Backends with a different request shape (ElevenLabs path-based
//! voice IDs, Gemini multimodal, Edge TTS websocket, MiniMax
//! voice_setting object) get their own module — this file keeps
//! the wire format intentionally narrow so it can stay
//! provider-shape agnostic.
//!
//! Configuration (see [`CloudTtsConfig`]):
//!   * `alias`              "openai" / "xai" / "custom" — drives default base URL.
//!   * `base_url`           override the default; trailing `/` stripped.
//!   * `api_key`            sent as `Authorization: Bearer ...`.
//!   * `model`              e.g. `tts-1`, `tts-1-hd`.
//!   * `default_voice`      used when the request omits `voice`.
//!   * `extra_headers`      free-form (gateway routing, observability tags).
//!   * `request_timeout`    per-call timeout (Duration::ZERO disables).
//!
//! Wire shape: JSON `{"model": ..., "input": ..., "voice": ...,
//! "response_format": "mp3"|"wav"|...}` and the response is the
//! raw audio bytes (no JSON wrapper). HTTP errors are bucketed
//! through [`MediaError`].

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use super::tts::{AudioFormat, TtsProvider, TtsRequest, TtsResponse};
use super::MediaError;

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_XAI_BASE: &str = "https://api.x.ai/v1";

/// Aliases this provider answers to. `custom` falls back to OpenAI's
/// URL — caller is expected to override `base_url`.
pub const PROVIDER_ALIASES: &[&str] = &["openai", "xai", "custom"];

pub fn default_base_url_for(alias: &str) -> &'static str {
    match alias {
        "xai" => DEFAULT_XAI_BASE,
        _ => DEFAULT_OPENAI_BASE,
    }
}

#[derive(Debug, Clone)]
pub struct CloudTtsConfig {
    pub alias: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub default_voice: Option<String>,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
}

impl CloudTtsConfig {
    pub fn for_alias(alias: &str, model: impl Into<String>) -> Self {
        Self {
            alias: alias.to_string(),
            base_url: default_base_url_for(alias).to_string(),
            api_key: None,
            model: model.into(),
            default_voice: None,
            extra_headers: HashMap::new(),
            request_timeout: Duration::from_secs(60),
        }
    }
}

pub struct CloudTtsProvider {
    cfg: CloudTtsConfig,
    client: reqwest::Client,
}

impl CloudTtsProvider {
    pub fn new(cfg: CloudTtsConfig) -> Self {
        let mut builder =
            reqwest::Client::builder().user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")));
        if cfg.request_timeout > Duration::from_secs(0) {
            builder = builder.timeout(cfg.request_timeout);
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        Self { cfg, client }
    }

    fn endpoint(&self) -> String {
        let base = self.cfg.base_url.trim_end_matches('/');
        format!("{base}/audio/speech")
    }
}

#[derive(Debug, Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    input: &'a str,
    voice: &'a str,
    response_format: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f32>,
}

/// Map our [`AudioFormat`] enum to the wire string the OpenAI-style
/// endpoint expects. `Other` falls back to mp3 so the request is
/// always well-formed.
pub fn audio_format_wire(fmt: AudioFormat) -> &'static str {
    match fmt {
        AudioFormat::Wav => "wav",
        AudioFormat::Mp3 => "mp3",
        AudioFormat::Ogg => "opus",
        AudioFormat::Pcm16 => "pcm",
        AudioFormat::Other => "mp3",
    }
}

#[async_trait]
impl TtsProvider for CloudTtsProvider {
    fn name(&self) -> &str {
        self.cfg.alias.as_str()
    }

    fn is_configured(&self) -> bool {
        self.cfg.api_key.is_some()
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, MediaError> {
        request.validate()?;
        if self.cfg.api_key.is_none() {
            return Err(MediaError::NotConfigured(self.cfg.alias.clone()));
        }

        let format = request.format.unwrap_or(AudioFormat::Mp3);
        let voice = request
            .voice
            .as_deref()
            .or(self.cfg.default_voice.as_deref())
            .unwrap_or("alloy");

        let body = WireRequest {
            model: &self.cfg.model,
            input: &request.text,
            voice,
            response_format: audio_format_wire(format),
            speed: request.speed,
        };

        let mut http = self
            .client
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .json(&body);
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
            "tts_cloud",
        )
        .await?;

        if !status.is_success() {
            let preview = body_preview(&bytes);
            return Err(MediaError::Provider {
                status: status.as_u16(),
                message: preview,
            });
        }

        Ok(TtsResponse {
            audio: bytes.to_vec(),
            format,
            sample_rate: None,
        })
    }
}

fn body_preview(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    super::util::preview(&text, 512)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_url_for_known_aliases() {
        assert_eq!(default_base_url_for("openai"), DEFAULT_OPENAI_BASE);
        assert_eq!(default_base_url_for("xai"), DEFAULT_XAI_BASE);
        assert_eq!(default_base_url_for("custom"), DEFAULT_OPENAI_BASE);
        assert_eq!(default_base_url_for("unknown"), DEFAULT_OPENAI_BASE);
    }

    #[test]
    fn audio_format_wire_mapping() {
        assert_eq!(audio_format_wire(AudioFormat::Mp3), "mp3");
        assert_eq!(audio_format_wire(AudioFormat::Wav), "wav");
        assert_eq!(audio_format_wire(AudioFormat::Ogg), "opus");
        assert_eq!(audio_format_wire(AudioFormat::Pcm16), "pcm");
        assert_eq!(audio_format_wire(AudioFormat::Other), "mp3");
    }

    #[test]
    fn for_alias_pulls_default_base_url() {
        let c = CloudTtsConfig::for_alias("xai", "tts-1");
        assert_eq!(c.base_url, DEFAULT_XAI_BASE);
        assert_eq!(c.model, "tts-1");
        assert!(c.api_key.is_none());
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let mut c = CloudTtsConfig::for_alias("openai", "tts-1");
        c.base_url = "https://example.com/v1/".to_string();
        let p = CloudTtsProvider::new(c);
        assert_eq!(p.endpoint(), "https://example.com/v1/audio/speech");
    }

    #[test]
    fn provider_aliases_listed() {
        assert!(PROVIDER_ALIASES.contains(&"openai"));
        assert!(PROVIDER_ALIASES.contains(&"xai"));
        assert!(PROVIDER_ALIASES.contains(&"custom"));
    }

    #[test]
    fn name_reflects_alias() {
        let cfg = CloudTtsConfig::for_alias("xai", "tts-1");
        let p = CloudTtsProvider::new(cfg);
        assert_eq!(<CloudTtsProvider as TtsProvider>::name(&p), "xai");
    }

    #[test]
    fn is_configured_requires_api_key() {
        let mut cfg = CloudTtsConfig::for_alias("openai", "tts-1");
        let p1 = CloudTtsProvider::new(cfg.clone());
        assert!(!<CloudTtsProvider as TtsProvider>::is_configured(&p1));
        cfg.api_key = Some("sk-test".to_string());
        let p2 = CloudTtsProvider::new(cfg);
        assert!(<CloudTtsProvider as TtsProvider>::is_configured(&p2));
    }

    #[tokio::test]
    async fn synthesize_without_key_errors_not_configured() {
        let cfg = CloudTtsConfig::for_alias("openai", "tts-1");
        let p = CloudTtsProvider::new(cfg);
        let err = p.synthesize(TtsRequest::new("hello")).await.unwrap_err();
        assert!(matches!(err, MediaError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn synthesize_validates_request() {
        let mut cfg = CloudTtsConfig::for_alias("openai", "tts-1");
        cfg.api_key = Some("sk-test".to_string());
        let p = CloudTtsProvider::new(cfg);
        let err = p.synthesize(TtsRequest::new("")).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[test]
    fn body_preview_truncates_long_payloads() {
        let big = vec![b'x'; 600];
        let s = body_preview(&big);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= 513);
    }

    #[test]
    fn body_preview_keeps_short_payload() {
        assert_eq!(body_preview(b"oops"), "oops");
    }

    #[test]
    fn wire_request_serialises_required_fields() {
        let body = WireRequest {
            model: "tts-1",
            input: "hello",
            voice: "alloy",
            response_format: "mp3",
            speed: None,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["model"], "tts-1");
        assert_eq!(json["input"], "hello");
        assert_eq!(json["voice"], "alloy");
        assert_eq!(json["response_format"], "mp3");
        assert!(json.get("speed").is_none());
    }

    #[test]
    fn wire_request_includes_speed_when_set() {
        let body = WireRequest {
            model: "tts-1",
            input: "hi",
            voice: "alloy",
            response_format: "mp3",
            speed: Some(1.25),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["speed"], 1.25);
    }
}
