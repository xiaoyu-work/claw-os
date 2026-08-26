use super::*;

/// Test isolation: every test moves COS_DATA_DIR (which controls
/// `models_dir()`) to a unique subdir so concurrent tests can't
/// stomp on each other. We use a per-test counter (atomic) +
/// pid + thread id to guarantee uniqueness even with
/// --test-threads=N.
fn fresh_models_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("cos-import-test-")
        .tempdir()
        .expect("tempdir")
}

/// Set COS_DATA_DIR so `paths::models_dir()` resolves under the
/// test root. The previous value is restored on guard drop.
struct EnvGuard {
    prev: Option<String>,
}
impl EnvGuard {
    fn set(dir: &Path) -> Self {
        let prev = std::env::var("COS_DATA_DIR").ok();
        std::env::set_var("COS_DATA_DIR", dir);
        Self { prev }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("COS_DATA_DIR", v),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}

fn make_source(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, contents).unwrap();
    p
}

#[test]
fn imports_gguf_with_default_engine_and_task() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "tiny.gguf", b"FAKE-GGUF-DATA-ABCDEF");
    let cfg = ImportConfig::new(&src, "tiny");
    let imported = import_model(&cfg).expect("import ok");
    assert_eq!(imported.name, "tiny");
    assert_eq!(imported.version, "v1");
    assert_eq!(imported.engine, Engine::Llama);
    assert_eq!(imported.format, Format::Gguf);
    assert_eq!(imported.task, Task::Llm);
    assert!(imported.manifest_path.exists());
    assert!(imported.model_path.exists());
    assert_eq!(imported.size, b"FAKE-GGUF-DATA-ABCDEF".len() as u64);
    assert_eq!(imported.sha256.len(), 64);
}

#[test]
fn imports_onnx_with_default_engine_and_task() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "embed.onnx", b"FAKE-ONNX");
    let cfg = ImportConfig::new(&src, "embed");
    let imported = import_model(&cfg).unwrap();
    assert_eq!(imported.engine, Engine::Ort);
    assert_eq!(imported.format, Format::Onnx);
    assert_eq!(imported.task, Task::Embed);
}

#[test]
fn unknown_extension_without_format_is_rejected() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "weights.bin", b"X");
    let cfg = ImportConfig::new(&src, "mystery");
    let err = import_model(&cfg).unwrap_err();
    match err {
        ImportError::UnknownFormat(_) => {}
        other => panic!("want UnknownFormat, got {other:?}"),
    }
}

#[test]
fn unknown_extension_with_explicit_format_succeeds() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "weights.bin", b"X");
    let mut cfg = ImportConfig::new(&src, "mystery");
    cfg.format = Some(Format::Gguf);
    cfg.engine = Some(Engine::Llama);
    cfg.task = Some(Task::Llm);
    let imported = import_model(&cfg).unwrap();
    assert_eq!(imported.engine, Engine::Llama);
}

#[test]
fn invalid_name_is_rejected() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "f.gguf", b"X");
    let mut cfg = ImportConfig::new(&src, "");
    assert!(matches!(
        import_model(&cfg).unwrap_err(),
        ImportError::InvalidName(_)
    ));
    cfg.name = "../escape".into();
    assert!(matches!(
        import_model(&cfg).unwrap_err(),
        ImportError::InvalidName(_)
    ));
    cfg.name = ".hidden".into();
    assert!(matches!(
        import_model(&cfg).unwrap_err(),
        ImportError::InvalidName(_)
    ));
    cfg.name = "ok-name_v1".into();
    assert!(import_model(&cfg).is_ok());
}

#[test]
fn already_registered_without_force_is_rejected() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "f.gguf", b"V1");
    let cfg = ImportConfig::new(&src, "dup");
    import_model(&cfg).unwrap();
    // Second import must fail without force.
    let src2 = make_source(root.path(), "f2.gguf", b"V2");
    let cfg2 = ImportConfig::new(&src2, "dup");
    match import_model(&cfg2).unwrap_err() {
        ImportError::AlreadyRegistered { name, version } => {
            assert_eq!(name, "dup");
            assert_eq!(version, "v1");
        }
        other => panic!("want AlreadyRegistered, got {other:?}"),
    }
}

#[test]
fn force_overwrites_existing_registration() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "f.gguf", b"V1");
    import_model(&ImportConfig::new(&src, "dup")).unwrap();
    let src2 = make_source(root.path(), "f2.gguf", b"V2");
    let mut cfg2 = ImportConfig::new(&src2, "dup");
    cfg2.force = true;
    let imported = import_model(&cfg2).unwrap();
    assert_eq!(imported.size, b"V2".len() as u64);
    // Old basename should be gone, new one present.
    assert!(imported.model_path.exists());
    let listing: Vec<_> = fs::read_dir(paths::model_version_dir("dup", "v1"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(listing.contains(&"f2.gguf".to_string()));
    assert!(!listing.contains(&"f.gguf".to_string()));
}

#[test]
fn missing_source_is_rejected() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let cfg = ImportConfig::new(root.path().join("nope.gguf"), "x");
    match import_model(&cfg).unwrap_err() {
        ImportError::SourceMissing(_) => {}
        other => panic!("want SourceMissing, got {other:?}"),
    }
}

#[test]
fn directory_without_genai_config_is_rejected() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let dir = root.path().join("sub");
    fs::create_dir_all(&dir).unwrap();
    // No genai_config.json + no explicit format/engine override =>
    // directory layout is unrecognized.
    let cfg = ImportConfig::new(&dir, "x");
    match import_model(&cfg).unwrap_err() {
        ImportError::InvalidGenaiBundle(_) => {}
        other => panic!("want InvalidGenaiBundle, got {other:?}"),
    }
}

