use super::*;

fn draft(kind: &str) -> NotificationDraft {
    NotificationDraft::new(
        "test",
        kind,
        Severity::Info,
        "Test",
        "Test notification",
    )
}

#[test]
fn muted_kind_is_persisted_without_delivery_work() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let preferences = NotificationPreferences {
        muted_kinds: vec!["agent.completed".to_string()],
        ..NotificationPreferences::default()
    };
    service.set_preferences(5, preferences).unwrap();
    let notification = service.publish(5, draft("agent.completed")).unwrap();
    assert!(notification.deliveries.is_empty());
    assert_eq!(service.list(5, false, 10).unwrap().len(), 1);
}

#[test]
fn publish_prunes_records_older_than_retention_window() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let preferences = NotificationPreferences {
        retention_days: 1,
        ..NotificationPreferences::default()
    };
    service.set_preferences(5, preferences).unwrap();
    let old = service.publish(5, draft("old")).unwrap();
    service
        .lock()
        .unwrap()
        .execute(
            "UPDATE notifications SET created_at_ms = 0, updated_at_ms = 0 WHERE id = ?1",
            params![old.id],
        )
        .unwrap();

    service.publish(5, draft("new")).unwrap();
    let count = service
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM notifications WHERE owner_uid = 5",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn disabling_a_channel_suppresses_queued_delivery() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let notification = service.publish(5, draft("agent.completed")).unwrap();
    service
        .set_preferences(
            5,
            NotificationPreferences {
                desktop_enabled: false,
                ..NotificationPreferences::default()
            },
        )
        .unwrap();
    let updated = service.list(5, false, 10).unwrap().remove(0);
    let desktop = updated
        .deliveries
        .iter()
        .find(|delivery| delivery.channel == DeliveryChannel::Desktop)
        .unwrap();
    assert_eq!(desktop.state, DeliveryState::Suppressed);
    assert_eq!(updated.id, notification.id);
}
