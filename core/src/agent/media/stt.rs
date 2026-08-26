//! Speech-to-text provider trait + registry.
//!
//! Concrete backends register themselves with [`SttRegistry`] under
//! a stable name. Callers ask the registry for a provider, hand it
//! a [`SttRequest`] containing audio bytes, and receive the
//! transcribed text plus per-segment metadata.
//!
//! Backends planned:
//!
//!   * Cloud: groq, openai, mistral, xai.
//!   * Local: whisper (routed through `crate::model::tasks::stt`).
//!
//! This commit ships only the trait, registry, and a `noop`
//! reference impl. Real backends arrive in follow-up commits.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::tts::AudioFormat;
use super::MediaError;

#[derive(Debug, Clone)]
pub struct SttRequest {
    pub audio: Vec<u8>,
    pub format: AudioFormat,
    pub language: Option<String>,
    /// Hint to the backend (e.g. "json", "verbose_json", "text").
    /// Ignored if the backend doesn't support format selection.
    pub response_hint: Option<String>,
}

impl SttRequest {
    pub fn new(audio: Vec<u8>, format: AudioFormat) -> Self {
        Self {
            audio,
            format,
            language: None,
            response_hint: None,
        }
    }

    pub fn validate(&self) -> Result<(), MediaError> {
        if self.audio.is_empty() {
            return Err(MediaError::InvalidRequest(
                "stt: audio bytes must be non-empty".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SttSegment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SttResponse {
    pub text: String,
    pub language: Option<String>,
    pub segments: Vec<SttSegment>,
}

#[async_trait]
pub trait SttProvider: Send + Sync {
    fn name(&self) -> &str;
    fn is_configured(&self) -> bool;
    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, MediaError>;
}

#[derive(Default, Clone)]
pub struct SttRegistry {
    inner: Arc<BTreeMap<String, Arc<dyn SttProvider>>>,
}

impl SttRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_providers() -> Self {
        let mut map = BTreeMap::new();
        let noop: Arc<dyn SttProvider> = Arc::new(NoopStt);
        map.insert(noop.name().to_string(), noop);
        Self {
            inner: Arc::new(map),
        }
    }

    pub fn register(&mut self, provider: Arc<dyn SttProvider>) {
        let name = provider.name().to_string();
        let mut map = (*self.inner).clone();
        map.insert(name, provider);
        self.inner = Arc::new(map);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn SttProvider>> {
        self.inner.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.inner.keys().cloned().collect()
    }
}

/// Reference impl: returns an empty transcript so the call path is
/// exercisable without a real backend.
pub struct NoopStt;

#[async_trait]
impl SttProvider for NoopStt {
    fn name(&self) -> &str {
        "noop"
    }
    fn is_configured(&self) -> bool {
        true
    }
    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, MediaError> {
        request.validate()?;
        Ok(SttResponse {
            text: String::new(),
            language: request.language,
            segments: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/media/stt.rs"
    ));
}
