//! Speech-to-text task — audio → text via OpenAI-compatible
//! `/audio/transcriptions` (preserves source language) and
//! `/audio/translations` (translates to English).
//!
//! Phase 1.5: cloud only. Local Whisper via ort lands when ONNX files
//! are supplied.
//!
//! Live-verifiable against an Azure OpenAI Whisper deployment at
//! `https://<account>.openai.azure.com/openai/deployments/<name>/audio/transcriptions?api-version=...`.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::SttConfig;

// =====================================================================
// Public types
// =====================================================================

/// Whether to keep the source language (`Transcribe`) or render to
/// English (`Translate`). Maps to `/audio/transcriptions` and
/// `/audio/translations` respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SttMode {
    Transcribe,
    Translate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttRequest {
    /// Raw audio bytes — already-encoded file (mp3/wav/m4a/flac/...).
    pub audio: Vec<u8>,
    /// Hint at the upload's filename so the multipart part has a useful
    /// extension. Provider sniffs the format from this.
    pub filename: String,
    /// Optional source language hint (BCP-47, e.g. "en", "zh"). Only
    /// used for `Transcribe` mode.
    pub language: Option<String>,
    /// Optional prompt (style/spelling biasing).
    pub prompt: Option<String>,
    /// Response shape: `"json"`, `"text"`, `"verbose_json"`, `"srt"`, `"vtt"`.
    pub response_format: Option<String>,
    /// Sampling temperature [0.0–1.0].
    pub temperature: Option<f32>,
    pub mode: SttMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttResponse {
    pub text: String,
    pub model: String,
    /// Detected source language (if provider returned it).
    pub language: Option<String>,
    /// Raw provider response — preserved when caller asks for verbose_json
    /// or non-JSON formats so segment/word timestamps survive.
    pub raw: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("not configured: set [stt] block in config.json")]
    NotConfigured,
    #[error("authentication failed: bad or missing API key")]
    Auth,
    #[error("rate limited (retry after {retry_after_ms}ms)")]
    RateLimited { retry_after_ms: u64 },
    #[error("provider returned error: {status} — {message}")]
    Provider { status: u16, message: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[async_trait]
pub trait SpeechToText: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn is_configured(&self) -> bool;
    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, SttError>;
}

// =====================================================================
// Factory
// =====================================================================

pub fn build_default() -> Result<Option<Box<dyn SpeechToText>>, String> {
    build_from(&crate::config::get().stt)
}

pub fn build_from(cfg: &SttConfig) -> Result<Option<Box<dyn SpeechToText>>, String> {
    match cfg.provider.as_str() {
        "none" | "" => Ok(None),
        "openai" | "xai" | "deepseek" | "openrouter" | "ollama" => {
            Ok(Some(Box::new(OpenAICompatStt::from_config(cfg))))
        }
        other => Err(format!("unknown stt provider: {other}")),
    }
}

// =====================================================================
// OpenAI-compatible cloud STT
// =====================================================================

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";

pub struct OpenAICompatStt {
    alias: String,
    base_url: String,
    api_key: Option<String>,
    model: String,
    extra_headers: HashMap<String, String>,
    default_response_format: String,
    client: reqwest::Client,
}

impl OpenAICompatStt {
    pub fn from_config(cfg: &SttConfig) -> Self {
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
            default_response_format: cfg.default_response_format.clone(),
            client,
        }
    }

    fn endpoint(&self, mode: SttMode) -> String {
        let path = match mode {
            SttMode::Transcribe => "audio/transcriptions",
            SttMode::Translate => "audio/translations",
        };
        let (base, query) = match self.base_url.split_once('?') {
            Some((b, q)) => (b.trim_end_matches('/'), Some(q)),
            None => (self.base_url.as_str(), None),
        };
        match query {
            Some(q) => format!("{base}/{path}?{q}"),
            None => format!("{base}/{path}"),
        }
    }
}

