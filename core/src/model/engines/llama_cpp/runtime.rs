//! Process-wide loaded `libllama` runtime.
//!
//! Holds a [`libloading::Library`] plus the resolved [`LlamaSyms`]
//! function-pointer table. Constructed once per process via
//! [`LlamaRuntime::shared`], which caches **only successful loads** —
//! a failure to load now does NOT poison future calls so the daemon can
//! survive an `cos engine install llama-cpp` happening after startup.
//!
//! Active-version changes after a successful load are NOT picked up
//! until the process restarts. This is the documented behavior; a
//! `cos engine activate` followed by `cos service restart` swaps in
//! the new version.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use libloading::Library;

use super::ffi::LlamaSyms;
use super::EngineError;

/// One process-global loaded llama runtime. The library is kept alive
/// for the entire process lifetime; dropping it would invalidate any
/// outstanding function pointer.
pub struct LlamaRuntime {
    /// SAFETY: never dropped while `syms` is reachable. Keep the field
    /// here so its lifetime is tied to `LlamaRuntime`.
    _lib: Library,
    pub syms: LlamaSyms,
    /// Path the library was loaded from — surfaced by status so users
    /// can confirm which version is in use.
    pub lib_path: PathBuf,
}

/// Cached successful load. Failures are NOT stored — every miss
/// re-attempts resolution + load.
static SHARED: OnceLock<Arc<LlamaRuntime>> = OnceLock::new();

/// Test-only override: lets tests inject a pre-built runtime (or pin
/// resolution to a specific path) without poisoning the process-wide
/// `OnceLock`.
#[cfg(test)]
static TEST_OVERRIDE: RwLock<Option<Arc<LlamaRuntime>>> = RwLock::new(None);

#[cfg(not(test))]
#[allow(dead_code)] // Field exists only to keep the import shape uniform.
static TEST_OVERRIDE: RwLock<Option<Arc<LlamaRuntime>>> = RwLock::new(None);

impl LlamaRuntime {
    /// Load `libllama` from a specific file path. Used by [`shared`]
    /// after path resolution; tests can call it directly.
    ///
    /// On Windows we use `LOAD_WITH_ALTERED_SEARCH_PATH` so the loader
    /// also searches the directory containing `lib_path` for sibling
    /// DLLs (`ggml.dll`, `ggml-cpu.dll`, ...). The default behavior
    /// only searches the executable's directory + system paths, which
    /// would fail for our flat-layout engine installs.
    pub fn load(lib_path: &Path) -> Result<Self, EngineError> {
        if !lib_path.is_file() {
            return Err(EngineError::NotInstalled(format!(
                "expected llama runtime at {} — run `cos engine install llama-cpp` or check `cos engine list`",
                lib_path.display()
            )));
        }

        // SAFETY: `Library::new` (Unix) and `Library::load_with_flags`
        // (Windows) load arbitrary native code into the process. We
        // accept that risk because the only entry point that resolves
        // a path is `engine_pkg::active_library_path`, which only
        // returns paths under `<engines_dir>/<engine>/<active>/`.
        let lib = unsafe { load_with_sibling_search(lib_path) }
            .map_err(|e| EngineError::LibraryLoadFailed(format!("{}: {e}", lib_path.display())))?;

        // SAFETY: see `LlamaSyms::resolve` — `lib` was just produced by
        // libloading and the symbol signatures match llama.h's stable
        // global-lifecycle ABI.
        let syms = unsafe { LlamaSyms::resolve(&lib) }
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
    pub fn shared() -> Result<Arc<Self>, EngineError> {
        // Test override wins over the real cache so tests can pin the
        // runtime without race conditions across threads.
        if let Some(rt) = test_override_runtime() {
            return Ok(rt);
        }

        if let Some(existing) = SHARED.get() {
            return Ok(existing.clone());
        }

        let lib_path = crate::engine_pkg::active_library_path("llama-cpp", "llama")
            .ok_or_else(|| {
                EngineError::NotInstalled(
                    "no active llama-cpp engine — run `cos engine update llama-cpp` to install"
                        .into(),
                )
            })?;

        let runtime = Arc::new(Self::load(&lib_path)?);
        // Race tolerated: if two threads load concurrently the first
        // OnceLock::set wins, the second returns its own Arc that gets
        // dropped. Both threads then converge on `SHARED.get()` for
        // future calls.
        let _ = SHARED.set(runtime.clone());
        Ok(SHARED.get().cloned().unwrap_or(runtime))
    }
}

#[cfg(test)]
pub fn test_override_runtime() -> Option<Arc<LlamaRuntime>> {
    TEST_OVERRIDE.read().ok().and_then(|g| g.clone())
}

#[cfg(not(test))]
fn test_override_runtime() -> Option<Arc<LlamaRuntime>> {
    None
}

/// Test-only: install (or clear) a pre-built runtime that overrides the
/// `OnceLock` cache. Returns the previous override, if any.
#[cfg(test)]
#[allow(dead_code)] // Reserved for future tests that want to mock.
pub fn set_test_override(rt: Option<Arc<LlamaRuntime>>) -> Option<Arc<LlamaRuntime>> {
    let mut slot = TEST_OVERRIDE
        .write()
        .expect("test override lock poisoned");
    std::mem::replace(&mut *slot, rt)
}

#[cfg(target_os = "windows")]
unsafe fn load_with_sibling_search(path: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::windows as win;

    // 0x00000008 = LOAD_WITH_ALTERED_SEARCH_PATH. With this flag set
    // the loader uses the directory containing `path` as the first
    // search location for the library's own dependent DLLs, which is
    // exactly what we need so `llama.dll` finds `ggml.dll` etc. that
    // sit beside it in the engine install.
    const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;
    let lib = win::Library::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH)?;
    Ok(Library::from(lib))
}

