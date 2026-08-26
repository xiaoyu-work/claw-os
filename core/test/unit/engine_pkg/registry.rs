use super::*;

struct EnginesDirGuard {
    _td: tempfile::TempDir,
}

impl EnginesDirGuard {
    fn new() -> Self {
        let td = tempfile::Builder::new()
            .prefix("cos-engines-test-")
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

fn fake_install(version: &str) -> InstalledVersion {
    InstalledVersion {
        version: version.into(),
        installed_at: Utc::now(),
        bytes: 1024,
        source: format!("local:fake-{version}.zip"),
        sha256: String::new(),
    }
}

fn lay_down_dir(engine: &str, version: &str) {
    let p = super::super::paths::engine_version_dir(engine, version).join("lib");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("placeholder"), b"x").unwrap();
}

#[test]
fn load_or_default_creates_empty_when_missing() {
    let _g = EnginesDirGuard::new();
    let idx = EnginesIndex::load_or_default().unwrap();
    assert!(idx.engines.is_empty());
    assert_eq!(idx.version, SCHEMA_VERSION);
}

#[test]
fn save_then_load_round_trips() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("llama-cpp", fake_install("b4001"))
        .unwrap();
    idx.save().unwrap();
    let reloaded = EnginesIndex::load_or_default().unwrap();
    let entry = reloaded.entry("llama-cpp").unwrap();
    assert_eq!(entry.installed.len(), 1);
    assert_eq!(entry.installed[0].version, "b4001");
}

#[test]
fn record_install_rejects_duplicates() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("llama-cpp", fake_install("b4001"))
        .unwrap();
    let err = idx
        .record_install("llama-cpp", fake_install("b4001"))
        .unwrap_err();
    assert!(matches!(err, RegistryError::DuplicateVersion { .. }));
}

#[test]
fn activate_sets_active_and_moves_previous() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("llama-cpp", fake_install("b3950"))
        .unwrap();
    idx.record_install("llama-cpp", fake_install("b4001"))
        .unwrap();
    let prior = idx.activate("llama-cpp", "b3950").unwrap();
    assert_eq!(prior, "");
    let prior = idx.activate("llama-cpp", "b4001").unwrap();
    assert_eq!(prior, "b3950");
    let entry = idx.entry("llama-cpp").unwrap();
    assert_eq!(entry.active, "b4001");
    assert_eq!(entry.previous, "b3950");
}

#[test]
fn activate_rejects_unknown_version() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("llama-cpp", fake_install("b3950"))
        .unwrap();
    let err = idx.activate("llama-cpp", "b9999").unwrap_err();
    assert!(matches!(err, RegistryError::UnknownVersion { .. }));
}

#[test]
fn rollback_swaps_active_and_previous() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("llama-cpp", fake_install("b3950"))
        .unwrap();
    idx.record_install("llama-cpp", fake_install("b4001"))
        .unwrap();
    idx.activate("llama-cpp", "b3950").unwrap();
    idx.activate("llama-cpp", "b4001").unwrap();
    let (active, previous) = idx.rollback("llama-cpp").unwrap();
    assert_eq!(active, "b3950");
    assert_eq!(previous, "b4001");
    let (active, previous) = idx.rollback("llama-cpp").unwrap();
    assert_eq!(active, "b4001");
    assert_eq!(previous, "b3950");
}

#[test]
fn rollback_errors_with_no_previous() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("llama-cpp", fake_install("b4001"))
        .unwrap();
    idx.activate("llama-cpp", "b4001").unwrap();
    let err = idx.rollback("llama-cpp").unwrap_err();
    assert!(matches!(err, RegistryError::UnknownVersion { .. }));
}

#[test]
fn uninstall_refuses_active_version() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("llama-cpp", fake_install("b4001"))
        .unwrap();
    idx.activate("llama-cpp", "b4001").unwrap();
    let err = idx.uninstall("llama-cpp", "b4001").unwrap_err();
    assert!(matches!(err, RegistryError::UninstallActive { .. }));
}

#[test]
fn uninstall_clears_previous_when_target_is_previous() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("llama-cpp", fake_install("a")).unwrap();
    idx.record_install("llama-cpp", fake_install("b")).unwrap();
    lay_down_dir("llama-cpp", "a");
    lay_down_dir("llama-cpp", "b");
    idx.activate("llama-cpp", "a").unwrap();
    idx.activate("llama-cpp", "b").unwrap();
    idx.uninstall("llama-cpp", "a").unwrap();
    let entry = idx.entry("llama-cpp").unwrap();
    assert_eq!(entry.active, "b");
    assert!(entry.previous.is_empty());
    assert_eq!(entry.installed.len(), 1);
}

#[test]
fn gc_keeps_active_previous_and_last_n() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    for v in &["v1", "v2", "v3", "v4", "v5"] {
        idx.record_install("llama-cpp", fake_install(v)).unwrap();
        lay_down_dir("llama-cpp", v);
    }
    idx.activate("llama-cpp", "v1").unwrap();
    idx.activate("llama-cpp", "v3").unwrap();
    let removed = idx.gc("llama-cpp", 2).unwrap();
    assert_eq!(removed, vec!["v2".to_string()]);
    let entry = idx.entry("llama-cpp").unwrap();
    let kept: Vec<&str> = entry.installed.iter().map(|v| v.version.as_str()).collect();
    assert_eq!(kept, vec!["v1", "v3", "v4", "v5"]);
    // gc no longer rmtree's; caller is expected to invoke
    // `cleanup_uninstalled_dir` after a successful save.
    assert!(super::super::paths::engine_version_dir("llama-cpp", "v2").exists());
    EnginesIndex::cleanup_uninstalled_dir("llama-cpp", "v2").unwrap();
    assert!(!super::super::paths::engine_version_dir("llama-cpp", "v2").exists());
    assert!(super::super::paths::engine_version_dir("llama-cpp", "v3").exists());
}

#[test]
fn pin_unpin_round_trip() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("llama-cpp", fake_install("b4001"))
        .unwrap();
    idx.set_pinned("llama-cpp", true).unwrap();
    assert!(idx.entry("llama-cpp").unwrap().pinned);
    idx.set_pinned("llama-cpp", false).unwrap();
    assert!(!idx.entry("llama-cpp").unwrap().pinned);
}

#[test]
fn save_uses_atomic_rename_visible_to_load() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("ort", fake_install("1.22.0")).unwrap();
    idx.save().unwrap();
    let p = EnginesIndex::path();
    assert!(p.exists());
    let tmp = p.with_extension("json.tmp");
    assert!(!tmp.exists());
}

#[test]
fn list_view_includes_all_known_engines_even_when_empty() {
    let _g = EnginesDirGuard::new();
    let idx = EnginesIndex::empty();
    let v = idx.to_list_view();
    let obj = v.as_object().unwrap();
    for engine in super::super::KNOWN_ENGINES {
        assert!(obj.contains_key(*engine));
    }
}

#[test]
fn info_view_returns_structured_metadata() {
    let _g = EnginesDirGuard::new();
    let mut idx = EnginesIndex::empty();
    idx.record_install("llama-cpp", fake_install("b4001"))
        .unwrap();
    idx.activate("llama-cpp", "b4001").unwrap();
    let v = idx.info_view("llama-cpp");
    assert_eq!(v["engine"], "llama-cpp");
    assert_eq!(v["active"], "b4001");
    assert_eq!(v["installed"].as_array().unwrap().len(), 1);
}
