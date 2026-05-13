//! Per-version engine package manifest — `<engines_dir>/<engine>/<version>/manifest.json`.
//!
//! Phase 2.4.
//!
//! Each installed engine version may ship a `manifest.json` describing its
//! ABI (which OS/arch/accelerator it was built for), what GGUF major
//! versions it understands, the basename of its main library, and any
//! free-form build metadata (e.g. `cuda_version: "12.4"`). The compat
//! layer (`crate::model::compat`) consults these to decide whether a
//! given model can run against the active engine.
//!
//! ## Provenance
//!
//! - **Authoritative manifest**: shipped inside the installed payload
//!   (the upstream zip / package vendor wrote it, or a follow-up
//!   curation step). Trustable for capability claims.
//! - **Missing**: legacy install or upstream that doesn't yet ship one.
//!   The compat layer treats `gguf_versions` / `model_archs` as
//!   *unknown* (not a pass), but can still enforce engine-name and
//!   version range based on the registry directory name.
//! - **Synthesized**: produced in memory (never written) by
//!   [`EngineManifest::synthesize`] for UI views like `cos engine info`,
//!   to give users *something* to look at when no manifest is shipped.
//!   Carries `source: "synthesized"` so consumers know it's a guess.
//!
//! ## Why we *don't* auto-write a synthesized manifest at install time
//!
//! Persisting a synthesized manifest would erase the missing-vs-shipped
//! distinction — capability fields would always look authoritative even
//! when guessed. Compat decisions silently degrade. We always preserve
//! the fact that an install came without a real manifest.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineManifest {
    /// Schema evolution marker. We start at 1.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// The engine name (e.g. `"llama-cpp"`, `"ort"`, `"ort-genai"`).
    pub engine: String,

    /// The version this manifest describes (e.g. `"b4001"`, `"1.22.0"`).
    pub version: String,

    /// Compact ABI tag: `<os>-<arch>-<accelerator>` style. Examples:
    /// `"win-x64-cuda-12"`, `"linux-x64-cpu"`, `"darwin-arm64-metal"`.
    /// Empty string if not known.
    #[serde(default)]
    pub abi_tag: String,

    /// Free-form build metadata (`cuda_version`, `compiler`, etc).
    /// Loose so upstream packagers can stuff anything useful in.
    #[serde(default)]
    pub build_meta: BTreeMap<String, String>,

    /// GGUF major versions this engine accepts. Empty = unknown
    /// (compat treats as advisory; do not silently pass).
    #[serde(default)]
    pub gguf_versions: Vec<u32>,

    /// Model architectures this engine claims to handle (e.g.
    /// `["llama","mistral","qwen2"]`). Empty = no enumeration; treated
    /// as advisory (engine likely supports anything its ggml backend
    /// covers).
    #[serde(default)]
    pub model_archs: Vec<String>,

    /// Stem of the engine's main shared library, fed through
    /// [`crate::engine_pkg::platform_library_filename`] to resolve the
    /// platform-specific filename. Default `"llama"` → `llama.dll` /
    /// `libllama.so` / `libllama.dylib`.
    #[serde(default = "default_library_basename")]
    pub library_basename: String,

    /// Provenance marker. `"shipped"` (read from disk),
    /// `"synthesized"` (in-memory fallback), or empty string for
    /// shipped manifests that don't set it (legacy default).
    #[serde(default)]
    pub source: String,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

fn default_library_basename() -> String {
    "llama".to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// The manifest file was present but could not be parsed. Distinct
    /// from "no manifest" so callers can surface a packaging error
    /// instead of silently treating the engine as unannotated.
    #[error("manifest at {path} is malformed: {message}")]
    Malformed { path: PathBuf, message: String },

    /// Schema version on disk is newer than this binary supports.
    #[error(
        "manifest at {path} has unsupported schema_version {got} (max supported: {supported})"
    )]
    UnsupportedSchema {
        path: PathBuf,
        got: u32,
        supported: u32,
    },
}

impl EngineManifest {
    pub fn manifest_path(engine: &str, version: &str) -> PathBuf {
        super::paths::engine_version_dir(engine, version).join("manifest.json")
    }

    /// Load the manifest for an installed engine version.
    ///
    /// Returns:
    /// - `Ok(Some(manifest))` if a valid manifest is on disk.
    /// - `Ok(None)` if no manifest file exists (legacy install).
    /// - `Err(...)` if the file is present but malformed or has an
    ///   unsupported schema version.
    pub fn load(engine: &str, version: &str) -> Result<Option<Self>, ManifestError> {
        let p = Self::manifest_path(engine, version);
        if !p.is_file() {
            return Ok(None);
        }
        let bytes = std::fs::read(&p)?;
        let mut m: EngineManifest =
            serde_json::from_slice(&bytes).map_err(|e| ManifestError::Malformed {
                path: p.clone(),
                message: e.to_string(),
            })?;
        if m.schema_version > SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchema {
                path: p,
                got: m.schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        if m.source.is_empty() {
            m.source = "shipped".to_string();
        }
        Ok(Some(m))
    }

    /// Write the manifest to disk. Used by tools / curators that want
    /// to add a manifest to a previously-unannotated install. The
    /// install pipeline itself does NOT call this — see module docs.
    pub fn save(&self, engine: &str, version: &str) -> Result<(), ManifestError> {
        let p = Self::manifest_path(engine, version);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| ManifestError::Malformed {
            path: p.clone(),
            message: e.to_string(),
        })?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }

    /// Synthesize a best-effort manifest for an install that didn't
    /// ship one. The result carries `source: "synthesized"` so the
    /// compat layer can distinguish guessed-capability claims from
    /// authoritative ones. **Never persists.**
    ///
    /// ABI tag is derived from `cfg!()` of the host (the engine binary
    /// is assumed to match the host that runs it; an engine zip
    /// downloaded for the wrong OS would already have failed to load).
    pub fn synthesize(engine: &str, version: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            engine: engine.to_string(),
            version: version.to_string(),
            abi_tag: detect_host_abi_tag(),
            build_meta: BTreeMap::new(),
            // Empty = "unknown / no claim". The compat layer treats
            // these as advisory.
            gguf_versions: Vec::new(),
            model_archs: Vec::new(),
            library_basename: default_library_basename_for(engine),
            source: "synthesized".to_string(),
        }
    }

    /// True if this manifest was loaded from disk (ships authoritative
    /// capability claims). False if synthesized in memory.
    pub fn is_authoritative(&self) -> bool {
        self.source != "synthesized"
    }
}

/// `"llama-cpp"` → `"llama"`, `"ort"` → `"onnxruntime"`, etc. Used by
/// [`EngineManifest::synthesize`] when no shipped manifest declares it.
pub fn default_library_basename_for(engine: &str) -> String {
    match engine {
        "llama-cpp" => "llama".to_string(),
        "ort" => "onnxruntime".to_string(),
        "ort-genai" => "onnxruntime-genai".to_string(),
        _ => "lib".to_string(),
    }
}

/// Best-effort host triple → ABI tag. Used only by
/// [`EngineManifest::synthesize`].
pub fn detect_host_abi_tag() -> String {
    let os = if cfg!(target_os = "windows") {
        "win"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "unknown"
    };
    format!("{os}-{arch}-cpu")
}

#[cfg(test)]
mod tests {
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
}
