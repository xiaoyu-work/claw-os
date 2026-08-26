use super::*;

#[test]
fn model_paths_compose() {
    let v = model_version_dir("whisper-small", "v1");
    assert!(v.ends_with("whisper-small/v1") || v.ends_with("whisper-small\\v1"));
    let m = manifest_path("whisper-small", "v1");
    assert!(m.ends_with("manifest.json"));
}

#[test]
fn within_models_dir_check() {
    let m = models_dir().join("foo").join("v1");
    assert!(is_within_models_dir(&m));
    assert!(!is_within_models_dir(Path::new("/etc/passwd")));
}
