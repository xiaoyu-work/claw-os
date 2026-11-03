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
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path().as_os_str());
    let owner = ClientIdentity {
        pid: Some(10),
        uid: Some(1000),
        gid: Some(1000),
        execution_uid: None,
        start_time_ticks: Some(1),
        attended_local: false,
        extension_host: None,
    };
    let other = ClientIdentity {
        pid: Some(11),
        uid: Some(1001),
        gid: Some(1001),
        execution_uid: None,
        start_time_ticks: Some(1),
        attended_local: false,
        extension_host: None,
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
    let foreign = publish(json!({
        "source": "app.test", "kind": "test.foreign", "severity": "info",
        "title": "Foreign", "body": "Another owner's notification",
    }), &other).unwrap();
    assert_eq!(mark_all_read(&owner).unwrap()["updated"], 1);
    assert_eq!(mark_all_read(&owner).unwrap()["updated"], 0);
    let service = notifications::open_default().unwrap();
    assert_eq!(service.get(1000, created["id"].as_str().unwrap()).unwrap().state,
        crate::notifications::NotificationState::Read);
    assert_eq!(service.get(1001, foreign["id"].as_str().unwrap()).unwrap().state,
        crate::notifications::NotificationState::Unread);
    assert!(mark_all_read(&ClientIdentity::unknown()).is_err());

    let sid = crate::session::create("notification view fixture").unwrap();
    crate::session::update_meta(&sid, |meta| {
        meta.owner_uid = Some(1000);
        meta.creator_runtime = Some("clawd".into());
    }).unwrap();
    let before = crate::session::get_meta(&sid).unwrap();
    let viewer_id = uuid::Uuid::new_v4().to_string();
    let viewing = json!({
        "viewer": { "id": viewer_id, "revision": 1, "visible": true },
        "session_id": sid.to_string(),
    });
    assert!(view(viewing.clone(), &other).is_err());
    view(viewing.clone(), &owner).unwrap();
    assert!(crate::notifications::conversation_is_viewed(1000, &sid.to_string()).unwrap());
    assert!(!crate::notifications::conversation_is_viewed(1001, &sid.to_string()).unwrap());
    let after = crate::session::get_meta(&sid).unwrap();
    assert_eq!(before.client, after.client);
    assert_eq!(before.origin, after.origin);
    let mut forged = viewing;
    forged["owner_uid"] = json!(1000);
    assert!(view(forged, &other).is_err());
    view(json!({
        "viewer": { "id": viewer_id, "revision": 2, "visible": false },
    }), &owner).unwrap();
    assert!(!crate::notifications::conversation_is_viewed(1000, &sid.to_string()).unwrap());
}

#[test]
fn unprivileged_publishers_cannot_spoof_system_sources() {
    let peer = ClientIdentity {
        pid: Some(10),
        uid: Some(1000),
        gid: Some(1000),
        execution_uid: None,
        start_time_ticks: Some(1),
        attended_local: false,
        extension_host: None,
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
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path().as_os_str());
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
