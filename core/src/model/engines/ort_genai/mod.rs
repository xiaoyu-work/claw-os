//! ONNX Runtime GenAI (`onnxruntime-genai`) engine — runtime-loaded scaffold.
//!
//! Companion to [`super::ort`]. Whereas onnxruntime is the inference
//! kernel, onnxruntime-genai layers a generation-specific API on top
//! (KV cache management, decoder loop, sampling) and is the runtime we
//! intend to use for any local LLM-shaped ONNX model in the future.
//!
//! Same scaffold scope as [`super::ort`]:
//!
//!   - [`is_installed`] — cheap availability check.
//!   - [`engine_version_from_lib_path`] — version derived from loaded
//!     library path.
//!   - [`runtime::OrtGenaiRuntime`] — libloading singleton resolving a
//!     single stable export (`OgaShutdown`) as the "library is intact"
//!     probe. Concrete API binding (`OgaCreateModel`, `OgaGenerator_*`,
//!     etc.) is deferred to wire-in time.

use std::path::Path;

use super::EngineError;

pub mod ffi;
pub mod runtime;
pub mod safe;

/// FFI/agent-side identifier with underscore. Mirrors the
/// `llama_cpp` / `llama-cpp` convention.
pub const ENGINE_NAME: &str = "ort_genai";

/// Engine name used by `crate::engine_pkg` (kebab-case, matches
/// `microsoft/onnxruntime-genai`).
pub const PKG_ENGINE_NAME: &str = "ort-genai";

/// Library basename — composed by `engine_pkg::platform_library_filename`
/// into `onnxruntime-genai.dll` (Windows), `libonnxruntime-genai.so`
/// (Linux), `libonnxruntime-genai.dylib` (macOS).
pub const LIB_BASENAME: &str = "onnxruntime-genai";

/// Cheap availability check. Returns true iff the engine package
/// manager has an active version of `ort-genai` whose shared library
/// file exists on disk. **Does not load the library.**
pub fn is_installed() -> bool {
    crate::engine_pkg::active_library_path(PKG_ENGINE_NAME, LIB_BASENAME).is_some()
}

/// Engine version derived from the **loaded** library path. See
/// `super::ort::engine_version_from_lib_path` for rationale.
pub(crate) fn engine_version_from_lib_path(lib_path: &Path) -> Option<String> {
    let version_dir = lib_path.parent()?.parent()?;
    version_dir.file_name()?.to_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn engine_version_from_lib_path_unix_layout() {
        let p = PathBuf::from("/var/lib/cos/engines/ort-genai/0.13.1/lib/libonnxruntime-genai.so");
        assert_eq!(engine_version_from_lib_path(&p), Some("0.13.1".into()));
    }

    // Windows-layout test removed; see
    // `crate::model::engines::ort::tests` for the rationale.

    #[test]
    fn engine_version_from_lib_path_too_short() {
        let too_short = PathBuf::from("/tmp/onnxruntime-genai.dll");
        assert!(engine_version_from_lib_path(&too_short).is_none());
    }

    #[test]
    fn lib_basename_unchanged() {
        assert_eq!(LIB_BASENAME, "onnxruntime-genai");
    }

    #[test]
    fn pkg_engine_name_is_kebab() {
        assert_eq!(PKG_ENGINE_NAME, "ort-genai");
    }
}
