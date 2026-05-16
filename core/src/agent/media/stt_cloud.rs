//! Generic cloud STT provider — covers OpenAI's
//! `POST /v1/audio/transcriptions` multipart shape and any
//! backend that mimics it (Groq, xAI, Mistral Voxtral, custom
//! OpenAI-compatible STT gateway).
//!
//! Wire format (multipart/form-data):
//!   - `file`               audio bytes + filename + MIME
//!   - `model`              e.g. `whisper-1`, `gpt-4o-transcribe`
//!   - `language`           ISO-639-1 (optional)
//!   - `response_format`    `json` (default) or caller-supplied
//!
//! Successful JSON shape (verbose_json upgrade hooks segments in
//! when present, else uses the flat top-level `text`):
//!   { "text": "...", "language": "en",
//!     "segments": [{ "start": s, "end": s, "text": "..." }] }

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;

use super::stt::{SttProvider, SttRequest, SttResponse, SttSegment};
use super::tts::AudioFormat;
use super::MediaError;

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_GROQ_BASE: &str = "https://api.groq.com/openai/v1";
const DEFAULT_XAI_BASE: &str = "https://api.x.ai/v1";
const DEFAULT_MISTRAL_BASE: &str = "https://api.mistral.ai/v1";

pub const PROVIDER_ALIASES: &[&str] = &["openai", "groq", "xai", "mistral", "custom"];

pub fn default_base_url_for(alias: &str) -> &'static str {
    match alias {
        "groq" => DEFAULT_GROQ_BASE,
        "xai" => DEFAULT_XAI_BASE,
        "mistral" => DEFAULT_MISTRAL_BASE,
        _ => DEFAULT_OPENAI_BASE,
    }
}

#[derive(Debug, Clone)]
pub struct CloudSttConfig {
    pub alias: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
}

impl CloudSttConfig {
    pub fn for_alias(alias: &str, model: impl Into<String>) -> Self {
        Self {
            alias: alias.to_string(),
            base_url: default_base_url_for(alias).to_string(),
            api_key: None,
            model: model.into(),
            extra_headers: HashMap::new(),
            request_timeout: Duration::from_secs(120),
        }
    }
}

pub struct CloudSttProvider {
    cfg: CloudSttConfig,
    client: reqwest::Client,
}

impl CloudSttProvider {
    pub fn new(cfg: CloudSttConfig) -> Self {
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
        format!("{base}/audio/transcriptions")
    }
}

/// File extension and MIME type to attach to the multipart `file`
/// field. The OpenAI / Groq endpoints rely on the file extension
/// to infer the codec, so the filename matters even when the MIME
/// is generic.
pub fn audio_format_filename(fmt: AudioFormat) -> (&'static str, &'static str) {
    match fmt {
        AudioFormat::Wav => ("audio.wav", "audio/wav"),
        AudioFormat::Mp3 => ("audio.mp3", "audio/mpeg"),
        AudioFormat::Ogg => ("audio.ogg", "audio/ogg"),
        AudioFormat::Pcm16 => ("audio.pcm", "application/octet-stream"),
        AudioFormat::Other => ("audio.bin", "application/octet-stream"),
    }
}

#[derive(Debug, Deserialize)]
struct WireSegment {
    start: f64,
    end: f64,
    text: String,
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    segments: Option<Vec<WireSegment>>,
}

pub fn parse_response(bytes: &[u8]) -> Result<SttResponse, MediaError> {
    let parsed: WireResponse =
        serde_json::from_slice(bytes).map_err(|e| MediaError::Parse(e.to_string()))?;
    let segments = parsed
        .segments
        .unwrap_or_default()
        .into_iter()
        .map(|s| SttSegment {
            start_ms: (s.start * 1000.0).max(0.0) as u32,
            end_ms: (s.end * 1000.0).max(0.0) as u32,
            text: s.text,
        })
        .collect();
    Ok(SttResponse {
        text: parsed.text,
        language: parsed.language,
        segments,
    })
}

#[async_trait]
impl SttProvider for CloudSttProvider {
    fn name(&self) -> &str {
        self.cfg.alias.as_str()
    }

    fn is_configured(&self) -> bool {
        self.cfg.api_key.is_some()
    }

    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, MediaError> {
        request.validate()?;
        if self.cfg.api_key.is_none() {
            return Err(MediaError::NotConfigured(self.cfg.alias.clone()));
        }

        let (filename, mime) = audio_format_filename(request.format);
        let response_format = request
            .response_hint
            .clone()
            .unwrap_or_else(|| "verbose_json".to_string());
        let part = Part::bytes(request.audio)
            .file_name(filename)
            .mime_str(mime)
            .map_err(|e| MediaError::InvalidRequest(e.to_string()))?;

        let mut form = Form::new()
            .part("file", part)
            .text("model", self.cfg.model.clone())
            .text("response_format", response_format);
        if let Some(lang) = request.language {
            form = form.text("language", lang);
        }

        let mut http = self.client.post(self.endpoint()).multipart(form);
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
            super::util::MAX_TEXT_BODY_BYTES,
            "stt_cloud",
        )
        .await?;

