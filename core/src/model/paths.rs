//! Model-subsystem path resolution.
//!
//! Re-exports system-level helpers from `crate::paths` for convenience and
//! defines model-specific helpers (per-model directory layout).

use std::path::{Path, PathBuf};

pub use crate::paths::{models_cache_dir, models_dir, model_runtime_socket as socket_path};

/// Directory holding all versions of a single model: `<models>/<name>/`.
pub fn model_dir(name: &str) -> PathBuf {
    models_dir().join(name)
}

/// Directory holding a specific version of a model: `<models>/<name>/<version>/`.
pub fn model_version_dir(name: &str, version: &str) -> PathBuf {
    model_dir(name).join(version)
}

/// Manifest file for a specific version: `<models>/<name>/<version>/manifest.json`.
pub fn manifest_path(name: &str, version: &str) -> PathBuf {
    model_version_dir(name, version).join("manifest.json")
}

/// Cache (KV cache, tokenizer cache, etc.) for a model: `<cache>/<name>/`.
pub fn model_cache_dir(name: &str) -> PathBuf {
    models_cache_dir().join(name)
}

/// True iff `path` lies within the configured models dir (sandbox guard).
pub fn is_within_models_dir(path: &Path) -> bool {
    path.starts_with(models_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_paths_compose() {
        let v = model_version_dir("whisper-small", "v1");
        assert!(v.ends_with("whisper-small/v1") || v.ends_with("whisper-small\\v1"));
        let m = manifest_path("whisper-small", "v1");
        assert!(m.ends_with("manifest.json"));
    }

    #[test]
    fn within_models_dir_check() {
        let m = models_dir().join("foo").join("v1");
        assert!(is_within_models_dir(&m));
        assert!(!is_within_models_dir(Path::new("/etc/passwd")));
    }
}
