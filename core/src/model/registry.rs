//! Model registry — read manifest.json files under `<models>/<name>/<version>/`.
//!
//! Phase 0.5: list-only stub. Add/remove/import implementations land alongside
//! the engines.

use serde::{Deserialize, Serialize};
use std::fs;

use super::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Task {
    Llm,
    Stt,
    Tts,
    Embed,
    Vision,
    Imagegen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Ort,
    Llama,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Onnx,
    Gguf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub task: Task,
    pub engine: Engine,
    pub format: Format,
    pub sha256: String,
    pub size: u64,
    pub files: Vec<String>,
    #[serde(default)]
    pub default_device: Option<String>,
    #[serde(default)]
    pub params: serde_json::Value,
    /// P2.4: declarative engine compatibility. The active engine in
    /// `<engines_dir>/engines.json` must satisfy this requirement for
    /// the model to load. `None` means "any engine of the matching
    /// `engine` kind" — the `Engine` enum field above is the loose
    /// declaration, `requires_engine` is the precise one.
    #[serde(default)]
    pub requires_engine: Option<EngineRequirement>,
    /// P2.4: GGUF major version (e.g. 3 for GGUFv3). Used by
    /// [`crate::model::compat::check_engine_compat`] when the active
    /// engine ships an authoritative `gguf_versions` list. `None` =
    /// model author didn't declare; capability check is skipped.
    #[serde(default)]
    pub gguf_version: Option<u32>,
    /// P2.4: model architecture identifier (e.g. `"llama"`, `"qwen2"`,
    /// `"mistral"`). Used by [`crate::model::compat::check_engine_compat`]
    /// when the active engine ships an authoritative `model_archs`
    /// list. `None` = model author didn't declare; check skipped.
    #[serde(default)]
    pub arch: Option<String>,
}

/// P2.4: declarative engine compatibility clause embedded in a model
/// manifest. See [`crate::model::compat`] for range syntax and matching
/// rules.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineRequirement {
    /// Engine name as it appears in `engine_pkg::KNOWN_ENGINES` (e.g.
    /// `"llama-cpp"`, `"ort"`, `"ort-genai"`).
    pub name: String,
    /// Range expression. See [`crate::model::compat::parse_range`] for
    /// syntax. Examples: `"*"`, `">=b3900"`, `">=b3900,<b4500"`,
    /// `">=1.22.0,<2.0.0"`.
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest parse: {0}")]
    Parse(#[from] serde_json::Error),
}

/// List every registered (name, version, manifest).
pub fn list() -> Result<Vec<Manifest>, RegistryError> {
    let root = paths::models_dir();
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for name_entry in entries.flatten() {
        let name_path = name_entry.path();
        if !name_path.is_dir() {
            continue;
        }
        let Ok(versions) = fs::read_dir(&name_path) else {
            continue;
        };
        for ver_entry in versions.flatten() {
            let ver_path = ver_entry.path();
            if !ver_path.is_dir() {
                continue;
            }
            let manifest_path = ver_path.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            let bytes = fs::read(&manifest_path)?;
            let manifest: Manifest = serde_json::from_slice(&bytes)?;
            out.push(manifest);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_handles_missing_root_dir() {
        // Best-effort: should not panic if /var/lib/cos/models doesn't exist.
        let _ = list();
    }
}
