//! Path resolution for the engine package manager, with a test-only
//! thread-local override.
//!
//! In production these all delegate to `crate::paths::*`. In tests we
//! redirect to a per-test temp directory **without touching the
//! `COS_DATA_DIR` env var**, which would otherwise race with other
//! integration tests that read it (notably `ipc::*`).

use std::path::PathBuf;

#[cfg(test)]
thread_local! {
    static ENGINES_DIR_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_engines_dir_override(p: Option<PathBuf>) {
    ENGINES_DIR_OVERRIDE.with(|c| *c.borrow_mut() = p);
}

pub fn engines_dir() -> PathBuf {
    #[cfg(test)]
    {
        if let Some(p) = ENGINES_DIR_OVERRIDE.with(|c| c.borrow().clone()) {
            return p;
        }
    }
    crate::paths::engines_dir()
}

pub fn engine_dir(engine: &str) -> PathBuf {
    engines_dir().join(engine)
}

pub fn engine_version_dir(engine: &str, version: &str) -> PathBuf {
    engine_dir(engine).join(version)
}

pub fn engines_index_path() -> PathBuf {
    engines_dir().join("engines.json")
}
