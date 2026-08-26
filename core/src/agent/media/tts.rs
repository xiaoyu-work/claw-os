//! Text-to-speech provider trait + registry.
//!
//! Concrete backends register themselves with [`TtsRegistry`] under
//! a stable name. Callers ask the registry for a provider, build a
//! [`TtsRequest`], and receive raw audio bytes plus metadata.
//!
//! Backends planned:
//!
//!   * Cloud: edge, elevenlabs, openai, gemini, xai, minimax,
//!     mistral.
//!   * Local: piper / kittentts (routed through
//!     `crate::model::tasks::tts`).
//!
//! This commit ships only the trait, the registry, and a `noop`
//! provider that returns a deterministic empty WAV header. Real
//! backends land in follow-up commits.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::MediaError;

/// Audio container for the returned bytes. Most cloud providers
/// emit MP3 or WAV; some (Piper) emit raw PCM. Callers that need
/// to play / save / re-encode use this hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Wav,
    Mp3,
    Ogg,
    Pcm16,
    Other,
}

impl AudioFormat {
    pub fn extension(self) -> &'static str {
        match self {
            AudioFormat::Wav => "wav",
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Ogg => "ogg",
            AudioFormat::Pcm16 => "pcm",
            AudioFormat::Other => "bin",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TtsRequest {
    pub text: String,
    pub voice: Option<String>,
    pub language: Option<String>,
    pub speed: Option<f32>,
    pub format: Option<AudioFormat>,
}

impl TtsRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            voice: None,
            language: None,
            speed: None,
            format: None,
        }
    }

    pub fn validate(&self) -> Result<(), MediaError> {
        if self.text.trim().is_empty() {
            return Err(MediaError::InvalidRequest(
                "tts: text must be non-empty".to_string(),
            ));
        }
        if let Some(s) = self.speed {
            if !(0.1..=4.0).contains(&s) {
                return Err(MediaError::InvalidRequest(format!(
                    "tts: speed {s} out of supported range [0.1, 4.0]"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TtsResponse {
    pub audio: Vec<u8>,
    pub format: AudioFormat,
    pub sample_rate: Option<u32>,
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    fn name(&self) -> &str;
    fn is_configured(&self) -> bool;
    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, MediaError>;
}

/// In-memory provider registry. Cheap to clone (Arc-shared map).
#[derive(Default, Clone)]
pub struct TtsRegistry {
    inner: Arc<BTreeMap<String, Arc<dyn TtsProvider>>>,
}

impl TtsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_providers() -> Self {
        let mut map = BTreeMap::new();
        let noop: Arc<dyn TtsProvider> = Arc::new(NoopTts);
        map.insert(noop.name().to_string(), noop);
        Self {
            inner: Arc::new(map),
        }
    }

    pub fn register(&mut self, provider: Arc<dyn TtsProvider>) {
        let name = provider.name().to_string();
        let mut map = (*self.inner).clone();
        map.insert(name, provider);
        self.inner = Arc::new(map);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn TtsProvider>> {
        self.inner.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.inner.keys().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// Reference implementation: returns a 0-sample WAV header so call
/// sites can be exercised end-to-end without a real backend. Always
/// `is_configured == true`.
pub struct NoopTts;

#[async_trait]
impl TtsProvider for NoopTts {
    fn name(&self) -> &str {
        "noop"
    }

    fn is_configured(&self) -> bool {
        true
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, MediaError> {
        request.validate()?;
        let format = request.format.unwrap_or(AudioFormat::Wav);
        let audio = match format {
            AudioFormat::Wav => super::voice::wav::empty_header(1, 22_050),
            _ => Vec::new(),
        };
        Ok(TtsResponse {
            audio,
            format,
            sample_rate: Some(22_050),
        })
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/media/tts.rs"
    ));
}
