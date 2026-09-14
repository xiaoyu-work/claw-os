use super::*;

fn draft(kind: &str) -> NotificationDraft {
    NotificationDraft::new("test", kind, Severity::Info, "Test", "Test notification")
}

#[test]
fn task_page_filters_before_limiting_and_counts_unread_without_duplicates() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let mut owned = Vec::new();
    for index in 0..105 {
        let mut input = draft("associated");
        input.task_id = Some(format!("task-{}", index % 2));
        owned.push(service.publish(7, input).unwrap());
    }
    for _ in 0..510 {
        let mut input = draft("unrelated");
        input.task_id = Some("another-activity".into());
        service.publish(7, input).unwrap();
    }
    let mut foreign = draft("foreign");
    foreign.task_id = Some("task-0".into());
    service.publish(8, foreign).unwrap();
    service
        .mutate(7, &owned[0].id, NotificationMutation::Read)
        .unwrap();
    service
        .mutate(7, &owned[1].id, NotificationMutation::Dismiss)
        .unwrap();
    service
        .lock()
        .unwrap()
        .execute(
            "UPDATE notifications SET expires_at_ms = 0 WHERE id = ?1",
            params![owned[104].id],
        )
        .unwrap();

    let ids = vec!["task-0".into(), "task-1".into(), "task-0".into()];
    let page = service.list_tasks(7, &ids, 3).unwrap();
    assert_eq!(page.total, 103);
    assert_eq!(page.unread, 102);
    assert_eq!(page.notifications.len(), 3);
    assert!(page.notifications.iter().all(|row| {
        row.owner_uid == 7
            && row.kind == "associated"
            && row.state != NotificationState::Dismissed
            && row.id != owned[104].id
    }));
    assert_eq!(service.list_tasks(0, &ids, 3).unwrap().total, 0);
    assert_eq!(service.list_tasks(9, &ids, 3).unwrap().total, 0);
}

#[test]
fn task_page_is_read_only_and_preserves_delivery_and_acknowledgement() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let mut input = draft("waiting");
    input.task_id = Some("task-1".into());
    let row = service.publish(7, input).unwrap();
    service
        .claim_deliveries(Some(7), DeliveryChannel::Desktop, 1, 5000)
        .unwrap();
    let before = service.get(7, &row.id).unwrap();
    let cursor = service.cursor(7).unwrap();
    let ids = vec!["task-1".into()];
    for _ in 0..2 {
        let page = service.list_tasks(7, &ids, 1).unwrap();
        assert_eq!(page.notifications, vec![before.clone()]);
        assert_eq!((page.total, page.unread), (1, 1));
        assert_eq!(service.cursor(7).unwrap(), cursor);
    }
    let acknowledged = service
        .mutate(7, &row.id, NotificationMutation::Acknowledge)
        .unwrap();
    let page = service.list_tasks(7, &ids, 1).unwrap();
    assert_eq!(page.notifications, vec![acknowledged]);
    assert_eq!((page.total, page.unread), (1, 0));
    assert!(service
        .claim_deliveries(Some(7), DeliveryChannel::Desktop, 1, 5000)
        .unwrap()
        .is_empty());
}

#[test]
fn task_page_never_interprets_an_empty_scope_as_all_owner_notifications() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    service.publish(7, draft("not-task-linked")).unwrap();
    let empty = service.list_tasks(7, &[], 1).unwrap();
    assert!(empty.notifications.is_empty());
    assert_eq!((empty.total, empty.unread), (0, 0));
    assert!(matches!(
        service.list_tasks(7, &["invalid task".into()], 1),
        Err(NotificationError::Invalid(_))
    ));
}

#[test]
fn activity_policy_groups_updates_without_queueing_interruptions() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let mut first = draft("agent.submitted")
        .dedupe("activity:activity-1:tasks")
        .activity();
    first.task_id = Some("task-1".into());
    first.session_id = Some("session-1".into());
    let created = service.publish(7, first).unwrap();

    let mut next = draft("agent.waiting")
        .dedupe("activity:activity-1:tasks")
        .activity();
    next.title = "Agent task needs approval".into();
    next.body = "Waiting for a permission decision.".into();
    next.task_id = Some("task-2".into());
    next.session_id = Some("session-2".into());
    let grouped = service.publish(7, next).unwrap();

    assert_eq!(grouped.id, created.id);
    assert_eq!(grouped.occurrences, 2);
    assert_eq!(grouped.kind, "agent.waiting");
    assert_eq!(grouped.task_id.as_deref(), Some("task-2"));
    assert_eq!(grouped.session_id.as_deref(), Some("session-2"));
    assert!(grouped.deliveries.is_empty());
    for channel in [
        DeliveryChannel::Web,
        DeliveryChannel::Desktop,
        DeliveryChannel::Ntfy,
    ] {
        assert!(service
            .claim_deliveries(Some(7), channel, 10, 5_000)
            .unwrap()
            .is_empty());
    }
    let page = service
        .list_tasks(7, &["task-1".into(), "task-2".into()], 10)
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.notifications, vec![grouped]);
}

