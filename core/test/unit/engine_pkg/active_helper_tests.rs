// Tests for the P2.3 helpers `active_engine_root` and
// `active_library_path`. The engine layer (model::engines::*)
// is the primary consumer; we duplicate coverage here to catch
// regressions in the resolution rules early.

// Tests for the P2.3 helpers `active_engine_root` and
// `active_library_path`. The engine layer (model::engines::*)
// is the primary consumer; we duplicate coverage here to catch
// regressions in the resolution rules early.

use super::*;

fn write_index(engines_dir: &std::path::Path, engine: &str, active: &str) {
    let json = serde_json::json!({
        "version": 1,
        "engines": {
            engine: {
                "active": active,
                "previous": "",
                "installed": [{"version": active, "installed_at": "2026-01-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""}],
                "pinned": false,
                "channel": "release",
                "accelerator": "",
                "source": ""
            }
        }
    });
    std::fs::write(
        engines_dir.join("engines.json"),
        serde_json::to_vec_pretty(&json).unwrap(),
    )
    .unwrap();
}

#[test]
fn platform_library_filename_matches_target_os() {
    let f = platform_library_filename("llama");
    if cfg!(target_os = "windows") {
        assert_eq!(f, "llama.dll");
    } else if cfg!(target_os = "macos") {
        assert_eq!(f, "libllama.dylib");
    } else {
        assert_eq!(f, "libllama.so");
    }
}

#[test]
fn active_engine_root_unknown_engine_returns_none() {
    assert!(active_engine_root("not-a-real-engine").is_none());
}

#[test]
fn active_engine_root_no_index_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    assert!(active_engine_root("llama-cpp").is_none());
    paths::set_engines_dir_override(None);
}

#[test]
fn active_engine_root_empty_active_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    // Index exists but `active` is empty string.
    let json = serde_json::json!({
        "version": 1,
        "engines": {
            "llama-cpp": {
                "active": "",
                "previous": "",
                "installed": [],
                "pinned": false,
                "channel": "release",
                "accelerator": "",
                "source": ""
            }
        }
    });
    std::fs::write(
        tmp.path().join("engines.json"),
        serde_json::to_vec_pretty(&json).unwrap(),
    )
    .unwrap();
    assert!(active_engine_root("llama-cpp").is_none());
    paths::set_engines_dir_override(None);
}

#[test]
fn active_engine_root_directory_must_exist() {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    // Registry says active = "v0" but no directory on disk.
    write_index(tmp.path(), "llama-cpp", "v0");
    assert!(active_engine_root("llama-cpp").is_none());
    paths::set_engines_dir_override(None);
}

#[test]
fn active_engine_root_returns_path_when_present() {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    std::fs::create_dir_all(tmp.path().join("llama-cpp/v0/lib")).unwrap();
    write_index(tmp.path(), "llama-cpp", "v0");
    let p = active_engine_root("llama-cpp").expect("should resolve");
    assert!(p.ends_with("llama-cpp/v0") || p.ends_with("llama-cpp\\v0"));
    paths::set_engines_dir_override(None);
}

#[test]
fn active_library_path_prefers_lib_over_bin() {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    let lib_name = platform_library_filename("llama");
    let lib_dir = tmp.path().join("llama-cpp/v0/lib");
    let bin_dir = tmp.path().join("llama-cpp/v0/bin");
    std::fs::create_dir_all(&lib_dir).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::write(lib_dir.join(&lib_name), b"x").unwrap();
    std::fs::write(bin_dir.join(&lib_name), b"y").unwrap();
    write_index(tmp.path(), "llama-cpp", "v0");
    let p = active_library_path("llama-cpp", "llama").expect("should resolve");
    assert!(p.to_string_lossy().contains("lib"));
    assert!(!p.to_string_lossy().contains("bin"));
    paths::set_engines_dir_override(None);
}

