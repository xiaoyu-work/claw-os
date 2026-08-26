//! Path resolution for the engine package manager, with a test-only
//! thread-local override.
//!
//! In production these all delegate to `crate::paths::*`. In tests we
//! redirect to a per-test temp directory **without touching the
//! `COS_DATA_DIR` env var**, which would otherwise race with other
//! integration tests that read it (notably `ipc::*`).

use std::path::PathBuf;

pub fn engines_dir() -> PathBuf {
    #[cfg(test)]
    {
        if let Some(p) = tests::engines_dir_override() {
            return p;
        }
    }
    crate::paths::engines_dir()
}

pub fn engine_dir(engine: &str) -> PathBuf {
    engines_dir().join(sanitize_segment(engine))
}

pub fn engine_version_dir(engine: &str, version: &str) -> PathBuf {
    engine_dir(engine).join(sanitize_segment(version))
}

pub fn engines_index_path() -> PathBuf {
    engines_dir().join("engines.json")
}

/// Validate that an engine name or version is safe to splice into a
/// filesystem path. Allowed characters are ASCII alphanumerics plus
/// `._-`; anything else (including `/`, `\`, `..`, NUL, or
/// whitespace) is replaced with `_`. We *don't* outright reject —
/// the path API contract here is infallible — but we sanitize hard
/// enough that no caller can escape `engines_dir()` through clever
/// version strings. Engine/version names come from `engines.json`,
/// which is itself populated from `cos engine update` flags and
/// GitHub release tags; either source could host a hostile value.
fn sanitize_segment(s: &str) -> String {
    if s.is_empty() || s == "." || s == ".." {
        return "_invalid_".to_string();
    }
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned == "." || cleaned == ".." || cleaned.starts_with("..") {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

#[cfg(test)]
pub use tests::set_engines_dir_override;

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/engine_pkg/paths.rs"
    ));
}