        if !status.is_success() {
            let preview = body_preview(&bytes);
            return Err(MediaError::Provider {
                status: status.as_u16(),
                message: preview,
            });
        }

        parse_response(&bytes)
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
        assert_eq!(default_base_url_for("groq"), DEFAULT_GROQ_BASE);
        assert_eq!(default_base_url_for("xai"), DEFAULT_XAI_BASE);
        assert_eq!(default_base_url_for("mistral"), DEFAULT_MISTRAL_BASE);
        assert_eq!(default_base_url_for("custom"), DEFAULT_OPENAI_BASE);
    }

    #[test]
    fn audio_format_filename_extensions() {
        assert_eq!(audio_format_filename(AudioFormat::Wav).0, "audio.wav");
        assert_eq!(audio_format_filename(AudioFormat::Mp3).0, "audio.mp3");
        assert_eq!(audio_format_filename(AudioFormat::Ogg).0, "audio.ogg");
        assert_eq!(audio_format_filename(AudioFormat::Pcm16).0, "audio.pcm");
        assert_eq!(audio_format_filename(AudioFormat::Other).0, "audio.bin");
    }

    #[test]
    fn for_alias_pulls_default_base_url() {
        let c = CloudSttConfig::for_alias("groq", "whisper-large-v3");
        assert_eq!(c.base_url, DEFAULT_GROQ_BASE);
        assert_eq!(c.model, "whisper-large-v3");
        assert!(c.api_key.is_none());
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let mut c = CloudSttConfig::for_alias("openai", "whisper-1");
        c.base_url = "https://api.openai.com/v1/".to_string();
        let p = CloudSttProvider::new(c);
        assert_eq!(
            p.endpoint(),
            "https://api.openai.com/v1/audio/transcriptions"
        );
    }

    #[test]
    fn name_reflects_alias() {
        let cfg = CloudSttConfig::for_alias("groq", "whisper-large-v3");
        let p = CloudSttProvider::new(cfg);
        assert_eq!(<CloudSttProvider as SttProvider>::name(&p), "groq");
    }

    #[test]
    fn is_configured_requires_api_key() {
        let mut cfg = CloudSttConfig::for_alias("openai", "whisper-1");
        let p1 = CloudSttProvider::new(cfg.clone());
        assert!(!<CloudSttProvider as SttProvider>::is_configured(&p1));
        cfg.api_key = Some("sk".to_string());
        let p2 = CloudSttProvider::new(cfg);
        assert!(<CloudSttProvider as SttProvider>::is_configured(&p2));
    }

    #[tokio::test]
    async fn transcribe_without_key_errors_not_configured() {
        let cfg = CloudSttConfig::for_alias("openai", "whisper-1");
        let p = CloudSttProvider::new(cfg);
        let req = SttRequest::new(vec![1, 2, 3], AudioFormat::Wav);
        let err = p.transcribe(req).await.unwrap_err();
        assert!(matches!(err, MediaError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn transcribe_validates_request() {
        let mut cfg = CloudSttConfig::for_alias("openai", "whisper-1");
        cfg.api_key = Some("sk".to_string());
        let p = CloudSttProvider::new(cfg);
        let req = SttRequest::new(vec![], AudioFormat::Wav);
        let err = p.transcribe(req).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[test]
    fn parse_response_minimal_text_only() {
        let r = parse_response(br#"{"text":"hello world"}"#).unwrap();
        assert_eq!(r.text, "hello world");
        assert!(r.language.is_none());
        assert!(r.segments.is_empty());
    }

    #[test]
    fn parse_response_verbose_with_segments() {
        let body = br#"{
            "text": "hello world",
            "language": "en",
            "segments": [
                {"start": 0.0, "end": 0.5, "text": "hello"},
                {"start": 0.5, "end": 1.0, "text": "world"}
            ]
        }"#;
        let r = parse_response(body).unwrap();
        assert_eq!(r.text, "hello world");
        assert_eq!(r.language.as_deref(), Some("en"));
        assert_eq!(r.segments.len(), 2);
        assert_eq!(r.segments[0].start_ms, 0);
        assert_eq!(r.segments[0].end_ms, 500);
        assert_eq!(r.segments[1].start_ms, 500);
        assert_eq!(r.segments[1].end_ms, 1000);
    }

    #[test]
    fn parse_response_negative_starts_clamp_to_zero() {
        let body = br#"{
            "text": "x",
            "segments": [{"start": -0.001, "end": 0.1, "text": "x"}]
        }"#;
        let r = parse_response(body).unwrap();
        assert_eq!(r.segments[0].start_ms, 0);
    }

    #[test]
    fn parse_response_rejects_garbage() {
        let err = parse_response(b"{not json").unwrap_err();
        assert!(matches!(err, MediaError::Parse(_)));
    }

    #[test]
    fn body_preview_truncates_long() {
        let big = vec![b'x'; 600];
        let s = body_preview(&big);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn provider_aliases_listed() {
        for a in ["openai", "groq", "xai", "mistral", "custom"] {
            assert!(PROVIDER_ALIASES.contains(&a));
        }
    }
}
