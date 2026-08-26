use super::*;

#[test]
fn l2_normalize_unit_vector_is_idempotent() {
    let mut v = vec![3.0f32, 4.0, 0.0];
    l2_normalize(&mut v);
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5, "norm = {norm}");
}

#[test]
fn l2_normalize_zero_vector_remains_zero() {
    let mut v = vec![0.0f32; 4];
    l2_normalize(&mut v);
    assert!(v.iter().all(|x| *x == 0.0));
}

#[test]
fn precheck_reports_missing_dir() {
    let mut cfg = crate::config::EmbedConfig::default();
    cfg.provider = "qwen3-local".into();
    cfg.model_dir = Some("/definitely/does/not/exist/qwen3".to_string());
    let err = precheck(&cfg).unwrap_err();
    assert!(err.contains("does not exist"));
}

#[test]
fn embedder_constructs_with_explicit_dir() {
    let e = Qwen3GenaiEmbedder::new("/some/path");
    assert_eq!(e.name(), "qwen3-local");
    assert_eq!(e.model(), MODEL_NAME);
    assert!(
        !e.is_configured(),
        "non-existent path should not be configured"
    );
}

#[test]
fn resolve_model_dir_falls_back_to_default() {
    let mut cfg = crate::config::EmbedConfig::default();
    cfg.model_dir = None;
    let dir = resolve_model_dir(&cfg);
    // Pinned default — if the registry layout changes, this test
    // catches it.
    assert!(
        dir.ends_with("qwen3-embedding-0.6b/v1") || dir.ends_with("qwen3-embedding-0.6b\\v1")
    );
}

#[test]
fn resolve_model_dir_uses_explicit_override() {
    let mut cfg = crate::config::EmbedConfig::default();
    cfg.model_dir = Some("C:\\custom\\qwen".to_string());
    assert_eq!(resolve_model_dir(&cfg), PathBuf::from("C:\\custom\\qwen"));
}

#[test]
fn empty_inputs_rejected() {
    let e = Qwen3GenaiEmbedder::new("/nonexistent");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let res = rt.block_on(e.embed(EmbedRequest { inputs: vec![] }));
    match res {
        Err(EmbedError::InvalidInput(_)) => {}
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}