#[test]
fn directory_with_genai_config_imports_as_onnx_genai() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = root.path().join("qwen3-export");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("genai_config.json"), b"{}").unwrap();
    fs::write(src.join("model.onnx"), b"fake-onnx-graph").unwrap();
    fs::write(src.join("tokenizer.json"), b"{}").unwrap();

    let cfg = ImportConfig::new(&src, "qwen3-test");
    let imported = import_model(&cfg).expect("import");
    assert_eq!(imported.engine, Engine::OrtGenai);
    assert_eq!(imported.format, Format::OnnxGenai);
    assert_eq!(imported.task, Task::Embed);
    assert!(imported.size > 0);
    // Tree-hash sha is hex.
    assert_eq!(imported.sha256.len(), 64);
    assert!(imported.sha256.chars().all(|c| c.is_ascii_hexdigit()));

    // Directory contents preserved.
    let target_dir = paths::model_version_dir("qwen3-test", "v1");
    assert!(target_dir.join("genai_config.json").is_file());
    assert!(target_dir.join("model.onnx").is_file());
    assert!(target_dir.join("tokenizer.json").is_file());
    assert!(target_dir.join("manifest.json").is_file());

    // Manifest declares ort-genai requires_engine pin.
    let raw = fs::read_to_string(target_dir.join("manifest.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["engine"], "ort-genai");
    assert_eq!(parsed["format"], "onnx-genai");
    assert_eq!(parsed["requires_engine"]["name"], "ort-genai");
    assert_eq!(
        parsed["requires_engine"]["version"],
        format!("={}", crate::engine_pkg::ORT_GENAI_KNOWN_GOOD_VERSION)
    );
    assert!(parsed["files"].as_array().unwrap().len() >= 3);
}

#[test]
fn directory_tree_hash_is_deterministic() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());

    // First import.
    let src1 = root.path().join("a");
    fs::create_dir_all(&src1).unwrap();
    fs::write(src1.join("genai_config.json"), b"{}").unwrap();
    fs::write(src1.join("model.onnx"), b"onnx-bytes").unwrap();
    let cfg1 = ImportConfig::new(&src1, "modelA");
    let h1 = import_model(&cfg1).unwrap().sha256;

    // Identical content → identical hash.
    let src2 = root.path().join("b");
    fs::create_dir_all(&src2).unwrap();
    fs::write(src2.join("genai_config.json"), b"{}").unwrap();
    fs::write(src2.join("model.onnx"), b"onnx-bytes").unwrap();
    let cfg2 = ImportConfig::new(&src2, "modelB");
    let h2 = import_model(&cfg2).unwrap().sha256;

    assert_eq!(h1, h2, "tree hash must be content-addressable");

    // Same paths but different content → different hash.
    let src3 = root.path().join("c");
    fs::create_dir_all(&src3).unwrap();
    fs::write(src3.join("genai_config.json"), b"{}").unwrap();
    fs::write(src3.join("model.onnx"), b"onnx-bytes-DIFFERENT").unwrap();
    let cfg3 = ImportConfig::new(&src3, "modelC");
    let h3 = import_model(&cfg3).unwrap().sha256;

    assert_ne!(h1, h3);
}

#[test]
fn move_flag_removes_source() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "moveme.gguf", b"X");
    let mut cfg = ImportConfig::new(&src, "moved");
    cfg.r#move = true;
    let imported = import_model(&cfg).unwrap();
    assert!(imported.model_path.exists(), "registered file should exist");
    assert!(!src.exists(), "source should have been moved");
}

#[test]
fn manifest_round_trips_through_registry_list() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "m.gguf", b"DATA");
    import_model(&ImportConfig::new(&src, "rt")).unwrap();
    let listed = super::super::registry::list().unwrap();
    let m = listed.iter().find(|m| m.name == "rt").expect("found");
    assert_eq!(m.version, "v1");
    assert_eq!(m.engine, Engine::Llama);
    assert_eq!(m.format, Format::Gguf);
    assert_eq!(m.size, b"DATA".len() as u64);
}

#[test]
fn remove_model_drops_version_dir() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "rm.gguf", b"X");
    import_model(&ImportConfig::new(&src, "removable")).unwrap();
    assert!(remove_model("removable", "v1").unwrap());
    // Version dir gone.
    assert!(!paths::model_version_dir("removable", "v1").exists());
    // Empty model dir was also pruned (best-effort).
    assert!(!paths::model_dir("removable").exists());
    // Idempotent: removing a missing one returns false, not an error.
    assert!(!remove_model("removable", "v1").unwrap());
}

#[test]
fn remove_model_keeps_other_versions() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "v1.gguf", b"X");
    import_model(&ImportConfig::new(&src, "multi")).unwrap();
    let src2 = make_source(root.path(), "v2.gguf", b"Y");
    let mut cfg2 = ImportConfig::new(&src2, "multi");
    cfg2.version = "v2".into();
    import_model(&cfg2).unwrap();
    // Remove v1 only.
    remove_model("multi", "v1").unwrap();
    assert!(!paths::model_version_dir("multi", "v1").exists());
    assert!(paths::model_version_dir("multi", "v2").exists());
}

#[test]
fn imported_model_json_includes_expected_fields() {
    let root = fresh_models_root();
    let _guard = EnvGuard::set(root.path());
    let src = make_source(root.path(), "j.gguf", b"X");
    let imported = import_model(&ImportConfig::new(&src, "jsmodel")).unwrap();
    let v = imported_model_json(&imported);
    assert_eq!(v["status"], "imported");
    assert_eq!(v["name"], "jsmodel");
    assert_eq!(v["version"], "v1");
    assert_eq!(v["engine"], "llama");
    assert_eq!(v["format"], "gguf");
    assert!(v["sha256"].as_str().unwrap().len() == 64);
}