#[test]
fn task_page_uses_latest_update_order_and_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notifications.db");
    let expected = {
        let service = SqliteNotificationService::open(&path).unwrap();
        let mut input = draft("old");
        input.task_id = Some("task-1".into());
        let old = service.publish(7, input.clone().dedupe("old")).unwrap();
        let other = service.publish(7, input.clone().dedupe("other")).unwrap();
        service
            .lock()
            .unwrap()
            .execute(
                "UPDATE notifications SET updated_at_ms = ?1 WHERE id = ?2",
                params![other.updated_at_ms + 1, old.id],
            )
            .unwrap();
        vec![old.id, other.id]
    };
    let service = SqliteNotificationService::open(path).unwrap();
    let page = service.list_tasks(7, &["task-1".into()], 10).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(
        page.notifications
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>(),
        expected
    );
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
    service
        .mutate(7, &rows[0].id, NotificationMutation::Read)
        .unwrap();
    service
        .mutate(7, &rows[104].id, NotificationMutation::Dismiss)
        .unwrap();
    let page = service.list_source(7, "test", 20).unwrap();
    assert_eq!(page.total, 105);
    assert_eq!(page.notifications.len(), 20);
    assert_eq!(page.notifications[0].id, rows[104].id);
    assert_eq!(page.notifications[0].state, NotificationState::Dismissed);
    assert_eq!(page.notifications[19].id, rows[85].id);
    assert_eq!(service.list_source(9, "test", 20).unwrap().total, 0);
    assert_eq!(service.list_source(7, "not-test", 20).unwrap().total, 0);
    service
        .lock()
        .unwrap()
        .execute(
            "UPDATE notifications SET expires_at_ms = 0 WHERE id = ?1",
            params![rows[104].id],
        )
        .unwrap();
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
    let before =
        {
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
        assert_eq!(
            service
                .lock()
                .unwrap()
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            2
        );
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
    assert!(matches!(
        service.mutate_source(7, &b.source, &a.id, NotificationMutation::Dismiss),
        Err(NotificationError::NotFound)
    ));
}

#[test]
fn acknowledged_or_closed_records_cannot_be_requeued_by_stale_delivery_receipts() {
    for mutation in [
        NotificationMutation::Acknowledge,
        NotificationMutation::Dismiss,
    ] {
        let service = SqliteNotificationService::open_in_memory().unwrap();
        let record = service.publish(7, draft("race")).unwrap();
        let claim = service
            .claim_deliveries(Some(7), DeliveryChannel::Desktop, 10, 1)
            .unwrap();
        assert_eq!(claim.len(), 1);
        service.mutate(7, &record.id, mutation).unwrap();
        let completed = service
            .complete_delivery(
                7,
                &record.id,
                DeliveryChannel::Desktop,
                DeliveryResult::Failed {
                    error_code: "late".into(),
                    retry_at_ms: 0,
                },
            )
            .unwrap();
        assert_eq!(
            completed
                .deliveries
                .iter()
                .find(|d| d.channel == DeliveryChannel::Desktop)
                .unwrap()
                .state,
            DeliveryState::Suppressed
        );
        assert!(service
            .claim_deliveries(Some(7), DeliveryChannel::Desktop, 10, 1)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn expired_lease_retries_only_for_the_same_owner_and_keeps_unread_state() {
    let service = SqliteNotificationService::open_in_memory().unwrap();
    let record = service.publish(7, draft("lease")).unwrap();
    assert_eq!(
        service
            .claim_deliveries(Some(7), DeliveryChannel::Desktop, 10, 5000)
            .unwrap()
            .len(),
        1
    );
    service
        .lock()
        .unwrap()
        .execute(
            "UPDATE notification_deliveries SET next_attempt_at_ms = 0 WHERE notification_id = ?1",
            params![record.id],
        )
        .unwrap();
    assert!(service
        .claim_deliveries(Some(8), DeliveryChannel::Desktop, 10, 5000)
        .unwrap()
        .is_empty());
    let retried = service
        .claim_deliveries(Some(7), DeliveryChannel::Desktop, 10, 5000)
        .unwrap();
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
