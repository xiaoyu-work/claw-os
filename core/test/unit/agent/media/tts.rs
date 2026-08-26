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