#[async_trait]
impl SpeechToText for OpenAICompatStt {
    fn name(&self) -> &str {
        &self.alias
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn is_configured(&self) -> bool {
        self.api_key.is_some()
    }

    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, SttError> {
        if request.audio.is_empty() {
            return Err(SttError::InvalidInput(
                "audio bytes must not be empty".into(),
            ));
        }
        if request.filename.trim().is_empty() {
            return Err(SttError::InvalidInput("filename must not be empty".into()));
        }
        let response_format = request
            .response_format
            .clone()
            .unwrap_or_else(|| self.default_response_format.clone());

        // Azure deployment URLs encode the model in the path — sending
        // a `model` field is harmless for stock OpenAI but redundant.
        let is_azure_deployment = self.base_url.contains("/deployments/");

        let mime = guess_mime(&request.filename);
        let part = reqwest::multipart::Part::bytes(request.audio.clone())
            .file_name(request.filename.clone())
            .mime_str(&mime)
            .map_err(|e| SttError::Transport(e.to_string()))?;
        let mut form = reqwest::multipart::Form::new().part("file", part);
        if !is_azure_deployment {
            form = form.text("model", self.model.clone());
        }
        form = form.text("response_format", response_format.clone());
        if let Some(t) = request.temperature {
            form = form.text("temperature", t.to_string());
        }
        if let Some(p) = request.prompt {
            form = form.text("prompt", p);
        }
        if request.mode == SttMode::Transcribe {
            if let Some(lang) = request.language {
                form = form.text("language", lang);
            }
        }

        let mut http = self
            .client
            .post(self.endpoint(request.mode))
            .multipart(form);
        if let Some(key) = &self.api_key {
            http = http.bearer_auth(key);
        }
        for (k, v) in &self.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }
        let resp = http
            .send()
            .await
            .map_err(|e| SttError::Transport(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| SttError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(classify_http_error(status.as_u16(), &bytes));
        }

        // Response shape varies. `text`/`srt`/`vtt` return plain text;
        // `json`/`verbose_json` return JSON.
        let body_str = String::from_utf8_lossy(&bytes).into_owned();
        match response_format.as_str() {
            "text" | "srt" | "vtt" => Ok(SttResponse {
                text: body_str.clone(),
                model: self.model.clone(),
                language: None,
                raw: serde_json::Value::String(body_str),
            }),
            _ => {
                let raw: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|e| SttError::Parse(e.to_string()))?;
                let text = raw
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let language = raw
                    .get("language")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Ok(SttResponse {
                    text,
                    model: self.model.clone(),
                    language,
                    raw,
                })
            }
        }
    }
}

fn guess_mime(filename: &str) -> String {
    let ext = Path::new(filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp3" => "audio/mpeg".into(),
        "m4a" | "mp4" => "audio/mp4".into(),
        "wav" => "audio/wav".into(),
        "flac" => "audio/flac".into(),
        "ogg" | "opus" => "audio/ogg".into(),
        "webm" => "audio/webm".into(),
        _ => "application/octet-stream".into(),
    }
}

