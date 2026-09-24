use super::*;

fn client(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: None,
        uid: Some(uid),
        gid: Some(uid),
        execution_uid: None,
        start_time_ticks: None,
        attended_local: true,
        extension_host: None,
    }
}

#[test]
fn learned_memory_reset_requires_explicit_confirmation() {
    let error = reset(json!({ "confirm": false }), &client(1000)).unwrap_err();
    assert_eq!(error, "memory reset requires confirm=true");
}

#[test]
fn learned_memory_reset_refuses_root_owner() {
    let error = reset(json!({ "confirm": true }), &client(0)).unwrap_err();
    assert_eq!(error, ROOT_OWNER_REFUSAL);
}

#[test]
fn learned_memory_reset_refuses_an_active_owner_task() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let owner_uid = 1000;
    Store::open_default()
        .unwrap()
        .submit(
            "pending task".into(),
            None,
            None,
            Some(owner_uid),
            Some("/home/test".into()),
        )
        .unwrap();

    let error = refuse_active_tasks(owner_uid).unwrap_err();
    assert!(error.contains("active Agent task"));
}
