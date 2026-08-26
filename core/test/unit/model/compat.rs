use super::*;
use crate::engine_pkg;
use crate::engine_pkg::registry::{EnginesIndex, InstalledVersion};
use chrono::Utc;

struct EnginesDirGuard {
    _td: tempfile::TempDir,
}

impl EnginesDirGuard {
    fn new() -> Self {
        let td = tempfile::Builder::new()
            .prefix("cos-compat-")
            .tempdir()
            .unwrap();
        engine_pkg::paths::set_engines_dir_override(Some(td.path().to_path_buf()));
        Self { _td: td }
    }
}

impl Drop for EnginesDirGuard {
    fn drop(&mut self) {
        engine_pkg::paths::set_engines_dir_override(None);
    }
}

fn lay_down_active(engine: &str, version: &str) {
    std::fs::create_dir_all(engine_pkg::paths::engine_version_dir(engine, version)).unwrap();
    let mut idx = EnginesIndex::empty();
    idx.record_install(
        engine,
        InstalledVersion {
            version: version.into(),
            installed_at: Utc::now(),
            bytes: 0,
            source: "local".into(),
            sha256: String::new(),
        },
    )
    .unwrap();
    idx.activate(engine, version).unwrap();
    idx.save().unwrap();
}

fn lay_down_manifest(engine: &str, version: &str, m: &EngineManifest) {
    std::fs::create_dir_all(engine_pkg::paths::engine_version_dir(engine, version)).unwrap();
    m.save(engine, version).unwrap();
}

// --- Range parser ---

#[test]
fn star_parses_to_empty_range() {
    let r = parse_range("*").unwrap();
    assert!(r.comparators.is_empty());
}

#[test]
fn whitespace_parses_to_empty_range() {
    let r = parse_range("  ").unwrap();
    assert!(r.comparators.is_empty());
}

#[test]
fn comma_separates_and_clauses() {
    let r = parse_range(">=b3900, <b4500").unwrap();
    assert_eq!(r.comparators.len(), 2);
    assert_eq!(r.comparators[0].op, Op::Ge);
    assert_eq!(r.comparators[0].operand, "b3900");
    assert_eq!(r.comparators[1].op, Op::Lt);
    assert_eq!(r.comparators[1].operand, "b4500");
}

#[test]
fn bare_operand_is_eq() {
    let r = parse_range("b4001").unwrap();
    assert_eq!(r.comparators.len(), 1);
    assert_eq!(r.comparators[0].op, Op::Eq);
    assert_eq!(r.comparators[0].operand, "b4001");
}

#[test]
fn empty_operand_rejected() {
    let err = parse_range(">=").unwrap_err();
    assert!(err.contains("no operand"), "{err}");
}

// --- Llama build comparator ---

#[test]
fn llama_build_compare_handles_large_numbers() {
    // Lex sort would say "b3950" > "b40000" — verify we don't fall
    // into that trap.
    let ord = compare_versions("llama-cpp", "b40000", "b3950").unwrap();
    assert_eq!(ord, std::cmp::Ordering::Greater);
}

#[test]
fn llama_build_match_version_in_range() {
    let r = parse_range(">=b3900, <b4500").unwrap();
    match_version("llama-cpp", "b4001", &r).unwrap();
}

#[test]
fn llama_build_match_version_below_range() {
    let r = parse_range(">=b3900").unwrap();
    let err = match_version("llama-cpp", "b3800", &r).unwrap_err();
    assert!(err.contains("Ge"), "{err}");
}

#[test]
fn llama_build_match_version_above_range() {
    let r = parse_range("<b4500").unwrap();
    let err = match_version("llama-cpp", "b4600", &r).unwrap_err();
    assert!(err.contains("Lt"), "{err}");
}

#[test]
fn llama_build_eq_strict() {
    let r = parse_range("=b4001").unwrap();
    match_version("llama-cpp", "b4001", &r).unwrap();
    match_version("llama-cpp", "b4002", &r).unwrap_err();
}

#[test]
fn llama_build_invalid_active_surfaces_error() {
    let r = parse_range(">=b3900").unwrap();
    let err = match_version("llama-cpp", "latest", &r).unwrap_err();
    assert!(err.contains("not a llama.cpp build number"), "{err}");
}

// --- Semver comparator ---

#[test]
fn semver_match_version_in_range() {
    let r = parse_range(">=1.22.0, <2.0.0").unwrap();
    match_version("ort", "1.22.5", &r).unwrap();
}

#[test]
fn semver_accepts_github_v_prefix() {
    let r = parse_range(">=0.12.0, <0.13.0").unwrap();
    match_version("ort-genai", "v0.12.2", &r).unwrap();
    match_version("ort-genai", "v0.13.1", &r).unwrap_err();
}

#[test]
fn semver_match_version_above_major() {
    let r = parse_range("<2.0.0").unwrap();
    match_version("ort", "2.0.0", &r).unwrap_err();
}

#[test]
fn semver_invalid_active_surfaces_error() {
    let r = parse_range(">=1.0.0").unwrap();
    let err = match_version("ort", "b4001", &r).unwrap_err();
    assert!(err.contains("not semver"), "{err}");
}

// --- check_against_active_engine ---

fn req(engine: &str, version_range: &str) -> EngineRequirement {
    EngineRequirement {
        name: engine.into(),
        version: version_range.into(),
    }
}

