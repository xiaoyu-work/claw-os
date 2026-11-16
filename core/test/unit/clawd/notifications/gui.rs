use super::*;
use crate::notifications::{NotificationDraft, Severity, SqliteNotificationService};

fn service() -> (
    tempfile::TempDir,
    crate::test_env::TestEnvVarGuard,
    SqliteNotificationService,
) {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let service = notifications::open(root.path().join("notifications.db")).unwrap();
    (root, data, service)
}

fn publish(service: &dyn NotificationService, owner: u32, key: &str) -> Notification {
    service
        .publish(
            owner,
            NotificationDraft::new(
                "app:independent-sender",
                "fixture",
                Severity::Info,
                "Original popup",
                "<not markup> & text",
            )
            .dedupe(key),
        )
        .unwrap()
}

fn presented(command: &Value) -> Feedback {
    let handle = command["handle"].as_u64().unwrap() as u32;
    Feedback::Presented {
        handle,
        token: serde_json::from_value(command["token"].clone()).unwrap(),
        card: Some(
            serde_json::from_value(json!(Card {
                version: 1,
                id: handle,
                source_label: "Display label".into(),
                title: "Original popup".into(),
                body: vec![],
                activation: Some("default".into()),
                icon: None,
                group_icon: None,
            }
            .to_json()
            .unwrap()))
            .unwrap(),
        ),
    }
}

fn apply(
    state: &mut State,
    event: Feedback,
    owner: u32,
    service: &dyn NotificationService,
) -> Result<(), BrokerError> {
    let card = event.validate()?;
    apply_feedback(state, event, card, owner, service)
}

#[test]
fn private_presentation_contract_rejects_authority_paths_and_unbounded_handles() {
    let request = json!({"session":"fixture","event":{"kind":"closed","handle":1,
        "token":"a".repeat(32),"reason":"dismissed"}});
    serde_json::from_value::<Exchange>(request.clone()).unwrap();
    for field in [
        "owner_uid",
        "app_id",
        "source",
        "id",
        "channel",
        "socket",
        "path",
        "lease_ms",
        "grant",
    ] {
        let mut bad = request.clone();
        bad[field] = json!("forged");
        assert!(serde_json::from_value::<Exchange>(bad).is_err(), "{field}");
        let mut bad = request.clone();
        bad["event"][field] = json!("forged");
        assert!(serde_json::from_value::<Exchange>(bad).is_err(), "{field}");
    }
    for (handle, token) in [
        (0, "a".repeat(32)),
        (1, "A".repeat(32)),
        (1, "a".repeat(31)),
    ] {
        let event: Feedback = serde_json::from_value(json!({"kind":"closed","handle":handle,
            "token":token,"reason":"dismissed"}))
        .unwrap();
        assert!(event.validate().is_err());
    }
    let bad: Feedback = serde_json::from_value(json!({"kind":"presented","handle":1,
        "token":"a".repeat(32),"card":"{\"version\":1,\"id\":1,\"source_label\":\"x\",\"title\":\"x\",\"body\":[],\"activation\":null,\"icon\":null,\"group_icon\":null,\"owner_uid\":0}"})).unwrap();
    assert!(bad.validate().is_err());
}

#[test]
fn display_binding_is_generic_but_requires_this_surface_panel_and_exact_topic_declarations() {
    let panel = json!({"verb":"ui.panel","scope":{"kind":"fixed","scope":{"kind":"wild"}},
        "why":{"en":"Present native popup surfaces."}});
    let topic = json!({"verb":"ipc.subscribe","scope":{"kind":"fixed",
        "scope":{"kind":"name","value":"notifications.presentation"}},"why":{"en":"Present notification data."}});
    let mut value = json!({"id":"independent-renderer","version":"1.0.0","name":{"en":"Independent"},
        "runtime":"binary","entry":"main","desktop":{"exec":"--gui","needs":[panel,topic]}});
    let eligible = |value: Value| {
        let manifest: crate::caps::manifest::Manifest = serde_json::from_value(value).unwrap();
        let selected = manifest.gui_entry(None).unwrap();
        declares_resource(&selected, Verb::UI_PANEL)
            && declares_resource(&selected, Verb::IPC_SUBSCRIBE)
    };
    assert!(eligible(value.clone()));
    for id in ["cosmic-notifications", "notify", "another-signed-renderer"] {
        value["id"] = json!(id);
        assert!(
            eligible(value.clone()),
            "an App name became an entitlement or exclusion"
        );
    }
    for needs in [json!([panel]), json!([topic]), json!([])] {
        let mut bad = value.clone();
        bad["desktop"]["needs"] = needs;
        bad["desktop"]["optional_needs"] = json!([panel, topic]);
        assert!(
            !eligible(bad),
            "another surface or optional parent authority selected the resource"
        );
    }
    let mut bad = value.clone();
    bad["desktop"]["needs"][1]["scope"] = json!({"kind":"wild"});
    assert!(
        !eligible(bad),
        "an unbound inherited topic became explicit display selection"
    );
    let mut bad = value;
    bad["desktop"]["needs"][1]["scope"]["scope"]["value"] = json!("another.topic");
    assert!(!eligible(bad));
}

