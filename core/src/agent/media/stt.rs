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
    use super::*;

    #[test]
    fn request_rejects_empty_audio() {
        let r = SttRequest::new(Vec::new(), AudioFormat::Wav);
        assert!(r.validate().is_err());
    }

    #[tokio::test]
    async fn noop_returns_empty_transcript() {
        let p = NoopStt;
        let mut r = SttRequest::new(vec![1, 2, 3], AudioFormat::Wav);
        r.language = Some("en".to_string());
        let resp = p.transcribe(r).await.unwrap();
        assert!(resp.text.is_empty());
        assert_eq!(resp.language.as_deref(), Some("en"));
        assert!(resp.segments.is_empty());
    }

    #[tokio::test]
    async fn noop_validates_request() {
        let p = NoopStt;
        let err = p
            .transcribe(SttRequest::new(Vec::new(), AudioFormat::Wav))
            .await
            .unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[test]
    fn registry_default_has_noop() {
        let r = SttRegistry::with_default_providers();
        assert!(r.get("noop").is_some());
        assert!(r.names().contains(&"noop".to_string()));
    }

    #[test]
    fn registry_register_and_lookup() {
        struct Custom;
        #[async_trait]
        impl SttProvider for Custom {
            fn name(&self) -> &str {
                "custom"
            }
            fn is_configured(&self) -> bool {
                false
            }
            async fn transcribe(&self, _: SttRequest) -> Result<SttResponse, MediaError> {
                Err(MediaError::NotConfigured("custom".to_string()))
            }
        }
        let mut r = SttRegistry::new();
        r.register(Arc::new(Custom));
        assert!(r.get("custom").is_some());
    }

    #[test]
    fn segment_round_trip() {
        let s = SttSegment {
            start_ms: 0,
            end_ms: 1000,
            text: "hi".to_string(),
        };
        assert_eq!(s.start_ms, 0);
        assert_eq!(s.end_ms, 1000);
        assert_eq!(s.text, "hi");
    }

    #[test]
    fn registry_unknown_name_returns_none() {
        let r = SttRegistry::with_default_providers();
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn registry_clone_independent_after_mutation() {
        let r1 = SttRegistry::with_default_providers();
        let mut r2 = r1.clone();
        struct Extra;
        #[async_trait]
        impl SttProvider for Extra {
            fn name(&self) -> &str {
                "extra"
            }
            fn is_configured(&self) -> bool {
                true
            }
            async fn transcribe(&self, _: SttRequest) -> Result<SttResponse, MediaError> {
                Ok(SttResponse {
                    text: String::new(),
                    language: None,
                    segments: Vec::new(),
                })
            }
        }
        r2.register(Arc::new(Extra));
        assert!(r1.get("extra").is_none());
        assert!(r2.get("extra").is_some());
    }
}
