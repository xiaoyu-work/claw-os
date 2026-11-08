use super::*;

#[test]
fn app_events_deduplicate_same_reports_without_replaying_or_changing_owner() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let record = json!({
        "schema":1,"event":"context.event","id":"stable-id","ts":Utc::now(),
        "source":"app:email","app_id":"email","event_type":"message.received",
        "entity_id":"message","payload":{"object":"app://email/gmail-message?id=message"},
        "client":{"uid":1000,"pid":10},
    });
    let first = append_once(&record).unwrap();
    let mut retried = record.clone();
    retried["client"]["pid"] = json!(20);
    let second = append_once(&retried).unwrap();
    assert_eq!(first["event"], second["event"]);
    assert_eq!(second["deduplicated"], true);
    assert_eq!(
        std::fs::read_to_string(crate::paths::context_events_log_path())
            .unwrap()
            .lines()
            .count(),
        1
    );
    retried["payload"]["object"] = json!("different");
    assert!(append_once(&retried).is_err());
    retried = record;
    retried["client"]["uid"] = json!(2000);
    assert!(append_once(&retried).is_err());
}

#[test]
fn unresolved_event_journal_is_preserved_instead_of_repaired_or_replayed() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let path = crate::paths::context_events_log_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{\"partial\":").unwrap();
    assert!(append_once(&json!({"id":"new"}))
        .unwrap_err()
        .contains("partial append"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "{\"partial\":");
}
