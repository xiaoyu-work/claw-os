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

#[test]
fn owner_scoped_limits_preserve_usage_and_never_create_authority_or_jobs() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let activity = super::super::activities::create(
        json!({"title":"Bounded goal","goal":"Prepare"}),
        &peer(1000),
    )
    .unwrap();
    let id = activity["id"].as_str().unwrap();
    assert!(get(json!({"id":id}), &peer(1000)).unwrap()["execution_limits"].is_null());
    let request = json!({"id":id,"limits":{
        "max_attempts":2,"max_turns_per_attempt":3,"expires_at":"2099-01-01T00:00:00Z",
    }});
    let policy = set(request.clone(), &peer(1000)).unwrap();
    assert_eq!(policy["revision"], 1);
    assert_eq!(policy["used_attempts"], 0);
    assert_eq!(policy["owner_uid"], 1000);
    assert!(policy.get("caps").is_none());
    for uid in [0, 2000] {
        assert!(get(json!({"id":id}), &peer(uid)).is_err());
        assert!(set(request.clone(), &peer(uid)).is_err());
        assert!(enabled(
            json!({"id":id,"expected_revision":1,"enabled":false}),
            &peer(uid)
        )
        .is_err());
    }
    assert!(set(request, &peer(1000)).is_err());
    let disabled = enabled(
        json!({"id":id,"expected_revision":1,"enabled":false}),
        &peer(1000),
    )
    .unwrap();
    assert_eq!(disabled["revision"], 2);
    assert_eq!(disabled["enabled"], false);
    assert!(enabled(
        json!({"id":id,"expected_revision":1,"enabled":true}),
        &peer(1000)
    )
    .is_err());
    assert_eq!(
        activities::open_default()
            .unwrap()
            .get(1000, id)
            .unwrap()
            .state,
        activities::ActivityState::Active
    );
    assert!(!crate::paths::agent_jobs_dir().exists());
}
