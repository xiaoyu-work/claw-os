use super::*;
use crate::test_env::TestEnvVarGuard;
use serde_json::json;

fn peer(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(uid),
        gid: Some(uid),
        start_time_ticks: Some(1),
    }
}

#[test]
fn activity_preview_checks_owner_before_app_discovery_and_never_creates_work() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    let dir = apps.path().join("demo");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("app.json"), json!({
        "id":"demo","version":"1","name":{"en":"Demo"},
        "operations":{"get":{"label":{"en":"Get"},"args":[{"name":"key","kind":"name","required":true}],
        "effects":[{"kind":"read","label":{"en":"Read key"},"target_arg":"key","recovery":"not_applicable"}]}}
    }).to_string()).unwrap();
    std::fs::write(
        dir.join("main.py"),
        "raise AssertionError('no preview execution')\n",
    )
    .unwrap();
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::App, "demo");
    let activity =
        super::super::activities::create(json!({"title":"Goal","goal":"Read a key"}), &peer(1000))
            .unwrap();
    let params = json!({"id":activity["id"],"app_id":"demo","operation":"get","args":["key"]});
    let result = for_activity(params.clone(), &peer(1000)).unwrap();
    assert_eq!(result["effects"][0]["requested_targets"], json!(["key"]));
    assert_eq!(result["executed"], false);
    assert_eq!(result["authorization_checked"], false);
    for uid in [0, 2000] {
        assert!(for_activity(params.clone(), &peer(uid)).is_err());
    }
    assert!(for_activity(params, &ClientIdentity::unknown()).is_err());
    assert!(!crate::paths::agent_jobs_dir().exists());
}
