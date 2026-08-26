use super::*;
use std::sync::RwLock;

static TEST_OVERRIDE: RwLock<Option<Arc<LlamaRuntime>>> = RwLock::new(None);

pub(super) fn test_override_runtime() -> Option<Arc<LlamaRuntime>> {
    TEST_OVERRIDE.read().ok().and_then(|g| g.clone())
}

pub fn set_test_override(rt: Option<Arc<LlamaRuntime>>) -> Option<Arc<LlamaRuntime>> {
    let mut slot = TEST_OVERRIDE.write().expect("test override lock poisoned");
    std::mem::replace(&mut *slot, rt)
}

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
