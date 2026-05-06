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
            AudioFormat::Wav => empty_wav_header(),
            _ => Vec::new(),
        };
        Ok(TtsResponse {
            audio,
            format,
            sample_rate: Some(22_050),
        })
    }
}

/// Minimal WAV header: 44 bytes, RIFF/WAVE, mono 16-bit PCM @ 22050,
/// zero data chunk. Good enough that an ffmpeg-style probe sees a
/// well-formed (silent) file.
fn empty_wav_header() -> Vec<u8> {
    let mut v = Vec::with_capacity(44);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&36u32.to_le_bytes());
    v.extend_from_slice(b"WAVE");
    v.extend_from_slice(b"fmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&22_050u32.to_le_bytes());
    v.extend_from_slice(&44_100u32.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&0u32.to_le_bytes());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_format_extensions() {
        assert_eq!(AudioFormat::Wav.extension(), "wav");
        assert_eq!(AudioFormat::Mp3.extension(), "mp3");
        assert_eq!(AudioFormat::Ogg.extension(), "ogg");
        assert_eq!(AudioFormat::Pcm16.extension(), "pcm");
        assert_eq!(AudioFormat::Other.extension(), "bin");
    }

    #[test]
    fn request_rejects_empty_text() {
        assert!(TtsRequest::new("   ").validate().is_err());
        assert!(TtsRequest::new("").validate().is_err());
    }

    #[test]
    fn request_rejects_speed_out_of_range() {
        let mut r = TtsRequest::new("hello");
        r.speed = Some(0.0);
        assert!(r.validate().is_err());
        r.speed = Some(5.0);
        assert!(r.validate().is_err());
        r.speed = Some(1.5);
        assert!(r.validate().is_ok());
    }

    #[tokio::test]
    async fn noop_returns_wav_by_default() {
        let p = NoopTts;
        let resp = p.synthesize(TtsRequest::new("hi")).await.unwrap();
        assert_eq!(resp.format, AudioFormat::Wav);
        assert_eq!(resp.audio.len(), 44);
        assert_eq!(&resp.audio[..4], b"RIFF");
        assert_eq!(&resp.audio[8..12], b"WAVE");
    }

    #[tokio::test]
    async fn noop_honours_requested_format() {
        let p = NoopTts;
        let mut r = TtsRequest::new("hi");
        r.format = Some(AudioFormat::Mp3);
        let resp = p.synthesize(r).await.unwrap();
        assert_eq!(resp.format, AudioFormat::Mp3);
        assert!(resp.audio.is_empty());
    }

    #[tokio::test]
    async fn noop_validates_request() {
        let p = NoopTts;
        let err = p.synthesize(TtsRequest::new("")).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[test]
    fn registry_default_has_noop() {
        let r = TtsRegistry::with_default_providers();
        assert!(!r.is_empty());
        assert!(r.get("noop").is_some());
        assert!(r.names().contains(&"noop".to_string()));
    }

    #[test]
    fn registry_register_and_lookup() {
        struct Custom;
        #[async_trait]
        impl TtsProvider for Custom {
            fn name(&self) -> &str {
                "custom"
            }
            fn is_configured(&self) -> bool {
                false
            }
            async fn synthesize(&self, _: TtsRequest) -> Result<TtsResponse, MediaError> {
                Err(MediaError::NotConfigured("custom".to_string()))
            }
        }
        let mut r = TtsRegistry::new();
        r.register(Arc::new(Custom));
        assert!(r.get("custom").is_some());
        assert_eq!(r.names(), vec!["custom".to_string()]);
    }

    #[test]
    fn registry_clone_independent_after_mutation() {
        let r1 = TtsRegistry::with_default_providers();
        let mut r2 = r1.clone();
        struct Extra;
        #[async_trait]
        impl TtsProvider for Extra {
            fn name(&self) -> &str {
                "extra"
            }
            fn is_configured(&self) -> bool {
                true
            }
            async fn synthesize(&self, _: TtsRequest) -> Result<TtsResponse, MediaError> {
                Ok(TtsResponse {
                    audio: vec![],
                    format: AudioFormat::Wav,
                    sample_rate: None,
                })
            }
        }
        r2.register(Arc::new(Extra));
        assert!(r1.get("extra").is_none());
        assert!(r2.get("extra").is_some());
    }

    #[test]
    fn registry_unknown_name_returns_none() {
        let r = TtsRegistry::with_default_providers();
        assert!(r.get("does-not-exist").is_none());
    }
}
