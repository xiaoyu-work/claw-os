use super::*;
use crate::activities::{ReceiptOutcome, ReceiptSource};
use crate::test_env::TestEnvVarGuard;

fn peer(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(uid),
        gid: Some(uid),
        start_time_ticks: Some(1),
    }
}

fn app(root: &std::path::Path) -> crate::apps::App {
    let dir = root.join("demo");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("app.json"), json!({
        "id":"demo","version":"1","name":{"en":"Demo"},
        "operations":{"get":{"label":{"en":"Get"},"args":[{"name":"key","kind":"name","required":true}],
        "effects":[{"kind":"read","label":{"en":"Read key"},"target_arg":"key","recovery":"not_applicable"}]}}
    }).to_string()).unwrap();
    std::fs::write(
        dir.join("main.py"),
        "raise AssertionError('recording never executes')\n",
    )
    .unwrap();
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::App, "demo");
    crate::apps::find_verified(root, "demo").unwrap()
}

#[test]
fn receipt_recording_derives_source_and_owner_without_claiming_execution() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    let app = app(apps.path());
    let activity =
        super::super::activities::create(json!({"title":"Goal","goal":"Read key"}), &peer(1000))
            .unwrap();
    let report = crate::operations::receipts::capture(
        uuid::Uuid::new_v4().to_string(),
        "demo".into(),
        "get".into(),
        app.require_verified().unwrap().content_digest().into(),
        Ok(Some(r#"{"value":"ready"}"#.into())),
    );
    let request = json!({"id":activity["id"],"report":report});
    let saved = record(request.clone(), &peer(1000)).unwrap();
    assert_eq!(saved["source"], "caller_reported");
    assert_eq!(saved["owner_uid"], 1000);
    assert_eq!(saved["report"]["outcome"], "returned");
    assert!(saved["declaration_error"].is_null());
    assert_eq!(saved["declaration"]["effects"][0]["kind"], "read");
    assert_eq!(record(request.clone(), &peer(1000)).unwrap(), saved);
    assert!(record(request.clone(), &peer(0)).is_err());
    assert!(record(request, &peer(2000)).is_err());
    let listed = list(json!({"id":activity["id"]}), &peer(1000)).unwrap();
    assert_eq!(listed["receipts"], json!([saved]));
    assert!(list(json!({"id":activity["id"]}), &peer(0)).is_err());
    let current = activities::open_default()
        .unwrap()
        .get(1000, activity["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(current.state, activities::ActivityState::Active);
    assert!(!crate::paths::agent_jobs_dir().exists());
}

#[test]
fn receipt_keeps_unmatched_reports_and_redacts_text_without_authenticating_them() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    let _app = app(apps.path());
    let activity =
        super::super::activities::create(json!({"title":"Goal","goal":"Read key"}), &peer(1000))
            .unwrap();
    let token = format!("ghp_{}", "a".repeat(36));
    let report = ReceiptReport {
        id: uuid::Uuid::new_v4().to_string(),
        app_id: "demo".into(),
        operation: "get".into(),
        package_digest: format!("sha256:{}", "b".repeat(64)),
        outcome: ReceiptOutcome::Indeterminate,
        result: None,
        error: Some(token.clone()),
    };
    let saved = record(json!({"id":activity["id"],"report":report}), &peer(1000)).unwrap();
    assert!(saved["declaration"].is_null());
    assert!(!saved["declaration_error"].as_str().unwrap().is_empty());
    assert!(!saved.to_string().contains(&token));
    assert_eq!(
        saved["source"],
        serde_json::to_value(ReceiptSource::CallerReported).unwrap()
    );
}
