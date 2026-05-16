//! Model-subsystem path resolution.
//!
//! Re-exports system-level helpers from `crate::paths` for convenience and
//! defines model-specific helpers (per-model directory layout).

use std::path::{Path, PathBuf};

pub use crate::paths::{model_runtime_socket as socket_path, models_cache_dir, models_dir};

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
///
/// Both sides are canonicalized before comparison so a symlink
/// pointing out of the models dir (or a `..` segment) can't smuggle
/// access to an unrelated tree. If either path fails to canonicalize
/// — e.g. because `path` doesn't exist yet — we walk up to the
/// deepest existing ancestor, canonicalize that, and re-append the
/// missing tail. This keeps the guard usable for not-yet-created
/// targets (model import, version-dir creation) while still
/// resolving every existing symlink along the way.
pub fn is_within_models_dir(path: &Path) -> bool {
    let needle = canonicalize_partial(path);
    let haystack = canonicalize_partial(&models_dir());
    needle.starts_with(&haystack)
}

/// Canonicalize as much of `p` as exists, then re-append any missing
/// tail components verbatim. Returns `p` unchanged if no prefix
/// resolves (extremely rare — usually means the cwd itself is gone).
fn canonicalize_partial(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cur = p;
    while let Some(parent) = cur.parent() {
        if let Some(name) = cur.file_name() {
            tail.push(name);
        }
        if let Ok(c) = std::fs::canonicalize(parent) {
            let mut out = c;
            for seg in tail.iter().rev() {
                out.push(seg);
            }
            return out;
        }
        cur = parent;
    }
    p.to_path_buf()
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
