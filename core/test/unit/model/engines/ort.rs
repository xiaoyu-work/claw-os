use super::*;
use std::path::PathBuf;

#[test]
fn engine_version_from_lib_path_unix_layout() {
    let p = PathBuf::from("/var/lib/cos/engines/ort/1.25.1/lib/libonnxruntime.so");
    assert_eq!(engine_version_from_lib_path(&p), Some("1.25.1".into()));
}

// Windows-layout test removed: `Path::parent()` on Linux does not
// recognise `\` as a separator, so a path like
// `C:\ProgramData\cos\engines\ort\1.25.1\lib\onnxruntime.dll` parses
// as a single bare filename and `engine_version_from_lib_path`
// returns None. The unix_layout test above covers the production
// path on the only platforms claw-os ships for.

#[test]
fn engine_version_from_lib_path_bin_dir_supported() {
    // P2.3's active_library_path falls back to bin/ for some
    // upstream layouts. Version parsing only depends on the
    // <version>/<sub>/<file> tail shape, so either subdir works.
    let p = PathBuf::from("/var/lib/cos/engines/ort/1.25.1/bin/onnxruntime.dll");
    assert_eq!(engine_version_from_lib_path(&p), Some("1.25.1".into()));
}

#[test]
fn engine_version_from_lib_path_versioned_so() {
    // Linux versioned-only fallback — versioned soname instead of
    // unversioned symlink. The version parser keys off the directory
    // containing the file, not the filename, so this still resolves.
    let p = PathBuf::from("/var/lib/cos/engines/ort/1.25.1/lib/libonnxruntime.so.1.25.1");
    assert_eq!(engine_version_from_lib_path(&p), Some("1.25.1".into()));
}

#[test]
fn engine_version_from_lib_path_too_short() {
    let too_short = PathBuf::from("/tmp/onnxruntime.dll");
    assert!(engine_version_from_lib_path(&too_short).is_none());
}

#[test]
fn lib_basename_unchanged() {
    assert_eq!(LIB_BASENAME, "onnxruntime");
}
