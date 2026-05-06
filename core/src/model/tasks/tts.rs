//! Text-to-speech task — text → audio bytes via OpenAI-compatible
//! `/audio/speech`.
//!
//! Phase 1.5: cloud only. Local Piper / KittenTTS via ort lands when
//! the user supplies ONNX files.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::TtsConfig;

// =====================================================================
// Public types
// =====================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsRequest {
    pub text: String,
    /// e.g. `alloy`, `echo`, `fable`, `onyx`, `nova`, `shimmer`.
    pub voice: Option<String>,
    /// `mp3` | `opus` | `aac` | `flac` | `wav` | `pcm`.
    pub format: Option<String>,
    /// Speed [0.25 .. 4.0]. Provider-clamped.
    pub speed: Option<f32>,
    /// Optional system instruction (gpt-4o-mini-tts supports this).
    pub instructions: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TtsResponse {
    pub audio: Vec<u8>,
    pub format: String,
    pub model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    #[error("not configured: set [tts] block in config.json")]
    NotConfigured,
    #[error("authentication failed: bad or missing API key")]
    Auth,
    #[error("rate limited (retry after {retry_after_ms}ms)")]
    RateLimited { retry_after_ms: u64 },
    #[error("provider returned error: {status} — {message}")]
    Provider { status: u16, message: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[async_trait]
pub trait TextToSpeech: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn is_configured(&self) -> bool;
    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError>;
}

// =====================================================================
// Factory
// =====================================================================

pub fn build_default() -> Result<Option<Box<dyn TextToSpeech>>, String> {
    build_from(&crate::config::get().tts)
}

pub fn build_from(cfg: &TtsConfig) -> Result<Option<Box<dyn TextToSpeech>>, String> {
    match cfg.provider.as_str() {
        "none" | "" => Ok(None),
        "openai" | "xai" | "deepseek" | "openrouter" | "ollama" => {
            Ok(Some(Box::new(OpenAICompatTts::from_config(cfg))))
        }
        "edge" | "edge-tts" => Ok(Some(Box::new(EdgeTtsTask::from_config(cfg)))),
        other => Err(format!("unknown tts provider: {other}")),
    }
}

// =====================================================================
// OpenAI-compatible TTS
// =====================================================================

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";

pub struct OpenAICompatTts {
    alias: String,
    base_url: String,
    api_key: Option<String>,
    model: String,
    extra_headers: HashMap<String, String>,
    default_voice: String,
    default_format: String,
    client: reqwest::Client,
}

impl OpenAICompatTts {
    pub fn from_config(cfg: &TtsConfig) -> Self {
        let alias = cfg.provider.clone();
        let base_url = cfg
            .base_url
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_string());
        let base_url = base_url.trim_end_matches('/').to_string();

        let api_key = crate::agent::llm::providers::openai_compat::resolve_api_key(
            cfg.api_key_credential.as_deref(),
            cfg.api_key_env.as_deref(),
        )
        .ok()
        .flatten();

        let timeout = if cfg.request_timeout == 0 {
            Duration::from_secs(0)
        } else {
            Duration::from_secs(cfg.request_timeout)
        };
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            alias,
            base_url,
            api_key,
            model: cfg.model.clone(),
            extra_headers: cfg.extra_headers.clone(),
            default_voice: cfg.default_voice.clone(),
            default_format: cfg.default_format.clone(),
            client,
        }
    }

    fn endpoint(&self) -> String {
        let (base, query) = match self.base_url.split_once('?') {
            Some((b, q)) => (b.trim_end_matches('/'), Some(q)),
            None => (self.base_url.as_str(), None),
        };
        match query {
            Some(q) => format!("{base}/audio/speech?{q}"),
            None => format!("{base}/audio/speech"),
        }
    }
}

