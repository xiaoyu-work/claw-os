//! Model ↔ engine compatibility enforcement — Phase 2.4.
//!
//! A model manifest declares **requirements** about the engine that
//! must be running for it to load. The compat layer checks the active
//! engine in `<engines_dir>/engines.json` against those requirements
//! and either lets the load proceed or returns a structured error
//! pointing at the misconfiguration.
//!
//! ## Two version schemes
//!
//! Different engines version themselves differently. We dispatch by
//! engine name to the appropriate parser:
//!
//! - **`llama-cpp`**: build numbers like `b3950`, `b4001`, `b4500`. We
//!   strip the leading `b` and parse the rest as `u32`.
//! - **`ort`, `ort-genai`**: SemVer (`1.22.0`, `0.4.0-beta`). We use the
//!   `semver` crate.
//!
//! A single engine-agnostic comparator would be too weak (it would
//! compare `"b3950" < "b40000"` lexicographically as false even though
//! `3950 < 40000`). A general scheme trait would be overengineered for
//! three engines. We dispatch with a simple match.
//!
//! ## Range syntax
//!
//! Range expressions are comma-separated AND of comparators. We share
//! one parser; the per-engine comparator decides what each operand
//! means. Examples:
//!
//! - `*`                       — any version (always passes)
//! - `=b4001`                  — exact match
//! - `>=b3900`                 — minimum
//! - `>=b3900, <b4500`         — bounded range
//! - `>=1.22.0, <2.0.0`        — bounded semver range
//!
//! ## Capability checks vs version checks
//!
//! When an engine ships a `manifest.json`, we *also* check
//! `gguf_versions` and `model_archs` if the model declares them. If the
//! engine has *no* manifest (legacy install), capability checks return
//! `Unknown` rather than a silent pass — the version check still
//! enforces but the model author is asked to make capability claims
//! authoritatively.

use crate::engine_pkg::{self, manifest::EngineManifest, registry::EnginesIndex};
use crate::model::registry::{EngineRequirement, Manifest};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompatError {
    /// The model declared a `requires_engine.name` but no engine of
    /// that name has any active version in the registry.
    #[error("model requires engine \"{required}\" but no active version is installed (run `cos engine update {required}`)")]
    NoActiveEngine { required: String },

    /// An active engine exists but its name does not match what the
    /// model wants. Typically only happens if the runtime config tries
    /// to coerce a model into the wrong engine kind.
    #[error(
        "model requires engine \"{required}\" but the active engine for the runtime is \"{found}\""
    )]
    EngineMismatch { required: String, found: String },

    /// Active engine version doesn't satisfy the declared range.
    #[error("active {engine} version \"{active}\" does not satisfy required range \"{range}\": {reason}")]
    VersionOutOfRange {
        engine: String,
        active: String,
        range: String,
        reason: String,
    },

    /// Range expression is unparseable. Distinct from version-out-of-range
    /// so we can blame the model manifest rather than the host.
    #[error("model.requires_engine.version range \"{range}\" is invalid: {reason}")]
    InvalidRange { range: String, reason: String },

    /// Engine ships a manifest declaring supported GGUF versions and
    /// the model's GGUF version isn't among them.
    #[error("active {engine} version supports GGUF major versions {supported:?} but model needs GGUF v{needed}")]
    GgufVersionUnsupported {
        engine: String,
        supported: Vec<u32>,
        needed: u32,
    },

    /// Engine ships a manifest with an enumerated arch list and the
    /// model's arch isn't on it.
    #[error("active {engine} version's manifest enumerates archs {supported:?}; model arch \"{arch}\" is not listed")]
    ArchUnsupported {
        engine: String,
        supported: Vec<String>,
        arch: String,
    },

    /// Surfaces malformed manifests rather than silently treating them
    /// as missing.
    #[error("active {engine} manifest is malformed: {message}")]
    ManifestMalformed { engine: String, message: String },
}

