//! Source-kind regressions for immutable migrated App fixtures.

use std::path::Path;

use serde_json::{json, Value};

#[path = "../test/support/app_sources.rs"]
mod app_sources;

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn write_json(root: &Path, relative: &str, value: &Value) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    write_json(
        root.path(),
        "packaging/apps.lock.json",
        &json!({
            "version": 1, "revision": REVISION, "products": ["files"],
            "capabilities": ["document-engine"], "apps": ["fs", "doc"],
        }),
    );
    let source = root.path().join("build/app-sources").join(REVISION);
    write_json(
        &source,
        "products/files/package.json",
        &json!({"apps": ["apps/fs"]}),
    );
    write_json(
        &source,
        "products/files/apps/fs/app.json",
        &json!({"id": "fs"}),
    );
    write_json(
        &source,
        "capabilities/document-engine/package.json",
        &json!({
            "kind": "shared-capability-client", "apps": ["apps/doc"],
        }),
    );
    write_json(
        &source,
        "capabilities/document-engine/apps/doc/app.json",
        &json!({"id": "doc"}),
    );
    write_json(
        root.path(),
        "apps/doc/app.json",
        &json!({"id": "doc", "stale": true}),
    );
    root
}

#[test]
fn declared_capability_and_product_resolve_only_their_locked_roots() {
    let root = fixture();
    let source = root.path().join("build/app-sources").join(REVISION);
    assert_eq!(
        app_sources::app_dir_in(root.path(), "doc"),
        source.join("capabilities/document-engine/apps/doc")
    );
    assert_eq!(
        app_sources::app_dir_in(root.path(), "fs"),
        source.join("products/files/apps/fs")
    );
}

#[test]
fn missing_locked_capability_never_uses_the_local_os_copy() {
    let root = fixture();
    std::fs::remove_file(root.path().join(format!(
        "build/app-sources/{REVISION}/capabilities/document-engine/package.json"
    )))
    .unwrap();
    assert!(root.path().join("apps/doc/app.json").exists());
    assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
}

#[test]
fn capability_source_kind_is_explicit() {
    let root = fixture();
    let source = root.path().join("build/app-sources").join(REVISION);
    write_json(
        &source,
        "capabilities/document-engine/package.json",
        &json!({
            "kind": "product", "apps": ["apps/doc"],
        }),
    );
    assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
}

#[test]
fn lock_rejects_duplicate_traversing_or_nonimmutable_source_declarations() {
    let root = fixture();
    let path = root.path().join("packaging/apps.lock.json");
    let baseline: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for (field, value) in [
        ("capabilities", json!(["files"])),
        (
            "capabilities",
            json!(["document-engine", "document-engine"]),
        ),
        ("capabilities", json!(["../document-engine"])),
        ("apps", json!(["doc", "doc"])),
        ("revision", json!("main")),
    ] {
        let mut lock = baseline.clone();
        lock[field] = value;
        write_json(root.path(), "packaging/apps.lock.json", &lock);
        assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
    }
}

#[test]
fn missing_duplicate_or_traversing_app_layout_never_uses_local_source() {
    let root = fixture();
    let source = root.path().join("build/app-sources").join(REVISION);
    for apps in [
        json!([]),
        json!(["apps/doc", "apps/doc"]),
        json!(["apps/../doc"]),
        json!(["apps/missing"]),
    ] {
        write_json(
            &source,
            "capabilities/document-engine/package.json",
            &json!({
                "kind": "shared-capability-client", "apps": apps,
            }),
        );
        assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
    }
}

#[test]
fn capability_cannot_add_native_product_assets() {
    let root = fixture();
    let source = root.path().join("build/app-sources").join(REVISION);
    write_json(
        &source,
        "capabilities/document-engine/package.json",
        &json!({
            "kind": "shared-capability-client", "apps": ["apps/doc"], "native": {},
        }),
    );
    assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_capability_source_cannot_escape_its_declared_kind() {
    let root = fixture();
    let group = root.path().join(format!(
        "build/app-sources/{REVISION}/capabilities/document-engine"
    ));
    let outside = root.path().join("outside");
    std::fs::rename(&group, &outside).unwrap();
    std::os::unix::fs::symlink(outside, group).unwrap();
    assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
}

#[test]
fn version_one_product_only_locks_remain_supported() {
    let root = fixture();
    write_json(
        root.path(),
        "packaging/apps.lock.json",
        &json!({
            "version": 1, "revision": REVISION, "products": ["files"], "apps": ["fs"],
        }),
    );
    assert!(app_sources::app_dir_in(root.path(), "fs").ends_with("products/files/apps/fs"));
    assert_eq!(
        app_sources::app_dir_in(root.path(), "doc"),
        root.path().join("apps/doc")
    );
}

#[test]
fn published_doc_fixture_preserves_its_manifest_and_six_operations() {
    let directory = app_sources::app_dir("doc");
    assert!(directory.ends_with("capabilities/document-engine/apps/doc"));
    let manifest = cos::caps::manifest::Manifest::from_json(
        &std::fs::read_to_string(directory.join("app.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.id, "doc");
    assert_eq!(manifest.operations.len(), 6);
    assert!(manifest.operations.contains_key("read"));
    assert!(manifest.operations.contains_key("info"));
    assert!(manifest.operations.contains_key("convert"));
    assert!(manifest.operations.contains_key("summarize"));
    assert!(manifest.operations.contains_key("explain"));
    assert!(manifest.operations.contains_key("rewrite"));
    assert!(directory.join("main.py").is_file());
    assert!(directory.join("server.py").is_file());
}
