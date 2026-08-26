use super::*;
use std::sync::RwLock;

static TEST_OVERRIDE: RwLock<Option<Arc<OrtGenaiRuntime>>> = RwLock::new(None);

pub(super) fn test_override_runtime() -> Option<Arc<OrtGenaiRuntime>> {
    TEST_OVERRIDE.read().ok().and_then(|g| g.clone())
}

#[allow(dead_code)]
pub fn set_test_override(rt: Option<Arc<OrtGenaiRuntime>>) -> Option<Arc<OrtGenaiRuntime>> {
    let mut slot = TEST_OVERRIDE.write().expect("test override lock poisoned");
    std::mem::replace(&mut *slot, rt)
}

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