#[test]
fn check_passes_when_no_requires() {
    let _g = EnginesDirGuard::new();
    // No active engine; but `requires_engine` is None → pass.
    let m = make_minimal_model(None);
    check_engine_compat(&m).unwrap();
}

#[test]
fn check_fails_when_no_active_engine() {
    let _g = EnginesDirGuard::new();
    let err = check_against_active_engine(&req("llama-cpp", "*"), None, None).unwrap_err();
    assert!(matches!(err, CompatError::NoActiveEngine { .. }));
}

#[test]
fn check_passes_with_star_range_and_active_engine() {
    let _g = EnginesDirGuard::new();
    lay_down_active("llama-cpp", "b4001");
    check_against_active_engine(&req("llama-cpp", "*"), None, None).unwrap();
}

#[test]
fn check_fails_when_active_below_min() {
    let _g = EnginesDirGuard::new();
    lay_down_active("llama-cpp", "b3800");
    let err =
        check_against_active_engine(&req("llama-cpp", ">=b3900"), None, None).unwrap_err();
    match err {
        CompatError::VersionOutOfRange {
            engine,
            active,
            range,
            ..
        } => {
            assert_eq!(engine, "llama-cpp");
            assert_eq!(active, "b3800");
            assert_eq!(range, ">=b3900");
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn check_passes_when_active_in_bounded_range() {
    let _g = EnginesDirGuard::new();
    lay_down_active("llama-cpp", "b4001");
    check_against_active_engine(&req("llama-cpp", ">=b3900, <b4500"), None, None).unwrap();
}

#[test]
fn check_with_authoritative_manifest_enforces_gguf() {
    let _g = EnginesDirGuard::new();
    lay_down_active("llama-cpp", "b4001");
    let mut m = EngineManifest::synthesize("llama-cpp", "b4001");
    m.source = "shipped".into();
    m.gguf_versions = vec![3];
    lay_down_manifest("llama-cpp", "b4001", &m);
    // Model needs GGUF v4, engine claims v3 only → fail.
    let err = check_against_active_engine(&req("llama-cpp", "*"), Some(4), None).unwrap_err();
    assert!(matches!(
        err,
        CompatError::GgufVersionUnsupported { needed: 4, .. }
    ));
}

#[test]
fn check_with_authoritative_manifest_passes_listed_arch() {
    let _g = EnginesDirGuard::new();
    lay_down_active("llama-cpp", "b4001");
    let mut m = EngineManifest::synthesize("llama-cpp", "b4001");
    m.source = "shipped".into();
    m.model_archs = vec!["llama".into(), "qwen2".into()];
    lay_down_manifest("llama-cpp", "b4001", &m);
    check_against_active_engine(&req("llama-cpp", "*"), None, Some("qwen2")).unwrap();
}

#[test]
fn check_with_authoritative_manifest_rejects_unlisted_arch() {
    let _g = EnginesDirGuard::new();
    lay_down_active("llama-cpp", "b4001");
    let mut m = EngineManifest::synthesize("llama-cpp", "b4001");
    m.source = "shipped".into();
    m.model_archs = vec!["llama".into()];
    lay_down_manifest("llama-cpp", "b4001", &m);
    let err =
        check_against_active_engine(&req("llama-cpp", "*"), None, Some("falcon")).unwrap_err();
    assert!(matches!(err, CompatError::ArchUnsupported { .. }));
}

#[test]
fn check_with_synthesized_manifest_skips_capability_checks() {
    // No manifest on disk → no capability enforcement, only version.
    let _g = EnginesDirGuard::new();
    lay_down_active("llama-cpp", "b4001");
    // Model needs GGUF v4 — we should NOT fail because no
    // authoritative manifest claims a list.
    check_against_active_engine(&req("llama-cpp", "*"), Some(4), Some("anything")).unwrap();
}

#[test]
fn check_with_empty_capability_lists_skips_those_checks() {
    // Authoritative manifest exists but doesn't enumerate capabilities.
    let _g = EnginesDirGuard::new();
    lay_down_active("llama-cpp", "b4001");
    let mut m = EngineManifest::synthesize("llama-cpp", "b4001");
    m.source = "shipped".into();
    // gguf_versions and model_archs both empty → advisory.
    lay_down_manifest("llama-cpp", "b4001", &m);
    check_against_active_engine(&req("llama-cpp", "*"), Some(99), Some("future-arch")).unwrap();
}

#[test]
fn check_with_empty_engine_name_rejected() {
    let _g = EnginesDirGuard::new();
    let err = check_against_active_engine(&req("", "*"), None, None).unwrap_err();
    assert!(matches!(err, CompatError::InvalidRange { .. }));
}

#[test]
fn check_with_invalid_range_surfaces_invalid_range() {
    let _g = EnginesDirGuard::new();
    lay_down_active("llama-cpp", "b4001");
    let err = check_against_active_engine(&req("llama-cpp", ">="), None, None).unwrap_err();
    assert!(matches!(err, CompatError::InvalidRange { .. }));
}

fn make_minimal_model(req: Option<EngineRequirement>) -> Manifest {
    Manifest {
        name: "test-model".into(),
        version: "v1".into(),
        task: crate::model::registry::Task::Llm,
        engine: crate::model::registry::Engine::Llama,
        format: crate::model::registry::Format::Gguf,
        sha256: "0".repeat(64),
        size: 0,
        files: vec!["model.gguf".into()],
        default_device: None,
        params: serde_json::Value::Null,
        requires_engine: req,
        gguf_version: None,
        arch: None,
    }
}