/// Top-level compat check: does the active engine satisfy this model's
/// `requires_engine` clause?
///
/// Returns `Ok(())` if:
///   - the model declares no `requires_engine` (nothing to enforce), OR
///   - all declared requirements are satisfied.
///
/// Capability checks (gguf_versions, model_archs) are only applied
/// when the engine ships an authoritative manifest. With no manifest
/// the **version** check still enforces, but capability fields are
/// treated as advisory (they pass silently — the alternative would be
/// to refuse every load for legacy installs, which is too aggressive).
pub fn check_engine_compat(model: &Manifest) -> Result<(), CompatError> {
    let req = match &model.requires_engine {
        Some(r) => r,
        None => return Ok(()),
    };
    check_against_active_engine(req, model.gguf_version, model.arch.as_deref())
}

/// Reusable inner check that doesn't require a full Manifest — used by
/// `cos model check` before the manifest is fully assembled and by
/// tests.
pub fn check_against_active_engine(
    req: &EngineRequirement,
    gguf_version: Option<u32>,
    arch: Option<&str>,
) -> Result<(), CompatError> {
    let index = EnginesIndex::load_or_default().map_err(|e| CompatError::ManifestMalformed {
        engine: req.name.clone(),
        message: format!("engines.json: {e}"),
    })?;

    // Empty `name` would mean "any engine" but we currently demand the
    // model name an engine. Treat it as required.
    if req.name.is_empty() {
        return Err(CompatError::InvalidRange {
            range: req.version.clone(),
            reason: "requires_engine.name is empty".into(),
        });
    }

    let entry = match index.entry(&req.name) {
        Some(e) if !e.active.is_empty() => e,
        _ => {
            return Err(CompatError::NoActiveEngine {
                required: req.name.clone(),
            });
        }
    };

    let active_version = entry.active.clone();

    // 1. Version range check (always enforced).
    let range = parse_range(&req.version).map_err(|reason| CompatError::InvalidRange {
        range: req.version.clone(),
        reason,
    })?;
    if let Err(reason) = match_version(&req.name, &active_version, &range) {
        return Err(CompatError::VersionOutOfRange {
            engine: req.name.clone(),
            active: active_version,
            range: req.version.clone(),
            reason,
        });
    }

    // 2. Capability checks — only enforced when the engine has an
    //    authoritative manifest. Missing manifest → advisory pass.
    let manifest = match EngineManifest::load(&req.name, &active_version) {
        Ok(m) => m,
        Err(e) => {
            return Err(CompatError::ManifestMalformed {
                engine: req.name.clone(),
                message: e.to_string(),
            });
        }
    };
    if let Some(m) = manifest {
        if !m.is_authoritative() {
            return Ok(());
        }
        if !m.gguf_versions.is_empty() {
            if let Some(needed) = gguf_version {
                if !m.gguf_versions.contains(&needed) {
                    return Err(CompatError::GgufVersionUnsupported {
                        engine: req.name.clone(),
                        supported: m.gguf_versions.clone(),
                        needed,
                    });
                }
            }
        }
        if !m.model_archs.is_empty() {
            if let Some(a) = arch {
                if !m.model_archs.iter().any(|s| s == a) {
                    return Err(CompatError::ArchUnsupported {
                        engine: req.name.clone(),
                        supported: m.model_archs.clone(),
                        arch: a.to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------
// Range parsing — engine-agnostic at the syntax layer; the comparator
// is engine-aware.
// ------------------------------------------------------------------

/// One comparator in a range, e.g. `>=b3900` or `<2.0.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparator {
    pub op: Op,
    pub operand: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ge,
    Le,
    Gt,
    Lt,
}

/// Parsed range = AND of comparators. Empty (`*`) matches anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionRange {
    pub comparators: Vec<Comparator>,
}

/// Parse a range expression. Returns `Ok(empty range)` for `*` or whitespace-only input.
pub fn parse_range(s: &str) -> Result<VersionRange, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed == "*" {
        return Ok(VersionRange::default());
    }
    let mut comparators = Vec::new();
    for raw in trimmed.split(',') {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        comparators.push(parse_comparator(part)?);
    }
    if comparators.is_empty() {
        return Ok(VersionRange::default());
    }
    Ok(VersionRange { comparators })
}

fn parse_comparator(s: &str) -> Result<Comparator, String> {
    let (op, rest) = if let Some(r) = s.strip_prefix(">=") {
        (Op::Ge, r)
    } else if let Some(r) = s.strip_prefix("<=") {
        (Op::Le, r)
    } else if let Some(r) = s.strip_prefix('>') {
        (Op::Gt, r)
    } else if let Some(r) = s.strip_prefix('<') {
        (Op::Lt, r)
    } else if let Some(r) = s.strip_prefix('=') {
        (Op::Eq, r)
    } else {
        // Bare operand → exact match.
        (Op::Eq, s)
    };
    let operand = rest.trim();
    if operand.is_empty() {
        return Err(format!("comparator \"{s}\" has no operand"));
    }
    Ok(Comparator {
        op,
        operand: operand.to_string(),
    })
}

/// Per-engine comparator dispatch. Returns `Ok(())` if the active
/// version satisfies every comparator in the range, otherwise
/// `Err(reason)`.
pub fn match_version(engine: &str, active: &str, range: &VersionRange) -> Result<(), String> {
    if range.comparators.is_empty() {
        return Ok(());
    }
    for cmp in &range.comparators {
        let ord = compare_versions(engine, active, &cmp.operand)?;
        let satisfied = match cmp.op {
            Op::Eq => ord == std::cmp::Ordering::Equal,
            Op::Ge => ord != std::cmp::Ordering::Less,
            Op::Le => ord != std::cmp::Ordering::Greater,
            Op::Gt => ord == std::cmp::Ordering::Greater,
            Op::Lt => ord == std::cmp::Ordering::Less,
        };
        if !satisfied {
            return Err(format!("fails comparator {:?} {}", cmp.op, cmp.operand));
        }
    }
    Ok(())
}

/// Engine-aware version compare. Returns `active.cmp(operand)` under
/// the engine's version scheme.
pub fn compare_versions(
    engine: &str,
    active: &str,
    operand: &str,
) -> Result<std::cmp::Ordering, String> {
    match version_scheme(engine) {
        VersionScheme::LlamaBuild => compare_llama_build(active, operand),
        VersionScheme::Semver => compare_semver(active, operand),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionScheme {
    /// llama.cpp's `bNNNN` build numbers.
    LlamaBuild,
    /// SemVer (`1.22.0`, `0.4.0-beta`).
    Semver,
}

pub fn version_scheme(engine: &str) -> VersionScheme {
    match engine {
        "llama-cpp" => VersionScheme::LlamaBuild,
        "ort" | "ort-genai" => VersionScheme::Semver,
        // Unknown engines default to LlamaBuild because that's the
        // strictest reasonable fallback for u32-style version strings;
        // semver would reject any non-semver input outright. Compat
        // tests cover this fallback explicitly.
        _ => VersionScheme::LlamaBuild,
    }
}

fn compare_llama_build(active: &str, operand: &str) -> Result<std::cmp::Ordering, String> {
    let a = parse_llama_build(active)
        .ok_or_else(|| format!("active version \"{active}\" is not a llama.cpp build number"))?;
    let b = parse_llama_build(operand)
        .ok_or_else(|| format!("range operand \"{operand}\" is not a llama.cpp build number"))?;
    Ok(a.cmp(&b))
}

/// `b4001` → `Some(4001)`, `4001` → `Some(4001)` (lenient — operands without `b` accepted),
/// `latest` → `None`.
fn parse_llama_build(s: &str) -> Option<u32> {
    let stripped = s.strip_prefix('b').unwrap_or(s);
    stripped.parse::<u32>().ok()
}

fn compare_semver(active: &str, operand: &str) -> Result<std::cmp::Ordering, String> {
    let a = semver::Version::parse(active)
        .map_err(|e| format!("active version \"{active}\" is not semver: {e}"))?;
    let b = semver::Version::parse(operand)
        .map_err(|e| format!("range operand \"{operand}\" is not semver: {e}"))?;
    Ok(a.cmp(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
