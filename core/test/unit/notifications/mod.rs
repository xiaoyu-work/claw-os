use super::*;

fn draft(kind: &str) -> NotificationDraft {
    NotificationDraft::new(
        "test",
        kind,
        Severity::Warning,
        "Test notification",
        "Something happened.",
    )
    .dedupe(format!("test:{kind}"))
}

#[test]
fn persists_and_isolates_notifications_by_owner() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let created = service.publish(1000, draft("task.completed")).unwrap();

    assert_eq!(created.owner_uid, 1000);
    assert_eq!(service.list(1000, false, 10).unwrap().len(), 1);
    assert!(service.list(1001, false, 10).unwrap().is_empty());
    assert!(matches!(
        service.mutate(1001, &created.id, NotificationMutation::Dismiss),
        Err(NotificationError::NotFound)
    ));
}

#[test]
fn restart_preserves_records_and_deduplicates_replayed_events() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("notifications.db");
    let first = {
        let service = SqliteNotificationService::open(&path).unwrap();
        service.publish(1000, draft("cron.failed")).unwrap()
    };
    let reopened = SqliteNotificationService::open(&path).unwrap();
    let replay = reopened.publish(1000, draft("cron.failed")).unwrap();

    assert_eq!(replay.id, first.id);
    assert_eq!(replay.occurrences, 2);
    assert_eq!(reopened.list(1000, false, 10).unwrap().len(), 1);
}

#[test]
fn dedupe_updates_one_active_record_and_emits_a_change() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let first = service.publish(1000, draft("system.memory_low")).unwrap();
    let mut second_draft = draft("system.memory_low");
    second_draft.body = "Memory is still low.".to_string();
    let second = service.publish(1000, second_draft).unwrap();

    assert_eq!(first.id, second.id);
    assert_eq!(second.occurrences, 2);
    assert_eq!(second.body, "Memory is still low.");
    let changes = service.changes(1000, 0, 10).unwrap();
    assert_eq!(changes.changes.len(), 2);
    assert_eq!(changes.changes[1].change, "updated");
}

#[test]
fn acknowledgement_and_dismissal_are_owner_scoped() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let created = service.publish(42, draft("agent.completed")).unwrap();

    let acknowledged = service
        .mutate(42, &created.id, NotificationMutation::Acknowledge)
        .unwrap();
    assert_eq!(acknowledged.state, NotificationState::Acknowledged);
    assert!(acknowledged.acknowledged_at_ms.is_some());

    let dismissed = service
        .mutate(42, &created.id, NotificationMutation::Dismiss)
        .unwrap();
    assert_eq!(dismissed.state, NotificationState::Dismissed);
    assert!(service.list(42, false, 10).unwrap().is_empty());
    assert_eq!(service.list(42, true, 10).unwrap().len(), 1);
}

#[test]
fn dnd_suppresses_interrupting_channels_but_keeps_web_activity() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let preferences = NotificationPreferences {
        dnd_start_minute_utc: Some(0),
        dnd_end_minute_utc: Some(0),
        ntfy_enabled: true,
        ntfy_topic: Some("alerts".to_string()),
        ..NotificationPreferences::default()
    };
    service.set_preferences(7, preferences).unwrap();

    let created = service.publish(7, draft("cron.failed")).unwrap();
    let web = created
        .deliveries
        .iter()
        .find(|delivery| delivery.channel == DeliveryChannel::Web)
        .unwrap();
    assert_eq!(web.state, DeliveryState::Queued);
    for channel in [DeliveryChannel::Desktop, DeliveryChannel::Ntfy] {
        let delivery = created
            .deliveries
            .iter()
            .find(|delivery| delivery.channel == channel)
            .unwrap();
        assert_eq!(delivery.state, DeliveryState::Suppressed);
    }
}

#[test]
fn delivery_claims_are_leased_and_retryable() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let created = service.publish(9, draft("agent.failed")).unwrap();
    let claims = service
        .claim_deliveries(Some(9), DeliveryChannel::Desktop, 10, 5_000)
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].notification.id, created.id);
    assert_eq!(claims[0].attempts, 1);
    assert!(service
        .claim_deliveries(Some(9), DeliveryChannel::Desktop, 10, 5_000)
        .unwrap()
        .is_empty());

    service
        .complete_delivery(
            9,
            &created.id,
            DeliveryChannel::Desktop,
            DeliveryResult::Failed {
                error_code: "transport".to_string(),
                retry_at_ms: super::now_ms(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .claim_deliveries(Some(9), DeliveryChannel::Desktop, 10, 5_000)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn activity_notifications_do_not_interrupt_desktop_or_ntfy() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let preferences = NotificationPreferences {
        ntfy_enabled: true,
        ntfy_topic: Some("alerts".to_string()),
        ..NotificationPreferences::default()
    };
    service.set_preferences(12, preferences).unwrap();

    let created = service
        .publish(12, draft("cron.started").activity())
        .unwrap();
    assert_eq!(created.deliveries.len(), 1);
    assert_eq!(created.deliveries[0].channel, DeliveryChannel::Web);
}

#[test]
fn preference_validation_rejects_incomplete_ntfy_configuration() {
    let preferences = NotificationPreferences {
        ntfy_enabled: true,
        ..NotificationPreferences::default()
    };
    assert!(preferences.validate().is_err());
}

#[test]
fn notification_text_is_bounded_before_publication() {
    let oversized = "x".repeat(MAX_BODY_CHARS + 10);
    let bounded = bounded_body(&oversized);
    assert_eq!(bounded.chars().count(), MAX_BODY_CHARS);
    assert!(bounded.ends_with("..."));
}
