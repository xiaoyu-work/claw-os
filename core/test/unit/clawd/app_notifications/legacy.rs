use super::*;

fn send(message: &str, urgent: bool) -> Intent {
    intent(json!({"action":"send","message":message,"urgent":urgent}))
}

fn list() -> Intent {
    intent(json!({"action":"list","limit":1}))
}

fn reader() -> Decision {
    decision_with_caps(
        NOTIFY_APP,
        vec![Cap::new(Verb::DATA_INBOX_READ, Scope::Wild)],
    )
}

fn client() -> ClientIdentity {
    ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(unsafe { libc::geteuid() }),
        gid: Some(unsafe { libc::getegid() }),
        execution_uid: None,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        attended_local: false,
        extension_host: None,
    }
}

#[test]
fn notify_send_list_preserve_projection_restart_order_total_and_source_privacy() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let uid = unsafe { libc::geteuid() };
    let writer = decision(NOTIFY_APP, true);
    let path = root.path().join("fixture.db");
    let service = notifications::open(&path).unwrap();
    let first = apply(&service, send("First", false), uid, &writer, until()).unwrap();
    let second = apply(&service, send("Second\n界", true), uid, &writer, until()).unwrap();
    assert_eq!(first.as_object().unwrap().len(), 4);
    assert_eq!(second["message"], "Second\n界");
    assert_eq!(second["urgent"], true);
    let id = second["id"].as_str().unwrap();
    assert!(id.starts_with("notif-"));
    assert_eq!(id.len(), 38);
    assert_eq!(second["timestamp"].as_str().unwrap().len(), 19);
    let record = service.get(uid, id).unwrap();
    assert_eq!(record.source, NOTIFY_SOURCE);
    assert_eq!(record.owner_uid, uid);
    assert_eq!(record.session_id.as_deref(), writer.session_id());
    assert_eq!(record.task_id, None);
    assert_eq!(record.severity, Severity::Warning);
    assert_eq!(
        record.delivery_policy,
        notifications::DeliveryPolicy::Immediate
    );
    assert_eq!(record.presentation.as_ref().unwrap().expire_ms, -1);
    assert!(!record.presentation.as_ref().unwrap().transient);
    assert_eq!(record.expires_at_ms, None);
    service
        .publish(
            uid + 1,
            NotificationDraft::new(
                NOTIFY_SOURCE,
                "app.notification",
                Severity::Info,
                "Foreign owner",
                "Secret",
            ),
        )
        .unwrap();
    service
        .publish(
            uid,
            NotificationDraft::new(
                "agent",
                "task",
                Severity::Info,
                "Foreign source",
                "Secret task",
            ),
        )
        .unwrap();
    apply(
        &service,
        intent(request()),
        uid,
        &decision(APP, true),
        until(),
    )
    .unwrap();
    let listed = apply(&service, list(), uid, &reader(), until()).unwrap();
    assert_eq!(listed["total"], 2);
    assert_eq!(listed["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(listed["notifications"][0]["id"], id);
    assert_eq!(listed["notifications"][0]["read"], false);
    assert_eq!(listed["notifications"][0]["state"], "unread");
    assert_eq!(listed["notifications"][0].as_object().unwrap().len(), 6);
    service
        .mutate(
            uid,
            first["id"].as_str().unwrap(),
            NotificationMutation::Read,
        )
        .unwrap();
    assert_eq!(
        apply(&service, list(), uid, &reader(), until()).unwrap(),
        listed,
        "reading an older record must not reorder publication history"
    );
    service
        .complete_delivery(
            uid,
            id,
            DeliveryChannel::Desktop,
            notifications::DeliveryResult::Delivered,
        )
        .unwrap();
    assert_eq!(
        apply(&service, list(), uid, &reader(), until()).unwrap(),
        listed,
        "delivery must not acknowledge or mark the record read"
    );
    for (mutation, state) in [
        (NotificationMutation::Read, "read"),
        (NotificationMutation::Acknowledge, "acknowledged"),
        (NotificationMutation::Dismiss, "dismissed"),
    ] {
        service.mutate(uid, id, mutation).unwrap();
        let page = apply(&service, list(), uid, &reader(), until()).unwrap();
        assert_eq!(page["total"], 2);
        assert_eq!(page["notifications"][0]["state"], state);
        assert_eq!(page["notifications"][0]["read"], true);
    }
    assert!(apply(
        &service,
        intent(json!({"action":"close","id":id})),
        uid,
        &decision(APP, true),
        until()
    )
    .is_err());
    drop(service);
    let reopened = notifications::open(path).unwrap();
    let page = apply(&reopened, list(), uid, &reader(), until()).unwrap();
    assert_eq!(page["total"], 2);
    assert_eq!(page["notifications"][0]["id"], id);
    assert_eq!(page["notifications"][0]["state"], "dismissed");
}

#[test]
fn notification_action_app_and_capability_matrix_is_exact() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let uid = unsafe { libc::geteuid() };
    let service = SqliteNotificationService::open_in_memory().unwrap();
    for app in [NOTIFY_APP, APP, "unrelated"] {
        for verb in [Verb::UI_NOTIFY, Verb::DATA_INBOX_READ, Verb::SYS_OBSERVE] {
            let authority = decision_with_caps(app, vec![Cap::new(verb, Scope::Wild)]);
            assert_eq!(
                apply(&service, send("Test", false), uid, &authority, until()).is_ok(),
                app == NOTIFY_APP && verb == Verb::UI_NOTIFY,
                "{app} {verb}"
            );
            assert_eq!(
                apply(&service, list(), uid, &authority, until()).is_ok(),
                app == NOTIFY_APP && verb == Verb::DATA_INBOX_READ,
                "{app} {verb}"
            );
            assert_eq!(
                apply(&service, intent(request()), uid, &authority, until()).is_ok(),
                app == APP && verb == Verb::UI_NOTIFY,
                "{app} {verb}"
            );
        }
    }
    assert!(apply(&service, list(), uid + 1, &reader(), until()).is_err());
    let named = decision_with_caps(
        NOTIFY_APP,
        vec![Cap::new(Verb::DATA_INBOX_READ, Scope::name("unrelated"))],
    );
    assert!(apply(&service, list(), uid, &named, until()).is_err());
}

