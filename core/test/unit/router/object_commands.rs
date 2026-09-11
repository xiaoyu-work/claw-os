use super::*;
use crate::objects::ObjectCatalog;

fn fixture() -> (tempfile::TempDir, InstalledObjectCatalog) {
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("demo");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(app.join("app.json"), json!({
        "id":"demo","version":"1","name":{"en":"Demo"},
        "objects":{"entry":{"label":{"en":"Entry"},"resolve":{"operation":"get","id_arg":"key"}}},
        "operations":{"get":{"label":{"en":"Get"},"args":[
            {"name":"key","kind":"name","required":true}
        ]}}
    }).to_string()).unwrap();
    std::fs::write(
        app.join("main.py"),
        "raise AssertionError('do not execute during discovery')\n",
    )
    .unwrap();
    crate::test_env::sign_test_package(&app, crate::provenance::PackageKind::App, "demo");
    let catalog = InstalledObjectCatalog::new(root.path());
    (root, catalog)
}

#[test]
fn object_reference_command_is_pure_and_preserves_literal_ids() {
    let reference = reference_args(&[
        "demo".into(),
        "entry".into(),
        "--revision=v1".into(),
        "--".into(),
        "--schema".into(),
    ])
    .unwrap();
    assert_eq!(reference.object_id, "--schema");
    assert_eq!(reference.revision.as_deref(), Some("v1"));
    let result = crate::router::dispatch(&[
        "object".into(),
        "reference".into(),
        "demo".into(),
        "entry".into(),
        "--".into(),
        "--schema".into(),
    ])
    .unwrap()
    .unwrap();
    let value: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(value["reference"], "app://demo/entry?id=--schema");
}

#[test]
fn object_description_never_calls_an_executor() {
    let _lock = crate::test_env::lock_env();
    let (_root, catalog) = fixture();
    let value = run_with(
        &catalog,
        "describe",
        &["app://demo/entry?id=missing".into()],
        |_, _| panic!("description executed an App"),
    )
    .unwrap();
    assert_eq!(value["invocation"]["operation"], "get");
    assert_eq!(catalog.list(None).unwrap().apps.len(), 1);
}

#[test]
fn object_resolution_forwards_the_verified_app_and_propagates_normal_policy_errors() {
    let _lock = crate::test_env::lock_env();
    let (_root, catalog) = fixture();
    let error = run_with(
        &catalog,
        "resolve",
        &["app://demo/entry?id=key".into()],
        |app, invocation| {
            assert!(app.is_verified());
            assert_eq!(invocation.app_id, app.manifest.id);
            assert_eq!(invocation.operation, "get");
            assert_eq!(invocation.args, ["--", "key"]);
            Err("normal App permission denied".into())
        },
    )
    .unwrap_err();
    assert_eq!(error, "normal App permission denied");
    let value = run_with(
        &catalog,
        "resolve",
        &["app://demo/entry?id=key".into()],
        |_, _| Ok(Some(r#"{"key":"key","value":"result"}"#.into())),
    )
    .unwrap();
    assert_eq!(value["value"], "result");
}

#[test]
fn object_resolution_never_fabricates_missing_or_failed_results() {
    let _lock = crate::test_env::lock_env();
    let (_root, catalog) = fixture();
    for result in [
        None,
        Some("not json".into()),
        Some(r#"{"error":"missing"}"#.into()),
    ] {
        assert!(run_with(
            &catalog,
            "resolve",
            &["app://demo/entry?id=key".into()],
            |_, _| Ok(result),
        )
        .is_err());
    }
}
