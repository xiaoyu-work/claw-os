use super::*;

#[tokio::test]
async fn session_views_merge_durable_and_legacy_owner_memory() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let owner_uid = unsafe { libc::geteuid() } as u32;
    let durable_id = crate::session::SessionId::generate().into_string();
    let legacy_id = uuid::Uuid::new_v4().to_string();

    let durable = crate::agent::memory::sqlite_fts::MemoryDb::open(
        crate::paths::clawd_user_memory_db_path(owner_uid),
    )
    .unwrap();
    durable
        .record_message_at(&durable_id, "user", "durable question", 20)
        .unwrap();
    let legacy =
        crate::agent::memory::sqlite_fts::MemoryDb::open(crate::paths::agent_memory_db_path())
            .unwrap();
    legacy
        .record_message_at(&legacy_id, "user", "legacy question", 10)
        .unwrap();
    drop((durable, legacy));

    let state = AppState::new(crate::config::AgentConfig::default(), owner_uid);
    let Json(list) = list(State(state.clone())).await.unwrap();
    let sessions = list["sessions"].as_array().unwrap();
    assert!(sessions.iter().any(|entry| entry["id"] == durable_id));
    assert!(sessions.iter().any(|entry| entry["id"] == legacy_id));

    let Json(history) = history(State(state), Path(legacy_id)).await.unwrap();
    assert_eq!(history["messages"][0]["text"], "legacy question");
}