#[test]
fn real_leased_projection_is_owner_bound_and_delivery_is_not_acknowledgement() {
    let _lock = crate::test_env::lock_env();
    let (_root, _data, service) = service();
    let owner = 45671;
    let record = publish(&service, owner, "one");
    let other = publish(&service, owner + 1, "one");
    let mut state = State::default();
    let command = next_command(&mut state, owner, &service).unwrap();
    assert_eq!(command["kind"], "present");
    assert_eq!(command["body"], "<not markup> & text");
    let handle = command["handle"].as_u64().unwrap() as u32;
    assert_eq!(state.entries.len(), 1);
    assert_eq!(state.entries[&handle].record.id, record.id);
    assert!(!service
        .get(owner + 1, &other.id)
        .unwrap()
        .deliveries
        .iter()
        .any(|delivery| delivery.channel == DeliveryChannel::Desktop
            && delivery.state == DeliveryState::Delivering));
    let mut forged = presented(&command);
    if let Feedback::Presented { token, .. } = &mut forged {
        *token = serde_json::from_value(json!("b".repeat(32))).unwrap();
    }
    assert!(apply(&mut state, forged, owner, &service).is_err());
    apply(&mut state, presented(&command), owner, &service).unwrap();
    apply(&mut state, presented(&command), owner, &service).unwrap();
    let record = service.get(owner, &record.id).unwrap();
    assert_eq!(record.state, NotificationState::Unread);
    assert!(record.acknowledged_at_ms.is_none());
    assert!(record
        .deliveries
        .iter()
        .any(|delivery| delivery.channel == DeliveryChannel::Desktop
            && delivery.state == DeliveryState::Delivered
            && delivery.attempts == 1));
    let expired: Feedback = serde_json::from_value(json!({"kind":"closed","handle":handle,
        "token":command["token"],"reason":"expired"}))
    .unwrap();
    apply(&mut state, expired, owner, &service).unwrap();
    assert_eq!(
        service.get(owner, &record.id).unwrap().state,
        NotificationState::Unread
    );
    assert!(state.entries.is_empty());
    assert!(next_command(&mut state, owner, &service).unwrap().is_null());
}

#[test]
fn replacement_changes_ticket_and_only_exact_current_actions_mutate_durable_state() {
    let _lock = crate::test_env::lock_env();
    let (_root, _data, service) = service();
    let owner = 45672;
    let record = publish(&service, owner, "replace");
    let mut state = State::default();
    let first = next_command(&mut state, owner, &service).unwrap();
    apply(&mut state, presented(&first), owner, &service).unwrap();
    let updated = publish(&service, owner, "replace");
    assert_eq!(updated.id, record.id);
    let replacement = next_command(&mut state, owner, &service).unwrap();
    assert_ne!(replacement["handle"], first["handle"]);
    assert_eq!(replacement["replaces"], first["handle"]);
    assert_ne!(replacement["token"], first["token"]);
    let replaced: Feedback =
        serde_json::from_value(json!({"kind":"closed","handle":first["handle"],
        "token":first["token"],"reason":"closed"}))
        .unwrap();
    apply(&mut state, replaced, owner, &service).unwrap();
    assert!(apply(&mut state, presented(&first), owner, &service).is_err());
    apply(&mut state, presented(&replacement), owner, &service).unwrap();
    let invalid: Feedback =
        serde_json::from_value(json!({"kind":"activated","handle":replacement["handle"],
        "token":replacement["token"],"action":"launch-anything"}))
        .unwrap();
    assert!(apply(&mut state, invalid, owner, &service).is_err());
    let acknowledge: Feedback =
        serde_json::from_value(json!({"kind":"activated","handle":replacement["handle"],
        "token":replacement["token"],"action":"default"}))
        .unwrap();
    apply(&mut state, acknowledge, owner, &service).unwrap();
    assert_eq!(
        service.get(owner, &record.id).unwrap().state,
        NotificationState::Acknowledged
    );
    assert_eq!(
        next_command(&mut state, owner, &service).unwrap()["kind"],
        "close"
    );
    let closed: Feedback =
        serde_json::from_value(json!({"kind":"closed","handle":replacement["handle"],
        "token":replacement["token"],"reason":"closed"}))
        .unwrap();
    apply(&mut state, closed, owner, &service).unwrap();
    assert_eq!(
        service.get(owner, &record.id).unwrap().state,
        NotificationState::Acknowledged
    );
    assert!(state.entries.is_empty());
}

