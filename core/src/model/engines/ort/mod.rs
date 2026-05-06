//! ONNX Runtime engine — runtime-loaded scaffold.
//!
//! Like [`super::llama_cpp`], the `ort` engine is no longer compile-time
//! linked. The engine package manager installs a prebuilt
//! ONNX Runtime release into
//!
//! ```text
//! <engines_dir>/ort/<version>/lib/onnxruntime.dll        (Windows)
//!                             /libonnxruntime.so         (Linux)
//!                             /libonnxruntime.dylib      (macOS)
//! ```
//!
//! and we resolve the shared library at runtime via `libloading`. The
//! user controls availability with `cos engine update ort`,
//! `cos engine activate <ver>`, etc. — see `core/src/engine_pkg/`.
//!
//! **Scaffold scope (P2.3 follow-up):** this module exposes only:
//!
//!   - [`is_installed`] — cheap availability check used by
//!     `engines_linked()` and `cos agent status`.
//!   - [`engine_version_from_lib_path`] — derived from the loaded
//!     library path so audit logs reflect what's actually executing
//!     (mirrors the llama_cpp decision in P2.4-C).
//!   - [`runtime::OrtRuntime`] — the libloading singleton. Resolves a
//!     single stable entry point (`OrtGetApiBase`) on load and keeps
//!     the [`libloading::Library`] alive for the process lifetime.
//!
//! Concrete inference (creating an OrtEnv, binding an `OrtApi` vtable,
//! running sessions) is deferred to wire-in time — when a user imports
//! their first ONNX model. At that point the FFI surface grows to
//! include `OrtApi` (with `extern "system"` calling convention to honor
//! `ORT_API_CALL = __stdcall` on Windows-x86) and a full inference task
//! integration. None of that ships in the scaffold.

use std::path::Path;

use super::EngineError;

pub mod ffi;
pub mod runtime;

/// Static identifier — used by `engines_linked()`. Same value as
/// `PKG_ENGINE_NAME` because ONNX Runtime has no kebab-vs-snake split.
pub const ENGINE_NAME: &str = "ort";

/// Engine name used by `crate::engine_pkg`.
pub const PKG_ENGINE_NAME: &str = "ort";

/// Library basename — composed by `engine_pkg::platform_library_filename`
/// into `onnxruntime.dll` (Windows), `libonnxruntime.so` (Linux),
/// `libonnxruntime.dylib` (macOS).
pub const LIB_BASENAME: &str = "onnxruntime";

/// Cheap availability check. Returns true iff the engine package
/// manager has an active version of `ort` whose shared library file
/// exists on disk. **Does not load the library.**
///
/// On Linux/macOS this also matches versioned siblings
/// (`libonnxruntime.so.1.25.1`) — see
/// `engine_pkg::active_library_path` for the fallback rules.
pub fn is_installed() -> bool {
    crate::engine_pkg::active_library_path(PKG_ENGINE_NAME, LIB_BASENAME).is_some()
}

/// Engine version derived from the **loaded** library path, NOT from
/// the engine_pkg registry. Returns `None` if the path doesn't match
/// the expected `<engines_dir>/<engine>/<version>/{lib,bin}/<lib-file>`
/// shape (e.g. test-injected path).
///
/// The decision to read from the loaded path mirrors P2.4-C for
/// `llama_cpp`: the process-wide [`runtime::OrtRuntime`] cache may hold
/// the previously-active version even after `cos engine activate <new>`
/// — the user must restart the daemon for the new version to take
/// effect. The registry would falsely report the new version; this
/// returns what's actually executing.
pub(crate) fn engine_version_from_lib_path(lib_path: &Path) -> Option<String> {
    // .../<engine>/<version>/<lib-or-bin>/<file>
    //                ^^^^^^^^^                   parent.parent.file_name
    let version_dir = lib_path.parent()?.parent()?;
    version_dir.file_name()?.to_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn engine_version_from_lib_path_unix_layout() {
        let p = PathBuf::from("/var/lib/cos/engines/ort/1.25.1/lib/libonnxruntime.so");
        assert_eq!(engine_version_from_lib_path(&p), Some("1.25.1".into()));
    }

    #[test]
    fn engine_version_from_lib_path_windows_layout() {
        let p =
            PathBuf::from(r"C:\ProgramData\cos\engines\ort\1.25.1\lib\onnxruntime.dll");
        assert_eq!(engine_version_from_lib_path(&p), Some("1.25.1".into()));
    }

    #[test]
    fn engine_version_from_lib_path_bin_dir_supported() {
        // P2.3's active_library_path falls back to bin/ for some
        // upstream layouts. Version parsing only depends on the
        // <version>/<sub>/<file> tail shape, so either subdir works.
        let p = PathBuf::from("/var/lib/cos/engines/ort/1.25.1/bin/onnxruntime.dll");
        assert_eq!(engine_version_from_lib_path(&p), Some("1.25.1".into()));
    }

    #[test]
    fn engine_version_from_lib_path_versioned_so() {
        // Linux versioned-only fallback — versioned soname instead of
        // unversioned symlink. The version parser keys off the directory
        // containing the file, not the filename, so this still resolves.
        let p =
            PathBuf::from("/var/lib/cos/engines/ort/1.25.1/lib/libonnxruntime.so.1.25.1");
        assert_eq!(engine_version_from_lib_path(&p), Some("1.25.1".into()));
    }

    #[test]
    fn engine_version_from_lib_path_too_short() {
        let too_short = PathBuf::from("/tmp/onnxruntime.dll");
        assert!(engine_version_from_lib_path(&too_short).is_none());
    }

    #[test]
    fn lib_basename_unchanged() {
        assert_eq!(LIB_BASENAME, "onnxruntime");
    }
}
