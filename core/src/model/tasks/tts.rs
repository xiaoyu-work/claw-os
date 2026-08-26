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
                obj.insert(
                    "model".into(),
                    serde_json::Value::String(self.model.clone()),
                );
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/model/tasks/tts.rs"
    ));
}
