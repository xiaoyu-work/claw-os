use super::*;

#[test]
fn defaults_pin_xai_model_and_url() {
    let cfg = XaiImageGenConfig::default();
    assert_eq!(cfg.model, DEFAULT_MODEL);
    let p = XaiImageGenProvider::new(cfg);
    assert_eq!(<XaiImageGenProvider as ImageGenProvider>::name(&p), "xai");
}

#[test]
fn is_configured_requires_api_key() {
    let p1 = XaiImageGenProvider::new(XaiImageGenConfig::default());
    assert!(!<XaiImageGenProvider as ImageGenProvider>::is_configured(
        &p1
    ));
    let mut c = XaiImageGenConfig::default();
    c.api_key = Some("sk".to_string());
    let p2 = XaiImageGenProvider::new(c);
    assert!(<XaiImageGenProvider as ImageGenProvider>::is_configured(
        &p2
    ));
}

#[tokio::test]
async fn rejects_n_above_xai_cap() {
    let mut c = XaiImageGenConfig::default();
    c.api_key = Some("sk".to_string());
    let p = XaiImageGenProvider::new(c);
    let mut req = ImageGenRequest::new("cat");
    req.n = XAI_MAX_N + 1;
    let err = p.generate(req).await.unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[tokio::test]
async fn rejects_when_api_key_missing() {
    let p = XaiImageGenProvider::new(XaiImageGenConfig::default());
    let err = p.generate(ImageGenRequest::new("cat")).await.unwrap_err();
    assert!(matches!(err, MediaError::NotConfigured(_)));
}

#[test]
fn xai_base_url_constant() {
    assert_eq!(XAI_BASE_URL, "https://api.x.ai/v1");
}

#[test]
fn xai_max_n_constant() {
    assert_eq!(XAI_MAX_N, 10);
}
