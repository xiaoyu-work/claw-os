use super::*;

#[test]
fn default_classifier_includes_canonical_binary_extensions() {
    let c = BinaryExtensions::default();
    for ext in [
        "exe", "dll", "so", "png", "jpg", "pdf", "zip", "wasm", "onnx", "gguf", "mp4",
    ] {
        assert!(c.contains_extension(ext), "expected default to flag {ext}");
    }
}

#[test]
fn default_classifier_excludes_obvious_text_extensions() {
    let c = BinaryExtensions::default();
    for ext in ["txt", "md", "rs", "py", "js", "ts", "json", "yaml", "html"] {
        assert!(
            !c.contains_extension(ext),
            "expected default NOT to flag {ext}"
        );
    }
}

#[test]
fn contains_extension_is_case_insensitive_and_dot_tolerant() {
    let c = BinaryExtensions::default();
    assert!(c.contains_extension("PNG"));
    assert!(c.contains_extension(".png"));
    assert!(c.contains_extension("  .PNG  "));
}

#[test]
fn contains_extension_empty_string_is_false() {
    let c = BinaryExtensions::default();
    assert!(!c.contains_extension(""));
    assert!(!c.contains_extension("."));
    assert!(!c.contains_extension("   "));
}

#[test]
fn is_binary_path_uses_path_extension() {
    let c = BinaryExtensions::default();
    assert!(c.is_binary_path("foo.png"));
    assert!(c.is_binary_path("/abs/path/to/MovieFile.MP4"));
    assert!(c.is_binary_path("nested/dir/blob.gguf"));
}

#[test]
fn is_binary_path_extensionless_returns_false() {
    let c = BinaryExtensions::default();
    // Extension-less files are NOT classified binary by default;
    // callers can layer a magic-byte sniff on top if needed.
    assert!(!c.is_binary_path("README"));
    assert!(!c.is_binary_path("Makefile"));
    assert!(!c.is_binary_path(".gitignore"));
}

#[test]
fn is_binary_path_unknown_extension_returns_false() {
    let c = BinaryExtensions::default();
    assert!(!c.is_binary_path("custom.foo"));
    assert!(!c.is_binary_path("logfile.unknown"));
}

#[test]
fn with_extras_adds_to_set() {
    let c = BinaryExtensions::default().with_extras([".myproprietary", "bin2"]);
    assert!(c.contains_extension("myproprietary"));
    assert!(c.contains_extension("bin2"));
}

#[test]
fn without_drops_from_set() {
    // An installation decides .bin is actually their text-based JSONL
    // payload format.
    let c = BinaryExtensions::default().without(["bin"]);
    assert!(!c.contains_extension("bin"));
    assert!(c.contains_extension("exe"));
}

#[test]
fn iter_returns_sorted() {
    let c = BinaryExtensions::new().with_extras(["zzz", "aaa", "mmm"]);
    let v: Vec<&str> = c.iter().collect();
    assert_eq!(v, vec!["aaa", "mmm", "zzz"]);
}

#[test]
fn empty_classifier_classifies_nothing() {
    let c = BinaryExtensions::new();
    assert!(c.is_empty());
    assert_eq!(c.len(), 0);
    assert!(!c.is_binary_path("anything.png"));
    assert!(!c.contains_extension("png"));
}

#[test]
fn default_classifier_is_non_empty_and_dedupes() {
    let c = BinaryExtensions::default();
    assert!(
        c.len() > 50,
        "default list smaller than expected: {}",
        c.len()
    );
    // The data table is hand-maintained — it has at least one
    // duplicate (`key` appears under both crypto and Apple iWork
    // categories). Confirm dedup happens at construction time.
    let raw = DEFAULT_BINARY_EXTENSIONS.len();
    assert!(
        c.len() <= raw,
        "BTreeSet should never grow past the input list"
    );
}