#[cfg(not(target_os = "windows"))]
unsafe fn load_with_sibling_search(path: &Path) -> Result<Library, libloading::Error> {
    // Unix: `dlopen` already resolves dependent libraries via the
    // library's RPATH/RUNPATH metadata if the upstream artifact has it
    // set. The official llama.cpp linux/macos releases do; if a future
    // artifact doesn't, the failure surfaces as `LibraryLoadFailed`
    // and the user can either patch RPATH or set LD_LIBRARY_PATH /
    // DYLD_LIBRARY_PATH. Documented in the engine_pkg readme.
    Library::new(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `load()` against a path that doesn't exist returns the explicit
    /// `NotInstalled` variant so `cos agent status` can give a useful
    /// hint instead of a libloading-level OS error.
    #[test]
    fn load_missing_file_returns_not_installed() {
        let p = std::env::temp_dir().join(format!(
            "cos-llama-runtime-missing-{}-{}.dll",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        match LlamaRuntime::load(&p) {
            Err(EngineError::NotInstalled(_)) => {}
            Err(other) => panic!("expected NotInstalled, got {other:?}"),
            Ok(_) => panic!("nonexistent path should not load"),
        }
    }

    /// Loading an empty/garbage file fails with `LibraryLoadFailed`,
    /// not `NotInstalled` — distinguishes "not yet installed" from
    /// "installed but broken".
    #[test]
    fn load_garbage_file_returns_library_load_failed() {
        let p = std::env::temp_dir().join(format!(
            "cos-llama-runtime-garbage-{}-{}.dll",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&p, b"definitely not a real shared library").unwrap();
        match LlamaRuntime::load(&p) {
            Err(EngineError::LibraryLoadFailed(_)) => {}
            Err(other) => {
                let _ = std::fs::remove_file(&p);
                panic!("expected LibraryLoadFailed, got {other:?}");
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&p);
                panic!("garbage file should not load");
            }
        }
        let _ = std::fs::remove_file(&p);
    }

    /// `shared()` propagates `NotInstalled` when no engine version is
    /// active. Uses the engine_pkg test override so it doesn't read
    /// the real `<data_dir>/engines/`.
    #[test]
    fn shared_propagates_not_installed_when_no_active_engine() {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::engine_pkg::paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));

        // We can't safely test `LlamaRuntime::shared()` here because
        // it caches into a process-global `OnceLock` that other tests
        // also touch. Instead exercise the resolution path that
        // `shared()` delegates to:
        let p = crate::engine_pkg::active_library_path("llama-cpp", "llama");
        assert!(p.is_none(), "no active engine should be resolvable");

        crate::engine_pkg::paths::set_engines_dir_override(None);
    }
}
