use super::*;
use serde_json::json;

fn fixture() -> (tempfile::TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("demo");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("app.json"), json!({
        "id":"demo","version":"1","name":{"en":"Demo"},
        "operations":{
            "write":{"label":{"en":"Write"},"args":[
                {"name":"path","kind":"path","required":true},
                {"name":"content","kind":"text","binding":"flag"}
            ],"effects":[{"kind":"update","label":{"en":"Write file"},"target_arg":"path"}]},
            "legacy":{"label":{"en":"Legacy"}},
            "many":{"label":{"en":"Read several"},"args":[
                {"name":"names","kind":"name","required":true,"repeatable":true}
            ],"effects":[{"kind":"read","label":{"en":"Read requested names"},"target_arg":"names","recovery":"not_applicable"}]}
        }
    }).to_string()).unwrap();
    std::fs::write(
        dir.join("main.py"),
        "raise AssertionError('preview must never execute')\n",
    )
    .unwrap();
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::App, "demo");
    let app = crate::apps::find_verified(root.path(), "demo").unwrap();
    (root, app)
}

#[test]
fn effect_preview_is_app_declared_not_authorized_executed_or_confirmed() {
    let _lock = crate::test_env::lock_env();
    let (_root, app) = fixture();
    let result = preview(
        &app,
        "write",
        &[
            "relative/file".into(),
            "--content".into(),
            "private contents".into(),
        ],
    )
    .unwrap();
    assert!(result.effects_declared);
    assert!(!result.authorization_checked && !result.executed && !result.effects_confirmed);
    assert_eq!(result.effects[0].requested_targets, ["relative/file"]);
    assert_eq!(result.effects[0].target_state, "requested");
    assert_eq!(result.effects[0].recovery, EffectRecovery::Unknown);
    assert!(!serde_json::to_string(&result)
        .unwrap()
        .contains("private contents"));
    assert!(!result.package_digest.is_empty());
}

#[cfg(unix)]
#[test]
fn effect_preview_does_not_resolve_symlinks_or_read_target_data() {
    let _lock = crate::test_env::lock_env();
    let (root, app) = fixture();
    let target = root.path().join("private-target");
    let link = root.path().join("requested-link");
    std::fs::write(&target, "private-file-content").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let result = preview(&app, "write", &[link.to_string_lossy().into_owned()]).unwrap();
    assert_eq!(
        result.effects[0].requested_targets,
        [link.to_string_lossy().to_string()]
    );
    assert!(!serde_json::to_string(&result)
        .unwrap()
        .contains("private-file-content"));
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        "private-file-content"
    );
}

#[test]
fn missing_effect_metadata_is_unknown_not_implicitly_pure() {
    let _lock = crate::test_env::lock_env();
    let (_root, app) = fixture();
    let result = preview(&app, "legacy", &[]).unwrap();
    assert!(!result.effects_declared && result.effects.is_empty());
    assert!(result.notes.iter().any(|note| note.contains("unknown")));
    let many = preview(&app, "many", &["one".into(), "two".into()]).unwrap();
    assert_eq!(many.effects[0].requested_targets, ["one", "two"]);
}

#[test]
fn invalid_arguments_and_changed_manifests_are_refused() {
    let _lock = crate::test_env::lock_env();
    let (root, app) = fixture();
    assert!(preview(&app, "write", &[]).is_err());
    assert!(preview(&app, "missing", &[]).is_err());
    assert!(preview(&app, "write", &vec!["x".into(); 65]).is_err());
    assert!(preview(&app, "write", &["x".repeat(8193)]).is_err());
    let path = root.path().join("demo/app.json");
    let changed = std::fs::read_to_string(&path)
        .unwrap()
        .replace("Write file", "Changed effect");
    std::fs::write(path, changed).unwrap();
    assert!(preview(&app, "write", &["file".into()]).is_err());
}

#[test]
fn runtime_provider_selection_remains_explicitly_unresolved() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("calendar");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("app.json"), json!({
        "id":"calendar","version":"1","name":{"en":"Calendar"},
        "operations":{"list":{"label":{"en":"List"},"args":[
            {"name":"provider","kind":"name","binding":"flag","trusted_resolver":"calendar-provider","choices":["local","google","outlook"]}
        ],"effects":[{"kind":"read","label":{"en":"Read selected provider"},"target_arg":"provider","recovery":"not_applicable"}]}}
    }).to_string()).unwrap();
    std::fs::write(
        dir.join("main.py"),
        "raise AssertionError('no execution')\n",
    )
    .unwrap();
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::App, "calendar");
    let app = crate::apps::find_verified(root.path(), "calendar").unwrap();
    let result = preview(&app, "list", &[]).unwrap();
    assert_eq!(result.unresolved_arguments, ["provider"]);
    assert_eq!(result.effects[0].target_state, "unresolved");
    assert!(result.effects[0].requested_targets.is_empty());
}