#[test]
fn stale_display_receipts_cannot_deliver_or_acknowledge_a_republished_record() {
    let _lock = crate::test_env::lock_env();
    let (_root, _data, service) = service();
    let owner = 45675;
    let first = publish(&service, owner, "republication");
    let mut state = State::default();
    let old = next_command(&mut state, owner, &service).unwrap();
    publish(&service, owner, "republication");
    apply(&mut state, presented(&old), owner, &service).unwrap();
    let current = service.get(owner, &first.id).unwrap();
    assert_eq!(current.occurrences, 2);
    assert_eq!(current.state, NotificationState::Unread);
    assert!(current
        .deliveries
        .iter()
        .any(|delivery| delivery.channel == DeliveryChannel::Desktop
            && delivery.state == DeliveryState::Queued));
    let closed: Feedback = serde_json::from_value(json!({"kind":"closed","handle":old["handle"],
        "token":old["token"],"reason":"closed"}))
    .unwrap();
    apply(&mut state, closed, owner, &service).unwrap();
    let second = next_command(&mut state, owner, &service).unwrap();
    apply(&mut state, presented(&second), owner, &service).unwrap();
    publish(&service, owner, "republication");
    let stale: Feedback =
        serde_json::from_value(json!({"kind":"activated","handle":second["handle"],
        "token":second["token"],"action":"default"}))
        .unwrap();
    apply(&mut state, stale, owner, &service).unwrap();
    let current = service.get(owner, &first.id).unwrap();
    assert_eq!(current.occurrences, 3);
    assert_eq!(current.state, NotificationState::Unread);
    assert!(current.acknowledged_at_ms.is_none());
    assert!(current
        .deliveries
        .iter()
        .any(|delivery| delivery.channel == DeliveryChannel::Desktop
            && delivery.state == DeliveryState::Queued));
}

#[test]
fn dnd_disabled_delivery_and_stale_receipts_cannot_be_bypassed_by_the_presenter() {
    let _lock = crate::test_env::lock_env();
    let (_root, _data, service) = service();
    let owner = 45673;
    let mut preferences = service.preferences(owner).unwrap();
    preferences.desktop_enabled = false;
    service.set_preferences(owner, preferences.clone()).unwrap();
    publish(&service, owner, "disabled");
    let mut state = State::default();
    assert!(next_command(&mut state, owner, &service).unwrap().is_null());
    preferences.desktop_enabled = true;
    let minute = (notifications::now_ms().div_euclid(60_000).rem_euclid(1440)) as u16;
    preferences.dnd_start_minute_utc = Some(minute);
    preferences.dnd_end_minute_utc = Some((minute + 2) % 1440);
    preferences.critical_bypasses_dnd = false;
    service.set_preferences(owner, preferences.clone()).unwrap();
    publish(&service, owner, "dnd");
    assert!(next_command(&mut state, owner, &service).unwrap().is_null());
    preferences.dnd_start_minute_utc = None;
    preferences.dnd_end_minute_utc = None;
    service.set_preferences(owner, preferences).unwrap();
    let record = publish(&service, owner, "active");
    let command = next_command(&mut state, owner, &service).unwrap();
    assert_eq!(command["kind"], "present");
    let handle = command["handle"].as_u64().unwrap() as u32;
    state.entries.get_mut(&handle).unwrap().lease_until = 0;
    apply(&mut state, presented(&command), owner, &service).unwrap();
    assert_eq!(
        next_command(&mut state, owner, &service).unwrap()["kind"],
        "close"
    );
    let record = service.get(owner, &record.id).unwrap();
    assert_eq!(record.state, NotificationState::Unread);
    assert!(record
        .deliveries
        .iter()
        .any(|delivery| delivery.channel == DeliveryChannel::Desktop
            && delivery.state == DeliveryState::Failed));
}

#[test]
fn late_private_preference_acknowledgement_is_observed_without_replaying_a_write() {
    let _lock = crate::test_env::lock_env();
    let (_root, _data, service) = service();
    let mut state = State::default();
    let (result, completion) = oneshot::channel();
    state.preference_request = Some(PreferenceRequest {
        id: "c".repeat(32),
        muted: true,
        result,
    });
    drop(completion);
    assert!(next_command(&mut state, 45674, &service).unwrap().is_null());
    let event: Feedback = serde_json::from_value(json!({"kind":"preferences",
            "preferences":{"version":1,"muted":true},"request_id":"c".repeat(32)}))
    .unwrap();
    apply(&mut state, event, 45674, &service).unwrap();
    assert_eq!(state.preferences, Some(Preferences::new(true)));
    assert!(state.preference_request.is_none());
    assert!(next_command(&mut state, 45674, &service).unwrap().is_null());
}
