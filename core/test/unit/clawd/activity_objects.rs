use super::*;
use crate::test_env::TestEnvVarGuard;

fn peer(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(uid),
        gid: Some(uid),
        start_time_ticks: Some(1),
    }
}

fn package(root: &std::path::Path, signed: bool) {
    let dir = root.join("demo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), json!({
        "id":"demo","version":"1","name":{"en":"Demo"},
        "objects":{"entry":{"label":{"en":"Entry"},"resolve":{"operation":"get","id_arg":"key"}}},
        "operations":{"get":{"label":{"en":"Get"},"args":[
            {"name":"key","kind":"name","required":true}
        ]}}
    }).to_string()).unwrap();
    std::fs::write(
        dir.join("main.py"),
        "raise AssertionError('no automatic data resolution')\n",
    )
    .unwrap();
    if signed {
        crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::App, "demo");
    }
}

fn create_activity() -> Value {
    super::super::activities::create(
        json!({"title":"Release","goal":"Prepare a release"}),
        &peer(1000),
    )
    .unwrap()
}

fn attachment(id: &str) -> Value {
    json!({
        "id":id, "label":"Release status",
        "object":{"app_id":"demo","object_type":"entry","object_id":"release.status"}
    })
}

#[test]
fn activity_object_attachment_and_description_share_owner_scoped_metadata_without_execution() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    package(apps.path(), true);
    let activity = create_activity();
    let id = activity["id"].as_str().unwrap();
    let attached = attach(attachment(id), &peer(1000)).unwrap();
    assert_eq!(
        attached["resources"][0]["reference"],
        "app://demo/entry?id=release.status"
    );
    assert_eq!(attached["state"], "active");
    let view = list(json!({"id":id}), &peer(1000)).unwrap();
    assert_eq!(view["activity_id"], id);
    assert_eq!(view["objects"][0]["status"], "declared");
    assert_eq!(
        view["objects"][0]["description"]["invocation"]["args"],
        json!(["--", "release.status"])
    );
    assert!(view["objects"][0]["description"].get("data").is_none());
    assert!(!crate::paths::agent_jobs_dir().exists());

    for uid in [0, 2000] {
        assert!(attach(attachment(id), &peer(uid)).is_err());
        assert!(list(json!({"id":id}), &peer(uid)).is_err());
    }
    assert!(attach(attachment(id), &ClientIdentity::unknown()).is_err());
}

#[test]
fn activity_object_status_retains_legacy_invalid_and_unavailable_references_honestly() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    package(apps.path(), false);
    let activity = super::super::activities::create(
        json!({
            "title":"Legacy","goal":"Keep references",
            "resources":[
                {"label":"File","reference":"/home/user/file"},
                {"label":"Malformed","reference":"app:bad"},
                {"label":"Unsigned","reference":"app://demo/entry?id=key"}
            ]
        }),
        &peer(1000),
    )
    .unwrap();
    let id = activity["id"].as_str().unwrap();
    let result = list(json!({"id":id}), &peer(1000)).unwrap();
    assert_eq!(result["objects"].as_array().unwrap().len(), 2);
    assert_eq!(result["objects"][0]["status"], "invalid");
    assert_eq!(result["objects"][1]["status"], "unavailable");
    for entry in result["objects"].as_array().unwrap() {
        assert!(entry["description"].is_null());
        assert!(!entry["error"].as_str().unwrap().is_empty());
    }
    assert!(attach(attachment(id), &peer(1000)).is_err());
    let unchanged = activities::open_default().unwrap().get(1000, id).unwrap();
    assert_eq!(unchanged.resources.len(), 3);
}

#[test]
fn activity_object_mutation_preserves_editability_and_respects_current_package_verification() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    package(apps.path(), true);
    let activity = create_activity();
    let id = activity["id"].as_str().unwrap();
    let service = activities::open_default().unwrap();
    service
        .transition(1000, id, activities::ActivityState::Paused, None)
        .unwrap();
    attach(attachment(id), &peer(1000)).unwrap();
    let manifest_path = apps.path().join("demo").join("app.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["objects"]["entry"]["label"]["en"] = json!("Changed declaration");
    std::fs::write(manifest_path, manifest.to_string()).unwrap();
    let status = list(json!({"id":id}), &peer(1000)).unwrap();
    assert_eq!(status["objects"][0]["status"], "unavailable");
    assert!(attach(attachment(id), &peer(1000)).is_err());
    assert_eq!(service.get(1000, id).unwrap().resources.len(), 1);
}