#[test]
fn notify_validates_closed_bodies_before_authority_storage_or_publication() {
    let command = Command::SystemNotificationControl;
    for action in [
        json!({"action":"send","message":"Hi","urgent":false}),
        json!({"action":"list","limit":20}),
    ] {
        let valid = json!({"session":"s","deadline_unix_ms":123,"request":action});
        (command.route().decode)(valid.clone()).unwrap();
        for field in [
            "owner_uid",
            "source",
            "app",
            "app_name",
            "session",
            "session_id",
            "task_id",
            "confirm",
            "id",
            "icon",
            "dedupe_key",
        ] {
            let mut forged = valid.clone();
            forged["request"][field] = json!("forged");
            assert!((command.route().decode)(forged).is_err(), "{field}");
        }
    }
    for bad in [
        json!({"action":"send","message":"","urgent":false}),
        json!({"action":"send","message":" \n\t","urgent":false}),
        json!({"action":"send","message":"界".repeat(4001),"urgent":false}),
        json!({"action":"send","message":"hello\u{0001}","urgent":false}),
        json!({"action":"send","message":"hello","urgent":1}),
        json!({"action":"send","message":"hello","urgent":"true"}),
        json!({"action":"list","limit":0}),
        json!({"action":"list","limit":101}),
        json!({"action":"list","limit":true}),
        json!({"action":"list","limit":1.5}),
    ] {
        assert!(serde_json::from_value(bad)
            .map_err(|_| ())
            .and_then(|request| validate(request).map_err(|_| ()))
            .is_err());
    }
    assert!(validate(
        serde_json::from_value(json!({"action":"send","message":"😀".repeat(4000),"urgent":true}))
            .unwrap()
    )
    .is_ok());
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    std::fs::create_dir_all(crate::paths::notifications_db_path()).unwrap();
    let uid = unsafe { libc::geteuid() };
    let client = client();
    let unauthorized = decision(NOTIFY_APP, false);
    let error = control(
        json!({"session":unauthorized.session_id(),
        "deadline_unix_ms":until(),"request":{"action":"send","message":"","urgent":false}}),
        &client,
        &unauthorized,
    )
    .unwrap_err();
    assert_eq!(
        error.kind,
        super::super::super::protocol::BrokerErrorKind::Execution
    );
    assert!(error.message.contains("message"));
    assert!(apply(
        &SqliteNotificationService::open_in_memory().unwrap(),
        send("Late", false),
        uid,
        &decision(NOTIFY_APP, true),
        0
    )
    .is_err());
}

#[test]
fn notify_urgent_never_bypasses_dnd_and_uses_all_existing_delivery_policies() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let uid = unsafe { libc::geteuid() };
    for urgent in [false, true] {
        let service = SqliteNotificationService::open_in_memory().unwrap();
        service
            .set_preferences(
                uid,
                notifications::NotificationPreferences {
                    ntfy_enabled: true,
                    ntfy_topic: Some("fixture".into()),
                    ntfy_min_severity: Severity::Info,
                    dnd_start_minute_utc: Some(0),
                    dnd_end_minute_utc: Some(0),
                    critical_bypasses_dnd: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let sent = apply(
            &service,
            send("DND", urgent),
            uid,
            &decision(NOTIFY_APP, true),
            until(),
        )
        .unwrap();
        let row = service.get(uid, sent["id"].as_str().unwrap()).unwrap();
        assert_eq!(row.deliveries.len(), 3);
        for delivery in row.deliveries {
            assert_eq!(
                delivery.state,
                if delivery.channel == DeliveryChannel::Web {
                    DeliveryState::Queued
                } else {
                    DeliveryState::Suppressed
                }
            );
        }
    }
}

#[test]
fn notify_unavailable_service_never_reads_or_replays_legacy_history() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let file = root.path().join("notifications.json");
    std::fs::write(&file, b"not-json: historical bytes").unwrap();
    let before = std::fs::metadata(&file).unwrap();
    std::fs::create_dir_all(crate::paths::notifications_db_path()).unwrap();
    let client = client();
    for (request, authority) in [
        (
            json!({"action":"send","message":"New","urgent":false}),
            decision(NOTIFY_APP, true),
        ),
        (json!({"action":"list","limit":20}), reader()),
    ] {
        let error = control(
            json!({"session":authority.session_id(),"deadline_unix_ms":until(),
            "request":request}),
            &client,
            &authority,
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            super::super::super::protocol::BrokerErrorKind::Unavailable
        );
    }
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before.modified().unwrap()
    );
    assert_eq!(std::fs::read(file).unwrap(), b"not-json: historical bytes");
}
