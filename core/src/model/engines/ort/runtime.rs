//! Process-wide loaded `libonnxruntime` runtime.
//!
//! Holds a [`libloading::Library`] plus the resolved [`OrtSyms`]
//! function-pointer table. Constructed once per process via
//! [`OrtRuntime::shared`], which caches **only successful loads** —
//! a failure to load now does NOT poison future calls so the daemon
//! can survive an `cos engine update ort` happening after startup.
//!
//! Active-version changes after a successful load are NOT picked up
//! until the process restarts. This is the documented behavior; a
//! `cos engine activate` followed by `cos service restart` swaps in
//! the new version. Mirrors `super::super::llama_cpp::runtime` exactly.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use libloading::Library;

use super::ffi::OrtSyms;
use super::EngineError;

/// One process-global loaded ort runtime. The library is kept alive
/// for the entire process lifetime; dropping it would invalidate any
/// outstanding function pointer.
pub struct OrtRuntime {
    /// SAFETY: never dropped while `syms` is reachable. Keep the field
    /// here so its lifetime is tied to `OrtRuntime`.
    _lib: Library,
    pub syms: OrtSyms,
    /// Path the library was loaded from — surfaced by status so users
    /// can confirm which version is in use.
    pub lib_path: PathBuf,
}

/// Cached successful load. Failures are NOT stored — every miss
/// re-attempts resolution + load.
static SHARED: OnceLock<Arc<OrtRuntime>> = OnceLock::new();

impl OrtRuntime {
    /// Load `libonnxruntime` from a specific file path. Used by
    /// [`shared`] after path resolution; tests can call it directly.
    ///
    /// On Windows we use `LOAD_WITH_ALTERED_SEARCH_PATH` so the loader
    /// also searches the directory containing `lib_path` for sibling
    /// DLLs (`onnxruntime_providers_shared.dll`, ...). The default
    /// behavior only searches the executable's directory + system
    /// paths, which would fail for our flat-layout engine installs.
    pub fn load(lib_path: &Path) -> Result<Self, EngineError> {
        if !lib_path.is_file() {
            return Err(EngineError::NotInstalled(format!(
                "expected ort runtime at {} — run `cos engine update ort` or check `cos engine list`",
                lib_path.display()
            )));
        }

        // SAFETY: see `super::super::llama_cpp::runtime::load_with_sibling_search`
        // — `Library::new` (Unix) / `load_with_flags` (Windows) load
        // arbitrary native code into the process. The path is sourced
        // from `engine_pkg::active_library_path` which only returns
        // paths under `<engines_dir>/<engine>/<active>/`.
        let lib = unsafe { load_with_sibling_search(lib_path) }
            .map_err(|e| EngineError::LibraryLoadFailed(format!("{}: {e}", lib_path.display())))?;

        // SAFETY: see `OrtSyms::resolve` — `lib` was just produced by
        // libloading and the symbol signature matches
        // `onnxruntime_c_api.h`'s stable entry point.
        let syms = unsafe { OrtSyms::resolve(&lib) }
            .map_err(|e| EngineError::LibraryLoadFailed(format!("symbol resolution: {e}")))?;

        Ok(Self {
            _lib: lib,
            syms,
            lib_path: lib_path.to_path_buf(),
        })
    }

    /// Process-wide singleton. The first successful load is cached;
    /// subsequent calls return the cached `Arc` cheaply. Failures do
    /// NOT populate the cache, so the user can install + activate an
    /// engine while the daemon is running and the next call will pick
    /// it up.
    #[allow(dead_code)] // Real callers land when ort wire-in starts.
    pub fn shared() -> Result<Arc<Self>, EngineError> {
        #[cfg(test)]
        {
            if let Some(rt) = tests::test_override_runtime() {
                return Ok(rt);
            }
        }

        if let Some(existing) = SHARED.get() {
            return Ok(existing.clone());
        }

        let lib_path =
            crate::engine_pkg::active_library_path(super::PKG_ENGINE_NAME, super::LIB_BASENAME)
                .ok_or_else(|| {
                    EngineError::NotInstalled(
                        "no active ort engine — run `cos engine update ort` to install".into(),
                    )
                })?;

        let runtime = Arc::new(Self::load(&lib_path)?);
        // Race tolerated: see llama_cpp::runtime::shared() for why this
        // pattern is safe.
        let _ = SHARED.set(runtime.clone());
        Ok(SHARED.get().cloned().unwrap_or(runtime))
    }
}

#[cfg(target_os = "windows")]
unsafe fn load_with_sibling_search(path: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::windows as win;

    // 0x00000008 = LOAD_WITH_ALTERED_SEARCH_PATH. With this flag set
    // the loader uses the directory containing `path` as the first
    // search location for the library's own dependent DLLs, matching
    // the llama_cpp pattern from P2.3.
    const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;
    let lib = win::Library::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH)?;
    Ok(Library::from(lib))
}

#[cfg(not(target_os = "windows"))]
unsafe fn load_with_sibling_search(path: &Path) -> Result<Library, libloading::Error> {
    // Unix: `dlopen` resolves dependent libraries via the library's
    // RPATH/RUNPATH metadata if the upstream artifact has it set.
    // Official onnxruntime linux/macos tarballs include the relevant
    // RPATH; if a future artifact doesn't, the failure surfaces as
    // `LibraryLoadFailed` and the user can either patch RPATH or set
    // LD_LIBRARY_PATH / DYLD_LIBRARY_PATH.
    Library::new(path)
}

#[cfg(test)]
pub use tests::set_test_override;

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/model/engines/ort/runtime.rs"
    ));
}
