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
fn source_page_counts_the_full_scope_and_keeps_publication_order() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let mut rows = Vec::new();
    for _ in 0..105 {
        rows.push(service.publish(7, draft("owned")).unwrap());
    }
    service.publish(8, draft("foreign-owner")).unwrap();
    let mut foreign = draft("foreign-source");
    foreign.source = "app:another".into();
    service.publish(7, foreign).unwrap();
    service.mutate(7, &rows[0].id, NotificationMutation::Read).unwrap();
    service.mutate(7, &rows[104].id, NotificationMutation::Dismiss).unwrap();
    let page = service.list_source(7, "test", 20).unwrap();
    assert_eq!(page.total, 105);
    assert_eq!(page.notifications.len(), 20);
    assert_eq!(page.notifications[0].id, rows[104].id);
    assert_eq!(page.notifications[0].state, NotificationState::Dismissed);
    assert_eq!(page.notifications[19].id, rows[85].id);
    assert_eq!(service.list_source(9, "test", 20).unwrap().total, 0);
    assert_eq!(service.list_source(7, "not-test", 20).unwrap().total, 0);
    service.lock().unwrap().execute(
        "UPDATE notifications SET expires_at_ms = 0 WHERE id = ?1",
        params![rows[104].id],
    ).unwrap();
    let page = service.list_source(7, "test", 20).unwrap();
    assert_eq!(page.total, 104);
    assert_eq!(page.notifications[0].id, rows[103].id);
}

#[test]
fn source_page_count_and_rows_share_one_snapshot_across_connections() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let path = root.path().join("snapshot.db");
    let reader = SqliteNotificationService::open(&path).unwrap();
    let writer = SqliteNotificationService::open(path).unwrap();
    let started = std::sync::Arc::new(std::sync::Barrier::new(2));
    let child_started = started.clone();
    let child = std::thread::spawn(move || {
        child_started.wait();
        for index in 1..=100 {
            let mut input = draft("snapshot");
            input.body = index.to_string();
            writer.publish(7, input).unwrap();
        }
    });
    started.wait();
    loop {
        let page = reader.list_source(7, "test", 1).unwrap();
        if let Some(row) = page.notifications.first() {
            assert_eq!(row.body.parse::<u64>().unwrap(), page.total);
        } else {
            assert_eq!(page.total, 0);
        }
        if child.is_finished() {
            break;
        }
    }
    child.join().unwrap();
    assert_eq!(reader.list_source(7, "test", 1).unwrap().total, 100);
}

#[test]
fn notification_schema_upgrade_preserves_v1_records_and_is_idempotent() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let path = root.path().join("notifications.db");
    let before = {
        let service = SqliteNotificationService::open(&path).unwrap();
        let record = service.publish(7, draft("legacy")).unwrap();
        service.lock().unwrap().execute_batch(
            "ALTER TABLE notifications DROP COLUMN presentation_json; PRAGMA user_version = 1;",
        ).unwrap();
        record
    };
    for _ in 0..2 {
        let service = SqliteNotificationService::open(&path).unwrap();
        assert_eq!(service.get(7, &before.id).unwrap(), before);
        assert_eq!(service.lock().unwrap().query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0)).unwrap(), 2);
    }
}

#[test]
fn deduplication_never_reassigns_a_record_to_another_source() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let a = service.publish(7, draft("same").dedupe("key")).unwrap();
    let mut input = draft("same").dedupe("key");
    input.source = "app:cosmic-notifications".into();
    let b = service.publish(7, input).unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(service.get(7, &a.id).unwrap().source, a.source);
    assert!(matches!(service.mutate_source(7, &b.source, &a.id, NotificationMutation::Dismiss),
        Err(NotificationError::NotFound)));
}

#[test]
fn acknowledged_or_closed_records_cannot_be_requeued_by_stale_delivery_receipts() {
    for mutation in [NotificationMutation::Acknowledge, NotificationMutation::Dismiss] {
        let service = SqliteNotificationService::open_in_memory().unwrap();
        let record = service.publish(7, draft("race")).unwrap();
        let claim = service.claim_deliveries(Some(7), DeliveryChannel::Desktop, 10, 1).unwrap();
        assert_eq!(claim.len(), 1);
        service.mutate(7, &record.id, mutation).unwrap();
        let completed = service.complete_delivery(7, &record.id, DeliveryChannel::Desktop,
            DeliveryResult::Failed { error_code: "late".into(), retry_at_ms: 0 }).unwrap();
        assert_eq!(completed.deliveries.iter().find(|d| d.channel == DeliveryChannel::Desktop).unwrap().state,
            DeliveryState::Suppressed);
        assert!(service.claim_deliveries(Some(7), DeliveryChannel::Desktop, 10, 1).unwrap().is_empty());
    }
}

#[test]
fn expired_lease_retries_only_for_the_same_owner_and_keeps_unread_state() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let record = service.publish(7, draft("lease")).unwrap();
    assert_eq!(service.claim_deliveries(Some(7), DeliveryChannel::Desktop, 10, 5000).unwrap().len(), 1);
    service.lock().unwrap().execute(
        "UPDATE notification_deliveries SET next_attempt_at_ms = 0 WHERE notification_id = ?1",
        params![record.id],
    ).unwrap();
    assert!(service.claim_deliveries(Some(8), DeliveryChannel::Desktop, 10, 5000).unwrap().is_empty());
    let retried = service.claim_deliveries(Some(7), DeliveryChannel::Desktop, 10, 5000).unwrap();
    assert_eq!(retried[0].attempts, 2);
    assert_eq!(retried[0].notification.state, NotificationState::Unread);
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
