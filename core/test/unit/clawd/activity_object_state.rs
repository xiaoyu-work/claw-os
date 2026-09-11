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

fn entry(reference: &str) -> Value {
    json!({
        "id":uuid::Uuid::new_v4().to_string(),"reference":reference,
        "content":{"kind":"user_statement","text":"Waiting for review"},
    })
}

#[test]
fn object_state_is_owner_scoped_inert_and_does_not_complete_goals() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", root.path().join("no-apps"));
    let reference = "app://demo/document?id=release";
    let activity = super::super::activities::create(
        json!({
            "title":"Release","goal":"Review",
            "resources":[{"label":"Draft","reference":reference}],
        }),
        &peer(1000),
    )
    .unwrap();
    let id = activity["id"].as_str().unwrap();
    let request = json!({"id":id,"entry":entry(reference)});
    let saved = record(request.clone(), &peer(1000)).unwrap();
    assert_eq!(saved["source"], "caller_reported");
    assert_eq!(saved["owner_uid"], 1000);
    assert_eq!(saved["validity"], "unknown");
    assert_eq!(saved["draft"]["content"]["kind"], "user_statement");
    assert_eq!(record(request.clone(), &peer(1000)).unwrap(), saved);
    let listed = list(
        json!({"id":id,"reference":reference,"limit":100}),
        &peer(1000),
    )
    .unwrap();
    assert_eq!(
        listed,
        json!({"schema":1,"activity_id":id,"entries":[saved]})
    );
    for uid in [0, 2000] {
        assert!(record(request.clone(), &peer(uid)).is_err());
        assert!(list(json!({"id":id}), &peer(uid)).is_err());
    }
    for limit in [0, 101] {
        assert!(list(json!({"id":id,"limit":limit}), &peer(1000)).is_err());
    }
    assert!(list(json!({"id":id,"reference":"app:bad"}), &peer(1000)).is_err());
    assert!(record(
        json!({"id":id,"owner_uid":1000,"entry":entry(reference)}),
        &peer(1000)
    )
    .is_err());
    let current = activities::open_default().unwrap().get(1000, id).unwrap();
    assert_eq!(current.state, activities::ActivityState::Active);
    assert_eq!(current.updated_at, activity["updated_at"]);
    assert!(!root.path().join("no-apps").exists());
    assert!(!crate::paths::agent_jobs_dir().exists());
}
