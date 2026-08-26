use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "cos-context-engine-{}-{}",
        name,
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

// ---- empty / defaults ----

#[test]
fn empty_options_yield_empty_block() {
    let block = build(&ContextOptions::default());
    assert!(block.is_empty());
    assert_eq!(block.rendered(), "");
    assert_eq!(block.rendered_opt(), None);
    assert_eq!(block.hints.len(), 0);
    assert_eq!(block.references.len(), 0);
    assert_eq!(block.notes.len(), 0);
}

#[test]
fn empty_user_text_does_not_yield_references() {
    let block = build(&ContextOptions::default().with_user_text(""));
    assert!(block.is_empty());
}

// ---- hints ----

#[test]
fn hints_only_render_in_block() {
    let dir = temp_dir("hints-only");
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    let block = build(&ContextOptions::default().with_cwd(&dir));
    assert!(!block.is_empty());
    assert_eq!(block.hints.len(), 1);
    assert_eq!(block.hints[0].label, "Rust crate");
    let rendered = block.rendered();
    assert!(rendered.contains("<PROJECT_CONTEXT>"));
    assert!(rendered.contains("</PROJECT_CONTEXT>"));
    assert!(rendered.contains("Project hints"));
    assert!(rendered.contains("Rust crate"));
    assert!(!rendered.contains("User references"));
    assert!(!rendered.contains("Notes:"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hints_recursive_scan_finds_nested_manifests() {
    let dir = temp_dir("hints-recur");
    let nested = dir.join("apps").join("web");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("package.json"), "{}").unwrap();
    let depth0 = build(&ContextOptions::default().with_cwd(&dir));
    assert_eq!(depth0.hints.len(), 0);
    let depth3 = build(&ContextOptions::default().with_cwd(&dir).with_scan_depth(3));
    assert_eq!(depth3.hints.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hint_cap_is_respected() {
    let dir = temp_dir("hints-cap");
    std::fs::write(dir.join("Cargo.toml"), "").unwrap();
    std::fs::write(dir.join("package.json"), "{}").unwrap();
    std::fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
    let mut opts = ContextOptions::default().with_cwd(&dir);
    opts.max_hints = Some(2);
    let block = build(&opts);
    assert_eq!(block.hints.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- references ----

#[test]
fn references_only_render_in_block() {
    let block = build(
        &ContextOptions::default().with_user_text("see @notes.md and @https://example.com/x"),
    );
    assert_eq!(block.references.len(), 2);
    let r = block.rendered();
    assert!(r.contains("<PROJECT_CONTEXT>"));
    assert!(r.contains("User references in this turn:"));
    assert!(r.contains("notes.md (Path)"));
    assert!(r.contains("https://example.com/x (Url)"));
    assert!(!r.contains("Project hints"));
}

#[test]
fn references_dedup_default_collapses_duplicates() {
    let block = build(&ContextOptions::default().with_user_text("@a @a @a"));
    assert_eq!(block.references.len(), 1);
}

#[test]
fn references_dedup_off_keeps_all_occurrences() {
    let mut opts = ContextOptions::default().with_user_text("@a @a @a");
    opts.dedup_refs = false;
    let block = build(&opts);
    assert_eq!(block.references.len(), 3);
}

#[test]
fn reference_cap_is_respected() {
    let body = "@a @b @c @d @e";
    let mut opts = ContextOptions::default().with_user_text(body);
    opts.max_refs = Some(2);
    let block = build(&opts);
    assert_eq!(block.references.len(), 2);
}

// ---- notes ----

#[test]
fn notes_only_render_in_block() {
    let block = build(
        &ContextOptions::default()
            .with_note("host: Windows 11")
            .with_note("user has 12 MB free"),
    );
    assert_eq!(block.notes.len(), 2);
    let r = block.rendered();
    assert!(r.contains("<PROJECT_CONTEXT>"));
    assert!(r.contains("Notes:"));
    assert!(r.contains("host: Windows 11"));
    assert!(r.contains("user has 12 MB free"));
}

// ---- composition ----

#[test]
fn full_block_concatenates_all_sections_in_order() {
    let dir = temp_dir("hints-full");
    std::fs::write(dir.join("Cargo.toml"), "").unwrap();
    let block = build(
        &ContextOptions::default()
            .with_cwd(&dir)
            .with_user_text("see @lib.rs")
            .with_note("test mode"),
    );
    let r = block.rendered();
    let pos_hints = r.find("Project hints").expect("hints present");
    let pos_refs = r.find("User references").expect("refs present");
    let pos_notes = r.find("Notes:").expect("notes present");
    assert!(pos_hints < pos_refs);
    assert!(pos_refs < pos_notes);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deterministic_output_for_same_input() {
    let dir = temp_dir("hints-determ");
    std::fs::write(dir.join("Cargo.toml"), "").unwrap();
    std::fs::write(dir.join("package.json"), "{}").unwrap();
    let opts = ContextOptions::default()
        .with_cwd(&dir)
        .with_user_text("@a @b")
        .with_note("z")
        .with_note("y");
    let a = build(&opts);
    let b = build(&opts);
    assert_eq!(a.rendered(), b.rendered());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- to_json ----

#[test]
fn to_json_roundtrips_basic_fields() {
    let block = build(&ContextOptions::default().with_user_text("@a"));
    let v = block.to_json();
    assert_eq!(v.get("is_empty").and_then(|b| b.as_bool()), Some(false));
    let refs = v.get("references").and_then(|r| r.as_array()).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].get("raw").and_then(|s| s.as_str()), Some("a"));
    assert!(v
        .get("rendered")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .contains("PROJECT_CONTEXT"));
}

#[test]
fn to_json_empty_block_marked_empty() {
    let block = build(&ContextOptions::default());
    let v = block.to_json();
    assert_eq!(v.get("is_empty").and_then(|b| b.as_bool()), Some(true));
    assert!(v.get("rendered").map(|x| x.is_null()).unwrap_or(false));
}
