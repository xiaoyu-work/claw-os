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
mod tests {
    use super::*;

    #[test]
    fn sanitize_blocks_path_traversal() {
        // `../etc` ⇒ `/` becomes `_`, giving `.._etc`. Because that
        // still *starts with* `..`, we prefix `_` so the resulting
        // segment cannot be interpreted as a parent reference.
        assert_eq!(sanitize_segment("../etc"), "_.._etc");
        assert_eq!(sanitize_segment(".."), "_invalid_");
        assert_eq!(sanitize_segment("."), "_invalid_");
        assert_eq!(sanitize_segment(""), "_invalid_");
        assert_eq!(sanitize_segment("a/b"), "a_b");
        assert_eq!(sanitize_segment("a\\b"), "a_b");
        assert_eq!(sanitize_segment("a b"), "a_b");
    }

    #[test]
    fn sanitize_preserves_normal_names() {
        assert_eq!(sanitize_segment("llama-cpp"), "llama-cpp");
        assert_eq!(sanitize_segment("b4001"), "b4001");
        assert_eq!(sanitize_segment("1.2.3"), "1.2.3");
        assert_eq!(sanitize_segment("Ort_GenAI"), "Ort_GenAI");
    }
}