#[test]
fn active_library_path_falls_back_to_bin() {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    let lib_name = platform_library_filename("llama");
    let bin_dir = tmp.path().join("llama-cpp/v0/bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::write(bin_dir.join(&lib_name), b"y").unwrap();
    // No lib/ directory at all.
    write_index(tmp.path(), "llama-cpp", "v0");
    let p = active_library_path("llama-cpp", "llama").expect("should resolve via bin/");
    assert!(p.to_string_lossy().contains("bin"));
    paths::set_engines_dir_override(None);
}

#[test]
fn active_library_path_none_when_file_missing() {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    std::fs::create_dir_all(tmp.path().join("llama-cpp/v0/lib")).unwrap();
    write_index(tmp.path(), "llama-cpp", "v0");
    // Directories exist but no library file.
    assert!(active_library_path("llama-cpp", "llama").is_none());
    paths::set_engines_dir_override(None);
}

#[test]
fn parse_versioned_library_name_linux() {
    // The function is gated on cfg(target_os) at call sites, but the
    // parse helper itself runs on every host — the cfg switch only
    // chooses which suffix to strip. So on Windows we still verify
    // the macOS branch via cfg.
    if cfg!(target_os = "windows") {
        // On Windows the parser uses the macOS branch only when
        // target_os = "macos", so the linux branch is what runs.
        // Just confirm the unversioned + non-matching cases return None.
        assert_eq!(parse_versioned_library_name("llama.dll", "llama"), None);
        return;
    }
    if cfg!(target_os = "macos") {
        assert_eq!(
            parse_versioned_library_name("libonnxruntime.1.25.1.dylib", "onnxruntime"),
            Some("1.25.1".into())
        );
        // Unversioned .dylib is rejected (caller's exact-match path
        // wins).
        assert_eq!(
            parse_versioned_library_name("libonnxruntime.dylib", "onnxruntime"),
            None
        );
    } else {
        assert_eq!(
            parse_versioned_library_name("libonnxruntime.so.1.25.1", "onnxruntime"),
            Some("1.25.1".into())
        );
        assert_eq!(
            parse_versioned_library_name("libonnxruntime.so.1", "onnxruntime"),
            Some("1".into())
        );
        // Unversioned .so is rejected.
        assert_eq!(
            parse_versioned_library_name("libonnxruntime.so", "onnxruntime"),
            None
        );
        // Wrong basename rejected.
        assert_eq!(
            parse_versioned_library_name("libllama.so.1.0", "onnxruntime"),
            None
        );
        // Non-numeric reject.
        assert_eq!(
            parse_versioned_library_name("libonnxruntime.so.dev", "onnxruntime"),
            None
        );
        // Leading-dot reject (".." after strip_prefix would have empty digit lead).
        assert_eq!(
            parse_versioned_library_name("libonnxruntime.so..1", "onnxruntime"),
            None
        );
    }
}

#[cfg(not(target_os = "windows"))]
#[test]
fn active_library_path_finds_versioned_library_when_unversioned_missing() {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    let lib_dir = tmp.path().join("ort/v0/lib");
    std::fs::create_dir_all(&lib_dir).unwrap();
    let versioned_name = if cfg!(target_os = "macos") {
        "libonnxruntime.1.25.1.dylib"
    } else {
        "libonnxruntime.so.1.25.1"
    };
    std::fs::write(lib_dir.join(versioned_name), b"x").unwrap();
    write_index(tmp.path(), "ort", "v0");
    let p = active_library_path("ort", "onnxruntime").expect("should resolve via versioned");
    assert!(p.to_string_lossy().contains(versioned_name));
    paths::set_engines_dir_override(None);
}

#[cfg(not(target_os = "windows"))]
#[test]
fn active_library_path_unversioned_preferred_over_versioned() {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    let lib_dir = tmp.path().join("ort/v0/lib");
    std::fs::create_dir_all(&lib_dir).unwrap();
    let unversioned_name = platform_library_filename("onnxruntime");
    let versioned_name = if cfg!(target_os = "macos") {
        "libonnxruntime.1.25.1.dylib"
    } else {
        "libonnxruntime.so.1.25.1"
    };
    std::fs::write(lib_dir.join(&unversioned_name), b"u").unwrap();
    std::fs::write(lib_dir.join(versioned_name), b"v").unwrap();
    write_index(tmp.path(), "ort", "v0");
    let p = active_library_path("ort", "onnxruntime").expect("should resolve");
    assert!(p.ends_with(&unversioned_name));
    paths::set_engines_dir_override(None);
}

#[cfg(not(target_os = "windows"))]
#[test]
fn active_library_path_picks_highest_versioned() {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    let lib_dir = tmp.path().join("ort/v0/lib");
    std::fs::create_dir_all(&lib_dir).unwrap();
    let (lower, higher) = if cfg!(target_os = "macos") {
        ("libonnxruntime.1.20.0.dylib", "libonnxruntime.1.25.1.dylib")
    } else {
        ("libonnxruntime.so.1.20.0", "libonnxruntime.so.1.25.1")
    };
    std::fs::write(lib_dir.join(lower), b"a").unwrap();
    std::fs::write(lib_dir.join(higher), b"b").unwrap();
    write_index(tmp.path(), "ort", "v0");
    let p = active_library_path("ort", "onnxruntime").expect("should resolve");
    assert!(p.to_string_lossy().contains(higher));
    paths::set_engines_dir_override(None);
}
