use super::*;
use std::path::PathBuf;

#[test]
fn engine_version_from_lib_path_unix_layout() {
    let version = crate::engine_pkg::ORT_GENAI_KNOWN_GOOD_VERSION;
    let p = PathBuf::from(format!(
        "/var/lib/cos/engines/ort-genai/{version}/lib/libonnxruntime-genai.so"
    ));
    assert_eq!(engine_version_from_lib_path(&p), Some(version.into()));
}

// Windows-layout test removed; see
// `crate::model::engines::ort::tests` for the rationale.

#[test]
fn engine_version_from_lib_path_too_short() {
    let too_short = PathBuf::from("/tmp/onnxruntime-genai.dll");
    assert!(engine_version_from_lib_path(&too_short).is_none());
}

#[test]
fn lib_basename_unchanged() {
    assert_eq!(LIB_BASENAME, "onnxruntime-genai");
}

#[test]
fn pkg_engine_name_is_kebab() {
    assert_eq!(PKG_ENGINE_NAME, "ort-genai");
}