#[async_trait]
impl TextToSpeech for OpenAICompatTts {
    fn name(&self) -> &str {
        &self.alias
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn is_configured(&self) -> bool {
        self.api_key.is_some()
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        if request.text.trim().is_empty() {
            return Err(TtsError::InvalidInput("text must not be empty".into()));
        }
        let voice = request.voice.unwrap_or_else(|| self.default_voice.clone());
        let format = request
            .format
            .unwrap_or_else(|| self.default_format.clone());

        // Azure deployment URLs encode the model in the path.
        let is_azure_deployment = self.base_url.contains("/deployments/");

        let mut body = serde_json::json!({
            "input": request.text,
            "voice": voice,
            "response_format": format,
        });
        if let Some(obj) = body.as_object_mut() {
            if !is_azure_deployment {
                obj.insert("model".into(), serde_json::Value::String(self.model.clone()));
            }
            if let Some(s) = request.speed {
                obj.insert("speed".into(), serde_json::json!(s));
            }
            if let Some(i) = request.instructions {
                obj.insert("instructions".into(), serde_json::Value::String(i));
            }
        }

        let mut http = self
            .client
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(key) = &self.api_key {
            http = http.bearer_auth(key);
        }
        for (k, v) in &self.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }
        let resp = http
            .send()
            .await
            .map_err(|e| TtsError::Transport(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| TtsError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(classify_http_error(status.as_u16(), &bytes));
        }
        Ok(TtsResponse {
            audio: bytes.to_vec(),
            format,
            model: self.model.clone(),
        })
    }
}

fn classify_http_error(status: u16, bytes: &[u8]) -> TtsError {
    let message = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).chars().take(400).collect());
    match status {
        401 | 403 => TtsError::Auth,
        429 => TtsError::RateLimited {
            retry_after_ms: 1000,
        },
        _ => TtsError::Provider { status, message },
    }
}

// =====================================================================
// Edge TTS adapter
//
// `agent::media::tts_edge::EdgeTtsProvider` is the canonical Edge
// implementation (used by the agent's media registry). The `cos model
// speak` CLI plumbs through this `TextToSpeech` trait, so we wrap the
// canonical provider in a thin adapter that translates the request /
// response / error types.
// =====================================================================

const EDGE_PROVIDER_NAME: &str = "edge-tts";

pub struct EdgeTtsTask {
    inner: crate::agent::media::tts_edge::EdgeTtsProvider,
    /// `cfg.model` is meaningless for Edge (the voice IS the model)
    /// but the trait surface still wants a value, so we report the
    /// configured `default_voice` here (or the canonical default).
    reported_model: String,
    default_format: String,
}

impl EdgeTtsTask {
    pub fn from_config(cfg: &TtsConfig) -> Self {
        let default_voice = if cfg.default_voice.is_empty() {
            None
        } else {
            Some(cfg.default_voice.clone())
        };
        let request_timeout = if cfg.request_timeout == 0 {
            Duration::from_secs(0)
        } else {
            Duration::from_secs(cfg.request_timeout)
        };
        let mut ec = crate::agent::media::tts_edge::EdgeTtsConfig {
            default_voice: default_voice.clone(),
            extra_headers: cfg.extra_headers.clone(),
            request_timeout,
            ..crate::agent::media::tts_edge::EdgeTtsConfig::default()
        };
        if let Some(b) = cfg.base_url.as_ref().filter(|s| !s.is_empty()) {
            ec.base_url = b.clone();
        }
        let reported_model = default_voice.unwrap_or_else(|| "en-US-AriaNeural".to_string());
        let default_format = if cfg.default_format.is_empty() {
            "mp3".to_string()
        } else {
            cfg.default_format.clone()
        };
        Self {
            inner: crate::agent::media::tts_edge::EdgeTtsProvider::new(ec),
            reported_model,
            default_format,
        }
    }
}

#[async_trait]
impl TextToSpeech for EdgeTtsTask {
    fn name(&self) -> &str {
        EDGE_PROVIDER_NAME
    }

    fn model(&self) -> &str {
        &self.reported_model
    }

    fn is_configured(&self) -> bool {
        // No API key required.
        true
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        if request.text.trim().is_empty() {
            return Err(TtsError::InvalidInput("text must not be empty".into()));
        }
        let format_str = request
            .format
            .clone()
            .unwrap_or_else(|| self.default_format.clone());
        let format = parse_audio_format(&format_str)?;
        let media_req = crate::agent::media::tts::TtsRequest {
            text: request.text,
            voice: request.voice,
            language: None,
            speed: request.speed,
            format: Some(format),
        };
        // `synthesize` lives on the canonical `TtsProvider` trait.
        use crate::agent::media::tts::TtsProvider as _;
        let resp = self
            .inner
            .synthesize(media_req)
            .await
            .map_err(media_error_to_tts_error)?;
        Ok(TtsResponse {
            audio: resp.audio,
            format: format_str,
            model: self.reported_model.clone(),
        })
    }
}

/// Convert a user-facing format string (`"mp3"`, `"wav"`, `"ogg"`,
/// `"pcm"`) into the typed [`AudioFormat`]. Any other value (including
/// OpenAI-only formats like `"opus"`, `"aac"`, `"flac"`) is rejected
/// with a useful error — Edge has a fixed format menu.
fn parse_audio_format(s: &str) -> Result<crate::agent::media::tts::AudioFormat, TtsError> {
    use crate::agent::media::tts::AudioFormat;
    match s.to_ascii_lowercase().as_str() {
        "mp3" => Ok(AudioFormat::Mp3),
        "wav" | "riff" | "pcm-wav" => Ok(AudioFormat::Wav),
        "ogg" | "opus-ogg" => Ok(AudioFormat::Ogg),
        "pcm" | "raw" | "pcm16" => Ok(AudioFormat::Pcm16),
        other => Err(TtsError::InvalidInput(format!(
            "edge tts does not support format '{other}' — use mp3 | wav | ogg | pcm"
        ))),
    }
}

