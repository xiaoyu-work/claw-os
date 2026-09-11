use super::*;
use serde_json::json;

fn fixture(signed: bool) -> (tempfile::TempDir, InstalledObjectCatalog) {
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("demo");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(
        app.join("app.json"),
        json!({
            "id": "demo", "version": "1", "name": {"en": "Demo"},
            "objects": {
                "entry": {
                    "label": {"en": "Entry"},
                    "resolve": {"operation": "get", "id_arg": "key", "revision_arg": "revision"}
                },
                "file": {
                    "label": {"en": "File"},
                    "resolve": {"operation": "stat", "id_arg": "path"}
                }
            },
            "operations": {
                "get": {"label": {"en": "Get"}, "args": [
                    {"name": "key", "kind": "name", "required": true},
                    {"name": "revision", "kind": "text", "binding": "flag"}
                ]},
                "stat": {"label": {"en": "Stat"}, "args": [
                    {"name": "path", "kind": "path", "required": true}
                ]}
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        app.join("main.py"),
        "raise AssertionError('metadata must never execute App code')\n",
    )
    .unwrap();
    if signed {
        crate::test_env::sign_test_package(&app, crate::provenance::PackageKind::App, "demo");
    }
    let catalog = InstalledObjectCatalog::new(root.path());
    (root, catalog)
}

fn object(object_type: &str, object_id: &str, revision: Option<&str>) -> ObjectRef {
    ObjectRef {
        app_id: "demo".into(),
        object_type: object_type.into(),
        object_id: object_id.into(),
        revision: revision.map(str::to_string),
    }
}

#[test]
fn object_catalog_authenticates_declarations_without_executing_or_reading_objects() {
    let _lock = crate::test_env::lock_env();
    let (_root, catalog) = fixture(true);
    let result = catalog.list(None).unwrap();
    assert_eq!(result.apps.len(), 1);
    assert_eq!(result.apps[0].objects.len(), 2);
    let description = catalog
        .describe(&object("entry", "does-not-exist", None))
        .unwrap();
    assert_eq!(description.object_label, "Entry");
    assert_eq!(description.invocation.operation, "get");
    assert_eq!(description.invocation.args, ["--", "does-not-exist"]);
    assert!(!description.provenance.is_null());
}

#[test]
fn object_metadata_is_not_trusted_from_unsigned_or_tampered_packages() {
    let _lock = crate::test_env::lock_env();
    let (_root, unsigned) = fixture(false);
    let result = unsigned.list(None).unwrap();
    assert!(result.apps.is_empty());
    assert_eq!(result.quarantined.len(), 1);
    assert!(unsigned.describe(&object("entry", "key", None)).is_err());
    let (root, signed) = fixture(true);
    let (app, _) = signed.prepare(&object("entry", "key", None)).unwrap();
    let manifest_path = root.path().join("demo").join("app.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["objects"]["entry"]["label"]["en"] = json!("Unverified replacement");
    std::fs::write(manifest_path, manifest.to_string()).unwrap();
    assert!(assert_current(&app).is_err());
    assert!(signed.describe(&object("entry", "key", None)).is_err());
}

#[test]
fn object_resolution_preserves_opaque_ids_and_explicit_revision_constraints() {
    let _lock = crate::test_env::lock_env();
    let (_root, catalog) = fixture(true);
    let description = catalog
        .describe(&object(
            "entry",
            "--schema;$(not-a-shell)",
            Some("--revision-data"),
        ))
        .unwrap();
    assert_eq!(
        description.invocation.args,
        [
            "--revision=--revision-data",
            "--",
            "--schema;$(not-a-shell)"
        ]
    );
    assert!(catalog
        .describe(&object("file", "/home/user/file", Some("v1")))
        .is_err());
    assert!(catalog.describe(&object("missing", "key", None)).is_err());
}

#[test]
fn path_backed_objects_are_independent_of_terminal_or_desktop_working_directories() {
    let _lock = crate::test_env::lock_env();
    let (_root, catalog) = fixture(true);
    for path in ["relative/file", "~/file"] {
        assert!(
            catalog.describe(&object("file", path, None)).is_err(),
            "{path}"
        );
    }
    let description = catalog
        .describe(&object("file", "/home/user/file", None))
        .unwrap();
    assert_eq!(description.invocation.args, ["--", "/home/user/file"]);
}

#[test]
fn object_declarations_stop_resolving_after_artifact_revocation() {
    let _lock = crate::test_env::lock_env();
    let (root, catalog) = fixture(true);
    let app_dir = root.path().join("demo");
    std::fs::write(app_dir.join("main.py"), "# unique revocation fixture\n").unwrap();
    crate::test_env::sign_test_package(&app_dir, crate::provenance::PackageKind::App, "demo");
    let (app, _) = catalog.prepare(&object("entry", "key", None)).unwrap();
    let facts = app.provenance_facts();
    let digest = facts["content_digest"]
        .as_str()
        .expect("verified artifact digest");
    crate::test_env::revoke_test_package(digest);
    assert!(catalog.describe(&object("entry", "key", None)).is_err());
}
