use super::*;
use crate::test_env::TestEnvVarGuard;

struct Directory(std::path::PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::path::PathBuf::from(format!(
            ".activity-continuity-broker-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn peer(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(uid),
        gid: Some(uid),
        start_time_ticks: Some(1),
        ..ClientIdentity::unknown()
    }
}

#[test]
fn routes_export_and_import_through_one_owner_scoped_backend() {
    let dir = Directory::new();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", &dir.0);
    let created = super::super::activities::create(
        serde_json::json!({
            "title":"Release",
            "goal":"Publish the release",
            "resources":[
                {"label":"Local","reference":"/home/user/private.txt"},
                {"label":"Status","reference":"app://kv/entry?id=release.status"}
            ]
        }),
        &peer(1000),
    )
    .unwrap();
    let id = created["id"].as_str().unwrap();
    let document = export(serde_json::json!({"id":id}), &peer(1000)).unwrap();
    assert_eq!(document["kind"], crate::activities::CONTINUITY_KIND);
    assert_eq!(document["references"].as_array().unwrap().len(), 1);
    assert!(export(serde_json::json!({"id":id}), &peer(2000)).is_err());
    assert!(export(serde_json::json!({"id":id}), &peer(0)).is_err());

    let imported = import(
        serde_json::json!({
            "placement":"local",
            "document":serde_json::to_string(&document).unwrap()
        }),
        &peer(2000),
    )
    .unwrap();
    assert_eq!(imported["activity"]["state"], "paused");
    assert_ne!(imported["activity"]["id"], id);
    assert_eq!(
        imported["continuity_id"],
        document["lineage"]["id"].as_str().unwrap()
    );
    assert!(import(
        serde_json::json!({
            "placement":"local",
            "document":serde_json::to_string(&document).unwrap()
        }),
        &peer(2000),
    )
    .is_err());
}

#[test]
fn continuity_audit_records_only_bounded_metadata_and_result() {
    let dir = Directory::new();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", &dir.0);
    let private = "private-continuity-goal-and-object";
    let created = super::super::activities::create(
        serde_json::json!({
            "title":"Private",
            "goal":private,
            "resources":[{"label":"Private object","reference":
                "app://kv/entry?id=private-continuity-goal-and-object"}]
        }),
        &peer(1000),
    )
    .unwrap();
    let document = export(
        serde_json::json!({"id":created["id"].as_str().unwrap()}),
        &peer(1000),
    )
    .unwrap();
    import(
        serde_json::json!({
            "placement":"local",
            "document":serde_json::to_string(&document).unwrap()
        }),
        &peer(2000),
    )
    .unwrap();
    let audit = std::fs::read_to_string(dir.0.join("clawd").join("audit.jsonl")).unwrap();
    assert!(!audit.contains(private));
    let records: Vec<serde_json::Value> = audit
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["event"], "clawd.activity.continuity");
    assert_eq!(records[0]["action"], "export");
    assert_eq!(records[0]["schema_version"], 1);
    assert_eq!(records[0]["reference_count"], 1);
    assert_eq!(records[0]["result"], "exported");
    assert_eq!(records[1]["placement"], "local");
    assert_eq!(records[1]["result"], "imported");
    assert!(records[0].get("document").is_none());
    assert!(records[0].get("intent").is_none());
}