/// Translate `MediaError` (returned by the canonical media provider)
/// to `TtsError` (the model-task trait's error type). Keeps the
/// classifications aligned with `OpenAICompatTts::synthesize`.
fn media_error_to_tts_error(e: crate::agent::media::MediaError) -> TtsError {
    use crate::agent::media::MediaError;
    match e {
        MediaError::NotConfigured(_) => TtsError::Auth,
        MediaError::InvalidRequest(msg) => TtsError::InvalidInput(msg),
        MediaError::Transport(msg) => TtsError::Transport(msg),
        MediaError::Provider { status, message } => TtsError::Provider { status, message },
        MediaError::Parse(msg) => TtsError::Provider {
            status: 0,
            message: msg,
        },
        MediaError::Internal(msg) => TtsError::Transport(msg),
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TtsConfig {
        let mut c = TtsConfig::default();
        c.provider = "openai".into();
        c.model = "tts-1".into();
        c
    }

    #[test]
    fn build_returns_none_when_disabled() {
        let mut c = TtsConfig::default();
        c.provider = "none".into();
        assert!(build_from(&c).unwrap().is_none());
    }

    #[test]
    fn build_returns_err_for_unknown_provider() {
        let mut c = TtsConfig::default();
        c.provider = "unknown".into();
        assert!(build_from(&c).is_err());
    }

    #[test]
    fn build_edge_alias_returns_edge_task() {
        let mut c = TtsConfig::default();
        c.provider = "edge".into();
        let t = build_from(&c).unwrap().expect("edge task");
        assert_eq!(t.name(), "edge-tts");
        // Edge has no API key requirement.
        assert!(t.is_configured());
    }

    #[test]
    fn build_edge_tts_alias_returns_edge_task() {
        let mut c = TtsConfig::default();
        c.provider = "edge-tts".into();
        let t = build_from(&c).unwrap().expect("edge-tts task");
        assert_eq!(t.name(), "edge-tts");
    }

    #[test]
    fn edge_task_reports_default_voice_as_model() {
        let mut c = TtsConfig::default();
        c.provider = "edge".into();
        c.default_voice = "en-GB-RyanNeural".into();
        let t = EdgeTtsTask::from_config(&c);
        assert_eq!(t.model(), "en-GB-RyanNeural");
    }

    #[test]
    fn edge_task_falls_back_to_aria_when_voice_empty() {
        let mut c = TtsConfig::default();
        c.provider = "edge".into();
        c.default_voice = "".into();
        let t = EdgeTtsTask::from_config(&c);
        assert_eq!(t.model(), "en-US-AriaNeural");
    }

    #[test]
    fn parse_audio_format_accepts_known() {
        use crate::agent::media::tts::AudioFormat;
        assert!(matches!(parse_audio_format("mp3"), Ok(AudioFormat::Mp3)));
        assert!(matches!(parse_audio_format("MP3"), Ok(AudioFormat::Mp3)));
        assert!(matches!(parse_audio_format("wav"), Ok(AudioFormat::Wav)));
        assert!(matches!(parse_audio_format("ogg"), Ok(AudioFormat::Ogg)));
        assert!(matches!(parse_audio_format("pcm"), Ok(AudioFormat::Pcm16)));
    }

    #[test]
    fn parse_audio_format_rejects_unsupported() {
        let err = parse_audio_format("opus").unwrap_err();
        match err {
            TtsError::InvalidInput(msg) => assert!(msg.contains("opus")),
            other => panic!("unexpected: {other:?}"),
        }
        // OpenAI-compat formats Edge doesn't speak natively are
        // rejected, not silently coerced.
        assert!(parse_audio_format("aac").is_err());
        assert!(parse_audio_format("flac").is_err());
    }

    #[tokio::test]
    async fn edge_task_synthesize_rejects_empty_text() {
        let mut c = TtsConfig::default();
        c.provider = "edge".into();
        let t = EdgeTtsTask::from_config(&c);
        let err = t
            .synthesize(TtsRequest {
                text: "".into(),
                voice: None,
                format: None,
                speed: None,
                instructions: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, TtsError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn edge_task_synthesize_rejects_unsupported_format() {
        let mut c = TtsConfig::default();
        c.provider = "edge".into();
        let t = EdgeTtsTask::from_config(&c);
        let err = t
            .synthesize(TtsRequest {
                text: "hi".into(),
                voice: None,
                format: Some("flac".into()),
                speed: None,
                instructions: None,
            })
            .await
            .unwrap_err();
        match err {
            TtsError::InvalidInput(msg) => assert!(msg.contains("flac")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn endpoint_default_path() {
        let mut c = cfg();
        c.base_url = Some("https://api.openai.com/v1".into());
        let t = OpenAICompatTts::from_config(&c);
        assert_eq!(t.endpoint(), "https://api.openai.com/v1/audio/speech");
    }

    #[test]
    fn endpoint_handles_azure_query_string() {
        let mut c = cfg();
        c.base_url = Some(
            "https://account.openai.azure.com/openai/deployments/tts-1?api-version=2024-02-01"
                .into(),
        );
        let t = OpenAICompatTts::from_config(&c);
        assert_eq!(
            t.endpoint(),
            "https://account.openai.azure.com/openai/deployments/tts-1/audio/speech?api-version=2024-02-01"
        );
    }

    #[test]
    fn classify_http_error_maps_codes() {
        assert!(matches!(classify_http_error(401, b"{}"), TtsError::Auth));
        assert!(matches!(
            classify_http_error(429, b"{}"),
            TtsError::RateLimited { .. }
        ));
        let p = classify_http_error(500, br#"{"error":{"message":"oops"}}"#);
        if let TtsError::Provider { status, message } = p {
            assert_eq!(status, 500);
            assert!(message.contains("oops"));
        }
    }

    #[tokio::test]
    async fn synthesize_rejects_empty_text() {
        let t = OpenAICompatTts::from_config(&cfg());
        let err = t
            .synthesize(TtsRequest {
                text: "  ".into(),
                voice: None,
                format: None,
                speed: None,
                instructions: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, TtsError::InvalidInput(_)));
    }

    async fn spawn_one_shot_mock_binary(
        response_body: Vec<u8>,
        status_line: &'static str,
        content_type: &'static str,
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
                    let body_start =
                        total.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
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
            let response = format!(
                "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.write_all(&response_body).await;
            let _ = sock.shutdown().await;
            total
        });
        (url, handle)
    }

    #[tokio::test]
    async fn end_to_end_synthesize_round_trip() {
        std::env::set_var("COS_TEST_TTS_KEY", "sk-tts");
        // Pretend the response is an MP3. (4 bytes of fake audio.)
        let fake_audio = vec![0xff, 0xfb, 0x90, 0x44];
        let (base_url, handle) = spawn_one_shot_mock_binary(
            fake_audio.clone(),
            "HTTP/1.1 200 OK",
            "audio/mpeg",
        )
        .await;
        let mut c = cfg();
        c.base_url = Some(base_url);
        c.api_key_env = Some("COS_TEST_TTS_KEY".into());
        let t = OpenAICompatTts::from_config(&c);
        let resp = t
            .synthesize(TtsRequest {
                text: "Hello there.".into(),
                voice: Some("alloy".into()),
                format: Some("mp3".into()),
                speed: Some(1.0),
                instructions: None,
            })
            .await
            .expect("synthesize");
        assert_eq!(resp.audio, fake_audio);
        assert_eq!(resp.format, "mp3");

        let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
        assert!(req.contains("post /v1/audio/speech"));
        assert!(req.contains("authorization: bearer sk-tts"));
        assert!(req.contains("\"input\":\"hello there.\""));
        assert!(req.contains("\"voice\":\"alloy\""));
        assert!(req.contains("\"response_format\":\"mp3\""));
        assert!(req.contains("\"model\":\"tts-1\""));

        std::env::remove_var("COS_TEST_TTS_KEY");
    }

    #[tokio::test]
    async fn azure_deployment_omits_model_field() {
        std::env::set_var("COS_TEST_TTS_KEY_2", "sk-tts2");
        let (base_url, handle) =
            spawn_one_shot_mock_binary(vec![1, 2, 3], "HTTP/1.1 200 OK", "audio/mpeg").await;
        let azure = format!("{base_url}/deployments/tts");
        let mut c = cfg();
        c.base_url = Some(azure);
        c.api_key_env = Some("COS_TEST_TTS_KEY_2".into());
        let t = OpenAICompatTts::from_config(&c);
        let _ = t
            .synthesize(TtsRequest {
                text: "x".into(),
                voice: None,
                format: None,
                speed: None,
                instructions: None,
            })
            .await
            .expect("synthesize");
        let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
        assert!(
            !req.contains("\"model\""),
            "Azure deployment shape must not send model field"
        );
        std::env::remove_var("COS_TEST_TTS_KEY_2");
    }
}
