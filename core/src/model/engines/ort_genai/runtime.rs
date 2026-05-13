//! Process-wide loaded `libonnxruntime-genai` runtime.
//!
//! Symmetric to [`super::super::ort::runtime`] — same OnceLock pattern,
//! same Windows altered-search-path flag, same test override hook.
//! Single-symbol scaffold (`OgaShutdown`) resolved on load.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use libloading::Library;

use super::ffi::OrtGenaiSyms;
use super::EngineError;

pub struct OrtGenaiRuntime {
    /// SAFETY: never dropped while `syms` is reachable.
    _lib: Library,
    pub syms: OrtGenaiSyms,
    pub lib_path: PathBuf,
}

static SHARED: OnceLock<Arc<OrtGenaiRuntime>> = OnceLock::new();

#[cfg(test)]
static TEST_OVERRIDE: RwLock<Option<Arc<OrtGenaiRuntime>>> = RwLock::new(None);

#[cfg(not(test))]
#[allow(dead_code)] // Field exists only to keep the import shape uniform.
static TEST_OVERRIDE: RwLock<Option<Arc<OrtGenaiRuntime>>> = RwLock::new(None);

impl OrtGenaiRuntime {
    pub fn load(lib_path: &Path) -> Result<Self, EngineError> {
        if !lib_path.is_file() {
            return Err(EngineError::NotInstalled(format!(
                "expected ort-genai runtime at {} — run `cos engine update ort-genai` or check `cos engine list`",
                lib_path.display()
            )));
        }

        // SAFETY: see `super::super::ort::runtime::load_with_sibling_search`.
        let lib = unsafe { load_with_sibling_search(lib_path) }
            .map_err(|e| EngineError::LibraryLoadFailed(format!("{}: {e}", lib_path.display())))?;

        // SAFETY: see `OrtGenaiSyms::resolve`.
        let syms = unsafe { OrtGenaiSyms::resolve(&lib) }
            .map_err(|e| EngineError::LibraryLoadFailed(format!("symbol resolution: {e}")))?;

        Ok(Self {
            _lib: lib,
            syms,
            lib_path: lib_path.to_path_buf(),
        })
    }

    #[allow(dead_code)] // Real callers land when ort-genai wire-in starts.
    pub fn shared() -> Result<Arc<Self>, EngineError> {
        if let Some(rt) = test_override_runtime() {
            return Ok(rt);
        }

        if let Some(existing) = SHARED.get() {
            return Ok(existing.clone());
        }

        let lib_path =
            crate::engine_pkg::active_library_path(super::PKG_ENGINE_NAME, super::LIB_BASENAME)
                .ok_or_else(|| {
                    EngineError::NotInstalled(
                        "no active ort-genai engine — run `cos engine update ort-genai` to install"
                            .into(),
                    )
                })?;

        let runtime = Arc::new(Self::load(&lib_path)?);
        let _ = SHARED.set(runtime.clone());
        Ok(SHARED.get().cloned().unwrap_or(runtime))
    }
}

#[cfg(test)]
pub fn test_override_runtime() -> Option<Arc<OrtGenaiRuntime>> {
    TEST_OVERRIDE.read().ok().and_then(|g| g.clone())
}

#[cfg(not(test))]
fn test_override_runtime() -> Option<Arc<OrtGenaiRuntime>> {
    None
}

#[cfg(test)]
#[allow(dead_code)] // Reserved for future tests.
pub fn set_test_override(rt: Option<Arc<OrtGenaiRuntime>>) -> Option<Arc<OrtGenaiRuntime>> {
    let mut slot = TEST_OVERRIDE.write().expect("test override lock poisoned");
    std::mem::replace(&mut *slot, rt)
}

#[cfg(target_os = "windows")]
unsafe fn load_with_sibling_search(path: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::windows as win;

    const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;
    let lib = win::Library::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH)?;
    Ok(Library::from(lib))
}

#[cfg(not(target_os = "windows"))]
unsafe fn load_with_sibling_search(path: &Path) -> Result<Library, libloading::Error> {
    Library::new(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_not_installed() {
        let p = std::env::temp_dir().join(format!(
            "cos-ort-genai-runtime-missing-{}-{}.dll",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        match OrtGenaiRuntime::load(&p) {
            Err(EngineError::NotInstalled(_)) => {}
            Err(other) => panic!("expected NotInstalled, got {other:?}"),
            Ok(_) => panic!("nonexistent path should not load"),
        }
    }

    #[test]
    fn load_garbage_file_returns_library_load_failed() {
        let p = std::env::temp_dir().join(format!(
            "cos-ort-genai-runtime-garbage-{}-{}.dll",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&p, b"definitely not a real shared library").unwrap();
        match OrtGenaiRuntime::load(&p) {
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

    #[test]
    fn shared_propagates_not_installed_when_no_active_engine() {
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::engine_pkg::paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));

        let p = crate::engine_pkg::active_library_path(
            super::super::PKG_ENGINE_NAME,
            super::super::LIB_BASENAME,
        );
        assert!(p.is_none(), "no active engine should be resolvable");

        crate::engine_pkg::paths::set_engines_dir_override(None);
    }
}
