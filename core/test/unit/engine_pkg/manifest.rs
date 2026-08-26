use super::*;

struct EnginesDirGuard {
    _td: tempfile::TempDir,
}

impl EnginesDirGuard {
    fn new() -> Self {
        let td = tempfile::Builder::new()
            .prefix("cos-engine-manifest-")
            .tempdir()
            .unwrap();
        super::super::paths::set_engines_dir_override(Some(td.path().to_path_buf()));
        Self { _td: td }
    }
}

impl Drop for EnginesDirGuard {
    fn drop(&mut self) {
        super::super::paths::set_engines_dir_override(None);
    }
}

fn lay_down_install(engine: &str, version: &str) {
    std::fs::create_dir_all(super::super::paths::engine_version_dir(engine, version)).unwrap();
}

#[test]
fn load_returns_none_when_file_missing() {
    let _g = EnginesDirGuard::new();
    lay_down_install("llama-cpp", "b4001");
    let m = EngineManifest::load("llama-cpp", "b4001").unwrap();
    assert!(m.is_none(), "no manifest on disk -> Ok(None)");
}

#[test]
fn load_returns_none_when_install_dir_missing() {
    let _g = EnginesDirGuard::new();
    let m = EngineManifest::load("llama-cpp", "b9999").unwrap();
    assert!(m.is_none(), "no install dir at all -> Ok(None)");
}

#[test]
fn load_returns_malformed_for_bad_json() {
    let _g = EnginesDirGuard::new();
    lay_down_install("llama-cpp", "b4001");
    let p = EngineManifest::manifest_path("llama-cpp", "b4001");
    std::fs::write(&p, b"{this is not json").unwrap();
    let err = EngineManifest::load("llama-cpp", "b4001").unwrap_err();
    assert!(
        matches!(err, ManifestError::Malformed { .. }),
        "got {err:?}"
    );
}

#[test]
fn load_rejects_unsupported_schema() {
    let _g = EnginesDirGuard::new();
    lay_down_install("llama-cpp", "b4001");
    let p = EngineManifest::manifest_path("llama-cpp", "b4001");
    let body = serde_json::json!({
        "schema_version": 99,
        "engine": "llama-cpp",
        "version": "b4001",
    });
    std::fs::write(&p, serde_json::to_vec(&body).unwrap()).unwrap();
    let err = EngineManifest::load("llama-cpp", "b4001").unwrap_err();
    assert!(
        matches!(err, ManifestError::UnsupportedSchema { got: 99, .. }),
        "got {err:?}"
    );
}

#[test]
fn save_then_load_round_trips() {
    let _g = EnginesDirGuard::new();
    lay_down_install("llama-cpp", "b4001");
    let mut m = EngineManifest {
        schema_version: SCHEMA_VERSION,
        engine: "llama-cpp".into(),
        version: "b4001".into(),
        abi_tag: "win-x64-cuda-12".into(),
        build_meta: BTreeMap::new(),
        gguf_versions: vec![3],
        model_archs: vec!["llama".into(), "qwen2".into()],
        library_basename: "llama".into(),
        source: String::new(),
    };
    m.build_meta.insert("cuda_version".into(), "12.4".into());
    m.save("llama-cpp", "b4001").unwrap();
    let loaded = EngineManifest::load("llama-cpp", "b4001").unwrap().unwrap();
    // `source` defaults to "shipped" on load if blank on disk.
    assert_eq!(loaded.source, "shipped");
    assert_eq!(loaded.engine, m.engine);
    assert_eq!(loaded.version, m.version);
    assert_eq!(loaded.abi_tag, m.abi_tag);
    assert_eq!(loaded.gguf_versions, m.gguf_versions);
    assert_eq!(loaded.model_archs, m.model_archs);
    assert_eq!(loaded.build_meta.get("cuda_version").unwrap(), "12.4");
}

#[test]
fn synthesize_carries_provenance() {
    let m = EngineManifest::synthesize("llama-cpp", "b4001");
    assert_eq!(m.source, "synthesized");
    assert_eq!(m.engine, "llama-cpp");
    assert_eq!(m.version, "b4001");
    assert_eq!(m.library_basename, "llama");
    assert!(m.gguf_versions.is_empty(), "synth = unknown gguf versions");
    assert!(m.model_archs.is_empty(), "synth = unknown archs");
    assert!(!m.is_authoritative());
}

#[test]
fn synthesize_picks_per_engine_basename() {
    assert_eq!(default_library_basename_for("llama-cpp"), "llama");
    assert_eq!(default_library_basename_for("ort"), "onnxruntime");
    assert_eq!(
        default_library_basename_for("ort-genai"),
        "onnxruntime-genai"
    );
    // Unknown engine still gets a sane fallback rather than an empty string.
    assert_eq!(default_library_basename_for("future-engine"), "lib");
}

#[test]
fn detect_host_abi_tag_matches_compile_target() {
    let tag = detect_host_abi_tag();
    if cfg!(target_os = "windows") {
        assert!(tag.starts_with("win-"), "got {tag}");
    } else if cfg!(target_os = "linux") {
        assert!(tag.starts_with("linux-"), "got {tag}");
    } else if cfg!(target_os = "macos") {
        assert!(tag.starts_with("darwin-"), "got {tag}");
    }
    assert!(
        tag.ends_with("-cpu"),
        "synth tag never claims accelerator: {tag}"
    );
}

#[test]
fn loaded_manifest_with_explicit_source_keeps_it() {
    let _g = EnginesDirGuard::new();
    lay_down_install("llama-cpp", "b4001");
    let p = EngineManifest::manifest_path("llama-cpp", "b4001");
    let body = serde_json::json!({
        "schema_version": 1,
        "engine": "llama-cpp",
        "version": "b4001",
        "source": "curated"
    });
    std::fs::write(&p, serde_json::to_vec_pretty(&body).unwrap()).unwrap();
    let loaded = EngineManifest::load("llama-cpp", "b4001").unwrap().unwrap();
    assert_eq!(loaded.source, "curated");
    assert!(loaded.is_authoritative());
}

#[test]
fn missing_optional_fields_use_defaults() {
    let _g = EnginesDirGuard::new();
    lay_down_install("llama-cpp", "b4001");
    let p = EngineManifest::manifest_path("llama-cpp", "b4001");
    let body = serde_json::json!({
        "engine": "llama-cpp",
        "version": "b4001",
    });
    std::fs::write(&p, serde_json::to_vec(&body).unwrap()).unwrap();
    let loaded = EngineManifest::load("llama-cpp", "b4001").unwrap().unwrap();
    assert_eq!(loaded.schema_version, SCHEMA_VERSION);
    assert_eq!(loaded.library_basename, "llama");
    assert!(loaded.gguf_versions.is_empty());
    assert_eq!(loaded.source, "shipped");
}
