use super::*;
use crate::test_env::TestEnvVarGuard;

fn peer(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(uid),
        gid: Some(uid),
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        ..ClientIdentity::unknown()
    }
}

#[test]
fn policy_controls_are_owner_scoped_cas_constraints_not_authority() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let activity = super::super::activities::create(
        json!({"title":"Prepare release","goal":"Draft, do not publish"}),
        &peer(1000),
    )
    .unwrap();
    let id = activity["id"].as_str().unwrap();
    let absent = get(json!({"id":id}), &peer(1000)).unwrap();
    assert_eq!(absent["schema"], 1);
    assert_eq!(absent["activity_id"], id);
    assert!(absent["capability_policy"].is_null());
    let request = json!({"id":id,"policy":{"rules":[
        {"verb":"fs.delete","mode":"deny","scopes":[]},
        {"verb":"fs.write","mode":"require_approval","scopes":[{"kind":"path","value":"/workspace/**"}]}
    ]}});
    let policy = set(request.clone(), &peer(1000)).unwrap();
    assert_eq!(policy["revision"], 1);
    assert_eq!(policy["owner_uid"], 1000);
    assert_eq!(policy["enabled"], true);
    assert!(policy.get("caps").is_none());
    assert!(set(request.clone(), &peer(1000)).is_err());
    for uid in [0, 2000] {
        assert!(get(json!({"id":id}), &peer(uid)).is_err());
        assert!(set(request.clone(), &peer(uid)).is_err());
        assert!(enabled(
            json!({"id":id,"expected_revision":1,"enabled":false}),
            &peer(uid)
        )
        .is_err());
    }
    let disabled = enabled(
        json!({"id":id,"expected_revision":1,"enabled":false}),
        &peer(1000),
    )
    .unwrap();
    assert_eq!(disabled["revision"], 2);
    assert_eq!(disabled["enabled"], false);
    let edited = set(
        json!({"id":id,"expected_revision":2,"policy":{"rules":[]}}),
        &peer(1000),
    )
    .unwrap();
    assert_eq!(edited["revision"], 3);
    assert_eq!(
        edited["enabled"], false,
        "editing must not implicitly enable"
    );
    assert_eq!(
        get(json!({"id":id}), &peer(1000)).unwrap()["capability_policy"],
        edited
    );
    assert!(!crate::paths::agent_jobs_dir().exists());
    assert_eq!(
        activities::open_default()
            .unwrap()
            .get(1000, id)
            .unwrap()
            .state,
        activities::ActivityState::Active,
    );
}