fn classify_http_error(status: u16, bytes: &[u8]) -> SttError {
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
        401 | 403 => SttError::Auth,
        429 => SttError::RateLimited {
            retry_after_ms: 1000,
        },
        _ => SttError::Provider { status, message },
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SttConfig {
        let mut c = SttConfig::default();
        c.provider = "openai".into();
        c.model = "whisper-1".into();
        c
    }

    #[test]
    fn build_returns_none_when_disabled() {
        let mut c = SttConfig::default();
        c.provider = "none".into();
        assert!(build_from(&c).unwrap().is_none());
    }

    #[test]
    fn build_returns_err_for_unknown_provider() {
        let mut c = SttConfig::default();
        c.provider = "unknown".into();
        assert!(build_from(&c).is_err());
    }

    #[test]
    fn endpoint_path_changes_with_mode() {
        let mut c = cfg();
        c.base_url = Some("https://api.openai.com/v1".into());
        let s = OpenAICompatStt::from_config(&c);
        assert_eq!(
            s.endpoint(SttMode::Transcribe),
            "https://api.openai.com/v1/audio/transcriptions"
        );
        assert_eq!(
            s.endpoint(SttMode::Translate),
            "https://api.openai.com/v1/audio/translations"
        );
    }

    #[test]
    fn endpoint_handles_azure_query_string() {
        let mut c = cfg();
        c.base_url = Some(
            "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/whisper?api-version=2024-02-01".into(),
        );
        let s = OpenAICompatStt::from_config(&c);
        assert_eq!(
            s.endpoint(SttMode::Transcribe),
            "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/whisper/audio/transcriptions?api-version=2024-02-01"
        );
        assert_eq!(
            s.endpoint(SttMode::Translate),
            "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/whisper/audio/translations?api-version=2024-02-01"
        );
    }

    #[test]
    fn guess_mime_covers_common_audio_types() {
        assert_eq!(guess_mime("clip.mp3"), "audio/mpeg");
        assert_eq!(guess_mime("clip.wav"), "audio/wav");
        assert_eq!(guess_mime("clip.m4a"), "audio/mp4");
        assert_eq!(guess_mime("clip.flac"), "audio/flac");
        assert_eq!(guess_mime("clip.UNKNOWN"), "application/octet-stream");
    }

    #[test]
    fn classify_http_error_maps_codes() {
        assert!(matches!(classify_http_error(401, b"{}"), SttError::Auth));
        assert!(matches!(
            classify_http_error(429, b"{}"),
            SttError::RateLimited { .. }
        ));
        let prov = classify_http_error(500, br#"{"error":{"message":"oops"}}"#);
        if let SttError::Provider { status, message } = prov {
            assert_eq!(status, 500);
            assert!(message.contains("oops"));
        }
    }

    #[tokio::test]
    async fn transcribe_rejects_empty_audio() {
        let s = OpenAICompatStt::from_config(&cfg());
        let err = s
            .transcribe(SttRequest {
                audio: vec![],
                filename: "x.mp3".into(),
                language: None,
                prompt: None,
                response_format: None,
                temperature: None,
                mode: SttMode::Transcribe,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, SttError::InvalidInput(_)));
    }

    async fn spawn_one_shot_mock(
        response_body: String,
        status_line: &'static str,
    ) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1");
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 64 * 1024];
            let mut total = Vec::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                if total.windows(4).any(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&total);
                    let body_start = total.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                    let cl = head
                        .lines()
                        .find_map(|l| {
                            let l = l.to_ascii_lowercase();
                            l.strip_prefix("content-length:")
                                .map(|s| s.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if cl > 0 && total.len() - body_start >= cl {
                        break;
                    }
                }
            }
            let body = response_body.as_bytes();
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.write_all(body).await;
            let _ = sock.shutdown().await;
            total
        });
        (url, handle)
    }

    #[tokio::test]
    async fn end_to_end_transcribe_round_trip() {
        std::env::set_var("COS_TEST_STT_KEY", "sk-stt");
        let body = serde_json::json!({"text": "hello world"}).to_string();
        let (base_url, handle) = spawn_one_shot_mock(body, "HTTP/1.1 200 OK").await;
        let mut c = cfg();
        c.base_url = Some(base_url);
        c.api_key_env = Some("COS_TEST_STT_KEY".into());
        let s = OpenAICompatStt::from_config(&c);
        let resp = s
            .transcribe(SttRequest {
                audio: b"fake-mp3-bytes".to_vec(),
                filename: "clip.mp3".into(),
                language: Some("en".into()),
                prompt: None,
                response_format: Some("json".into()),
                temperature: None,
                mode: SttMode::Transcribe,
            })
            .await
            .expect("transcribe");
        assert_eq!(resp.text, "hello world");

        let req = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
        assert!(req.contains("post /v1/audio/transcriptions"));
        assert!(req.contains("authorization: bearer sk-stt"));
        assert!(req.contains("content-type: multipart/form-data"));
        // Multipart body contains the model, language, file part.
        assert!(req.contains("name=\"model\""));
        assert!(req.contains("whisper-1"));
        assert!(req.contains("name=\"language\""));
        assert!(req.contains("name=\"file\""));
        assert!(req.contains("filename=\"clip.mp3\""));

        std::env::remove_var("COS_TEST_STT_KEY");
    }

    #[tokio::test]
    async fn end_to_end_transcribe_text_format_returns_plain_text() {
        std::env::set_var("COS_TEST_STT_KEY_2", "sk-stt2");
        let body = "Just a transcript line.".to_string();
        let (base_url, _h) = spawn_one_shot_mock(body, "HTTP/1.1 200 OK").await;
        let mut c = cfg();
        c.base_url = Some(base_url);
        c.api_key_env = Some("COS_TEST_STT_KEY_2".into());
        let s = OpenAICompatStt::from_config(&c);
        let resp = s
            .transcribe(SttRequest {
                audio: b"fake".to_vec(),
                filename: "x.wav".into(),
                language: None,
                prompt: None,
                response_format: Some("text".into()),
                temperature: None,
                mode: SttMode::Transcribe,
            })
            .await
            .expect("transcribe");
        assert_eq!(resp.text, "Just a transcript line.");
        std::env::remove_var("COS_TEST_STT_KEY_2");
    }

    #[tokio::test]
    async fn azure_deployment_omits_model_field_in_multipart() {
        std::env::set_var("COS_TEST_STT_KEY_3", "sk-stt3");
        let body = serde_json::json!({"text": "ok"}).to_string();
        let (base_url, handle) = spawn_one_shot_mock(body, "HTTP/1.1 200 OK").await;
        // Force the URL to look like an Azure deployment URL.
        let azure_url = format!("{base_url}/deployments/whisper");
        let mut c = cfg();
        c.base_url = Some(azure_url);
        c.api_key_env = Some("COS_TEST_STT_KEY_3".into());
        let s = OpenAICompatStt::from_config(&c);
        let _ = s
            .transcribe(SttRequest {
                audio: b"x".to_vec(),
                filename: "x.mp3".into(),
                language: None,
                prompt: None,
                response_format: Some("json".into()),
                temperature: None,
                mode: SttMode::Transcribe,
            })
            .await
            .expect("transcribe");
        let raw = handle.await.unwrap();
        let req = String::from_utf8_lossy(&raw);
        assert!(
            !req.contains("name=\"model\""),
            "Azure deployment shape must not send model field in multipart"
        );
        std::env::remove_var("COS_TEST_STT_KEY_3");
    }
}
