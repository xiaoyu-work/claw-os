use super::*;

thread_local! {
    static ENGINES_DIR_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn engines_dir_override() -> Option<PathBuf> {
    ENGINES_DIR_OVERRIDE.with(|c| c.borrow().clone())
}

pub fn set_engines_dir_override(p: Option<PathBuf>) {
    ENGINES_DIR_OVERRIDE.with(|c| *c.borrow_mut() = p);
}

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
