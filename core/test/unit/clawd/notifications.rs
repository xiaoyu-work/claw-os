use super::*;

#[test]
fn retry_backoff_is_bounded() {
    assert_eq!(retry_delay_ms(1), 30_000);
    assert_eq!(retry_delay_ms(2), 60_000);
    assert_eq!(retry_delay_ms(100), 3_600_000);
}

#[test]
fn optional_limit_defaults_and_bounds() {
    assert_eq!(optional_limit(&json!({})).unwrap(), 100);
    assert_eq!(optional_limit(&json!({ "limit": 10 })).unwrap(), 10);
}

#[test]
fn broker_handlers_derive_owner_from_peer_credentials() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data =
        crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path().as_os_str());
    let owner = ClientIdentity {
        pid: Some(10),
        uid: Some(1000),
        gid: Some(1000),
        start_time_ticks: Some(1),
    };
    let other = ClientIdentity {
        pid: Some(11),
        uid: Some(1001),
        gid: Some(1001),
        start_time_ticks: Some(1),
    };

    let created = publish(
        json!({
            "source": "app.test",
            "kind": "test.ready",
            "severity": "info",
            "title": "Ready",
            "body": "The test is ready.",
        }),
        &owner,
    )
    .unwrap();
    assert_eq!(created["owner_uid"], 1000);
    assert_eq!(
        list(json!({ "limit": 10 }), &owner).unwrap()["notifications"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        list(json!({ "limit": 10 }), &other).unwrap()["notifications"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unprivileged_publishers_cannot_spoof_system_sources() {
    let peer = ClientIdentity {
        pid: Some(10),
        uid: Some(1000),
        gid: Some(1000),
        start_time_ticks: Some(1),
    };
    let error = publish(
        json!({
            "source": "heartbeat",
            "kind": "memory_low.critical",
            "severity": "critical",
            "title": "Spoofed",
            "body": "Spoofed",
        }),
        &peer,
    )
    .unwrap_err();
    assert!(error.contains("reserved notification source"));
}

#[test]
fn due_nudges_publish_without_an_agent_turn() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data =
        crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path().as_os_str());
    let path = crate::paths::clawd_user_agent_state_dir(1000).join("nudges.json");
    let nudges = crate::agent::nudge::NudgeStore::new(&path);
    nudges
        .add(crate::agent::nudge::Nudge {
            id: "due-now".to_string(),
            message: "Review the deployment".to_string(),
            due_at_epoch_s: crate::agent::nudge::now_epoch_s().saturating_sub(1),
            repeat_secs: None,
            tag: Some("test".to_string()),
            last_fired_epoch_s: None,
        })
        .unwrap();

    publish_due_nudges();

    let service = crate::notifications::open_default().unwrap();
    let records = service.list(1000, false, 10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, "nudge.due");
    assert!(nudges.list().is_empty());
}
