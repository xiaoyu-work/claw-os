use super::*;
use std::fs;
use std::io::Write;

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cos-subdir-hints-{}-{}",
        tag,
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn touch(p: &Path) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut f = fs::File::create(p).unwrap();
    writeln!(f, "{{}}").unwrap();
}

#[test]
fn marker_table_is_non_empty_and_alphabetised_within_kinds() {
    // Sanity: enough markers to be useful.
    assert!(MARKERS.len() >= 30);
    // Manifest entries should be sorted alphabetically (the table
    // claims this in its doc-comment for searchability).
    let manifests: Vec<&str> = MARKERS
        .iter()
        .filter(|m| m.kind == HintKind::Manifest)
        .map(|m| m.name)
        .collect();
    let mut sorted = manifests.clone();
    sorted.sort();
    assert_eq!(manifests, sorted, "Manifest markers should be alphabetical");
}

#[test]
fn scan_dir_finds_cargo_toml_at_root() {
    let dir = tmp_dir("cargo");
    touch(&dir.join("Cargo.toml"));
    let hits = scan_dir(&dir);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rel, "Cargo.toml");
    assert_eq!(hits[0].kind, HintKind::Manifest);
    assert_eq!(hits[0].label, "Rust crate");
    assert!(!hits[0].is_dir);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn scan_dir_distinguishes_dir_marker_from_file_collision() {
    let dir = tmp_dir("git-collision");
    // Create `.git` as a *file* — should not register as the
    // VCS marker (which expects a directory).
    touch(&dir.join(".git"));
    let hits = scan_dir(&dir);
    assert!(
        hits.iter().all(|h| h.label != "Git repository"),
        "expected no Git hint when .git is a file, got: {hits:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn scan_dir_finds_dir_markers() {
    let dir = tmp_dir("git-real");
    fs::create_dir_all(dir.join(".git")).unwrap();
    let hits = scan_dir(&dir);
    assert!(hits.iter().any(|h| h.label == "Git repository"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn scan_dir_does_not_descend_into_subdirs() {
    let dir = tmp_dir("flat");
    fs::create_dir_all(dir.join("subproj")).unwrap();
    touch(&dir.join("subproj").join("Cargo.toml"));
    let hits = scan_dir(&dir);
    assert!(
        hits.is_empty(),
        "single-level scan should not find subproj/Cargo.toml"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn scan_dir_recursive_finds_subdir_markers() {
    let dir = tmp_dir("deep");
    fs::create_dir_all(dir.join("apps").join("web")).unwrap();
    touch(&dir.join("apps").join("web").join("package.json"));
    let hits = scan_dir_recursive(&dir, 3);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rel, "apps/web/package.json");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn scan_dir_recursive_skips_noise_dirs() {
    let dir = tmp_dir("noise");
    fs::create_dir_all(dir.join("node_modules").join("foo")).unwrap();
    touch(&dir.join("node_modules").join("foo").join("package.json"));
    fs::create_dir_all(dir.join("real-pkg")).unwrap();
    touch(&dir.join("real-pkg").join("package.json"));
    let hits = scan_dir_recursive(&dir, 3);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].rel.starts_with("real-pkg/"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn scan_dir_recursive_respects_max_depth() {
    let dir = tmp_dir("depth-cap");
    fs::create_dir_all(dir.join("a").join("b").join("c")).unwrap();
    touch(&dir.join("a").join("b").join("c").join("Cargo.toml"));
    // depth 2 from root: visits root, a/, a/b/ — does NOT enter a/b/c/.
    let hits = scan_dir_recursive(&dir, 2);
    assert!(
        hits.is_empty(),
        "depth-2 scan should not find a/b/c/Cargo.toml"
    );
    // depth 3 should pick it up.
    let hits = scan_dir_recursive(&dir, 3);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rel, "a/b/c/Cargo.toml");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn scan_dir_returns_alphabetical_order_by_rel() {
    let dir = tmp_dir("order");
    touch(&dir.join("package.json"));
    touch(&dir.join("Cargo.toml"));
    touch(&dir.join("go.mod"));
    let hits = scan_dir(&dir);
    let rels: Vec<&str> = hits.iter().map(|h| h.rel.as_str()).collect();
    let mut sorted = rels.clone();
    sorted.sort();
    assert_eq!(rels, sorted);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn render_summary_groups_repeated_labels() {
    let hits = vec![
        Hint {
            rel: "package.json".into(),
            kind: HintKind::Manifest,
            label: "Node.js project",
            is_dir: false,
        },
        Hint {
            rel: "apps/web/package.json".into(),
            kind: HintKind::Manifest,
            label: "Node.js project",
            is_dir: false,
        },
    ];
    let s = render_summary(&hits);
    assert!(s.contains("2 locations"));
    assert!(s.contains("Node.js project"));
}

#[test]
fn render_summary_single_hit_shows_path() {
    let hits = vec![Hint {
        rel: "Cargo.toml".into(),
        kind: HintKind::Manifest,
        label: "Rust crate",
        is_dir: false,
    }];
    let s = render_summary(&hits);
    assert!(s.contains("Rust crate"));
    assert!(s.contains("Cargo.toml"));
}

#[test]
fn render_summary_empty_returns_empty_string() {
    assert_eq!(render_summary(&[]), "");
}

#[test]
fn relative_to_handles_root_itself() {
    let dir = tmp_dir("rel-self");
    // The file's relative path under itself is the basename.
    let p = dir.join("Cargo.toml");
    touch(&p);
    let r = relative_to(&dir, &p).unwrap();
    assert_eq!(r, "Cargo.toml");
    let _ = fs::remove_dir_all(&dir);
}
