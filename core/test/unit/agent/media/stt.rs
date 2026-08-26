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
