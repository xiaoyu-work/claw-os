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
fn routes_are_owner_scoped_cas_metadata_with_closed_audit_bodies() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let activity = super::super::activities::create(
        json!({"title":"Release","goal":"Prepare it"}),
        &peer(1000),
    )
    .unwrap();
    let id = activity["id"].as_str().unwrap();
    let absent = get(json!({"id":id}), &peer(1000)).unwrap();
    assert!(absent["scheduling_policy"].is_null());
    let created = set(
        json!({"id":id,"priority":"foreground"}),
        &peer(1000),
    )
    .unwrap();
    assert_eq!(created["revision"], 1);
    assert_eq!(created["priority"], "foreground");
    for uid in [0, 2000] {
        assert!(get(json!({"id":id}), &peer(uid)).is_err());
        assert!(set(
            json!({"id":id,"expected_revision":1,"priority":"background"}),
            &peer(uid)
        )
        .is_err());
    }
    let updated = set(
        json!({"id":id,"expected_revision":1,"priority":"background"}),
        &peer(1000),
    )
    .unwrap();
    assert_eq!(updated["revision"], 2);

    for (command, valid) in [
        (
            crate::clawd::routes::Command::ActivitySchedulingPolicyGet,
            json!({"id":id}),
        ),
        (
            crate::clawd::routes::Command::ActivitySchedulingPolicySet,
            json!({"id":id,"expected_revision":2,"priority":"standard"}),
        ),
    ] {
        let route = command.route();
        assert_eq!(route.access, crate::clawd::routes::Access::User);
        (route.decode)(valid.clone()).unwrap();
        for field in ["owner_uid", "caps", "grant", "preempt", "cancel_running"] {
            let mut forged = valid.clone();
            forged[field] = json!(true);
            assert!((route.decode)(forged).is_err(), "{field}");
        }
        assert!(route.audit_fields.iter().any(|(field, _)| *field == "id"));
        assert!(!route
            .audit_fields
            .iter()
            .any(|(field, _)| *field == "owner_uid"));
    }
}
