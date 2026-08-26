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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/model/engines/ort.rs"
    ));
}
