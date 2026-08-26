use super::*;
use crate::test_env::{lock_env, TestEnvVarGuard};

#[test]
fn task_session_reuse_requires_owner_and_refreshes_caps() {
    let _lock = lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let home = temp.path().join("home-owner");
    std::fs::create_dir_all(&home).unwrap();

    let session_id = create_task_session("test", 1001, &home).unwrap();
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open(
        crate::paths::clawd_user_memory_db_path(1001),
    )
    .unwrap();
    db.record_message(&session_id, "user", "hello").unwrap();

    let sid = session_id.parse::<session::SessionId>().unwrap();
    session::set_caps(&sid, &crate::caps::CapSet::new()).unwrap();
    prepare_task_session(&session_id, 1001, &home).unwrap();
    let refreshed = session::get_caps(&sid).unwrap();
    assert!(refreshed.covers(&crate::caps::Cap::new(
        crate::caps::Verb::NET_DIAL,
        crate::caps::Scope::host("example.com")
    )));

    let error = prepare_task_session(&session_id, 1002, &home).unwrap_err();
    assert!(error.contains("not owned"));
}
