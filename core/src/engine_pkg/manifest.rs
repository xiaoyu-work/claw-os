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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/engine_pkg/manifest.rs"
    ));
}
