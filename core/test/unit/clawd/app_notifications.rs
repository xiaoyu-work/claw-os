use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use crate::clawd::routes::Command;
use crate::notifications::{
    DeliveryChannel, DeliveryState, NotificationState, SqliteNotificationService,
};
use std::time::Duration;

mod worker {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_notifications/worker.rs"
    ));
}

fn decision(app: &str, allowed: bool) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let session = format!("notify-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(Some(app.into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(if allowed {
                vec![Cap::unscoped(Verb::UI_NOTIFY)]
            } else {
                vec![]
            }),
            lifetime: Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.notification.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.notification.control",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

fn request() -> Value {
    json!({
        "action":"post", "summary":"Ready", "body":"", "app_name":"Label only",
        "icon":"com.clawos.Notifications", "expire_ms":0, "transient":true,
        "dedupe_key":"repeat",
    })
}

fn intent(value: Value) -> Intent {
    validate(serde_json::from_value(value).unwrap()).unwrap()
}

fn until() -> u64 {
    notifications::now_ms() as u64 + 5000
}

#[test]
fn app_notifications_are_durable_source_owner_bound_and_not_acknowledged_on_delivery() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let uid = unsafe { libc::geteuid() };
    let authority = decision(APP, true);
    let path = root.path().join("fixture.db");
    let service = notifications::open(&path).unwrap();
    let first = apply(&service, intent(request()), uid, &authority, until()).unwrap();
    let id = first["id"].as_str().unwrap();
    let record = service.get(uid, id).unwrap();
    assert_eq!(record.source, SOURCE);
    assert_eq!(record.owner_uid, uid);
    assert_eq!(record.session_id.as_deref(), authority.session_id());
    assert_eq!(record.task_id, None);
    assert_eq!(record.body, "");
    assert_eq!(record.presentation.as_ref().unwrap().app_name, "Label only");
    assert_eq!(record.presentation.as_ref().unwrap().expire_ms, 0);
    assert!(record.presentation.as_ref().unwrap().transient);
    assert_eq!(
        record.expires_at_ms, None,
        "popup timeout is not record expiry"
    );
    assert_eq!(
        apply(&service, intent(request()), uid, &authority, until()).unwrap(),
        first
    );
    assert_eq!(service.get(uid, id).unwrap().occurrences, 2);
    let claim = service
        .claim_deliveries(Some(uid), DeliveryChannel::Desktop, 10, 5000)
        .unwrap();
    assert_eq!(claim.len(), 1);
    let delivered = service
        .complete_delivery(
            uid,
            id,
            DeliveryChannel::Desktop,
            notifications::DeliveryResult::Delivered,
        )
        .unwrap();
    assert_eq!(delivered.state, NotificationState::Unread);
    assert!(delivered.acknowledged_at_ms.is_none());
    let close = json!({"action":"close","id":id});
    assert!(apply(
        &service,
        intent(close.clone()),
        uid + 1,
        &authority,
        until()
    )
    .is_err());
    assert!(apply(
        &service,
        intent(close.clone()),
        uid,
        &decision("notify", true),
        until()
    )
    .is_err());
    assert!(apply(
        &service,
        intent(close.clone()),
        uid,
        &decision(APP, false),
        until()
    )
    .is_err());
    let closed = apply(&service, intent(close), uid, &authority, until()).unwrap();
    assert_eq!(closed["state"], "dismissed");
    drop(service);
    let reopened = notifications::open(path).unwrap();
    assert_eq!(
        reopened.get(uid, id).unwrap().state,
        NotificationState::Dismissed
    );
    assert!(reopened
        .claim_deliveries(Some(uid), DeliveryChannel::Desktop, 10, 5000)
        .unwrap()
        .is_empty());
    let other = reopened
        .publish(
            uid,
            NotificationDraft::new("agent", "test", Severity::Info, "Other", "Other")
                .dedupe("repeat"),
        )
        .unwrap();
    assert_ne!(other.id, id);
    assert!(apply(
        &reopened,
        intent(json!({"action":"close","id":other.id})),
        uid,
        &authority,
        until()
    )
    .is_err());
    assert_eq!(
        reopened.get(uid, &other.id).unwrap().state,
        NotificationState::Unread
    );
}

#[test]
fn app_notification_contract_is_closed_and_validated_before_effects() {
    let command = Command::SystemNotificationControl;
    let valid = json!({"session":"s","deadline_unix_ms":123,"request":request()});
    (command.route().decode)(valid.clone()).unwrap();
    for (field, value) in [
        ("owner_uid", json!(0)),
        ("app_id", json!(APP)),
        ("source", json!(SOURCE)),
        ("task_id", json!("forged")),
        ("session_id", json!("forged")),
        ("actions", json!([])),
        ("target", json!("/run/user/1000/bus")),
    ] {
        let mut bad = valid.clone();
        bad["request"][field] = value.clone();
        assert!((command.route().decode)(bad).is_err(), "{field}");
        let mut bad = valid.clone();
        bad[field] = value;
        assert!((command.route().decode)(bad).is_err(), "{field}");
    }
    for (field, value) in [
        ("body", json!(false)),
        ("expire_ms", json!("-1")),
        ("expire_ms", json!(2147483648_u64)),
        ("transient", json!("true")),
        ("summary", json!("x".repeat(961))),
    ] {
        let mut bad = valid.clone();
        bad["request"][field] = value;
        assert!((command.route().decode)(bad).is_err(), "{field}");
    }
    for (field, value) in [
        ("summary", json!(" ")),
        ("summary", json!("界".repeat(241))),
        ("body", json!("x".repeat(4001))),
        ("expire_ms", json!(-2)),
        ("icon", json!("/private/icon.svg")),
        ("icon", json!("file:///private/icon.svg")),
        ("app_name", json!("name\nspoof")),
        ("dedupe_key", json!("")),
    ] {
        let mut bad = request();
        bad[field] = value;
        assert!(
            validate(serde_json::from_value(bad).unwrap()).is_err(),
            "{field}"
        );
    }
    for id in [
        json!(42),
        json!("42"),
        json!("notif-x"),
        json!("../anything"),
    ] {
        let decoded = serde_json::from_value(json!({"action":"close","id":id}));
        assert!(decoded
            .map_err(|_| ())
            .and_then(|value| validate(value).map_err(|_| ()))
            .is_err());
    }
    assert!(deadline(0).is_err());
    assert_eq!(
        command
            .route()
            .audit_fields
            .iter()
            .map(|(field, _)| *field)
            .collect::<Vec<_>>(),
        ["session", "deadline_unix_ms", "request"]
    );
}

#[test]
fn app_notifications_follow_dnd_and_all_channel_policy() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let uid = unsafe { libc::geteuid() };
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
                ..Default::default()
            },
        )
        .unwrap();
    let result = apply(
        &service,
        intent(request()),
        uid,
        &decision(APP, true),
        until(),
    )
    .unwrap();
    let record = service.get(uid, result["id"].as_str().unwrap()).unwrap();
    assert_eq!(record.deliveries.len(), 3);
    for delivery in &record.deliveries {
        assert_eq!(
            delivery.state,
            if delivery.channel == DeliveryChannel::Web {
                DeliveryState::Queued
            } else {
                DeliveryState::Suppressed
            }
        );
    }
    service
        .mutate(uid, &record.id, NotificationMutation::Acknowledge)
        .unwrap();
    assert!(service
        .claim_deliveries(Some(uid), DeliveryChannel::Web, 10, 5000)
        .unwrap()
        .is_empty());
}

#[test]
fn unavailable_notification_service_is_not_success_or_an_in_memory_fallback() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let file = root.path().join("not-a-directory");
    std::fs::write(&file, b"preserve").unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    std::fs::create_dir_all(crate::paths::notifications_db_path()).unwrap();
    let uid = unsafe { libc::geteuid() };
    let authority = decision(APP, true);
    let client = ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(uid),
        gid: Some(unsafe { libc::getegid() }),
        execution_uid: None,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        attended_local: false,
        extension_host: None,
    };
    let error = control(
        json!({
            "session":authority.session_id(),"deadline_unix_ms":until(),"request":request(),
        }),
        &client,
        &authority,
    )
    .unwrap_err();
    assert_eq!(
        error.kind,
        super::super::protocol::BrokerErrorKind::Unavailable
    );
    assert_eq!(std::fs::read(file).unwrap(), b"preserve");
}
