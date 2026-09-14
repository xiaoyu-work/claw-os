use super::*;
use crate::activities::{
    ActivityExecutionPlacement, ActivitySchedulingPriority, ActivityService, ExecutionLimitsDraft,
    DATABASE_SCHEMA_VERSION,
};

fn semantic_draft() -> crate::activities::ActivityDraft {
    let mut value = super::super::tests::draft();
    value.resources.push(crate::activities::ActivityResource {
        label: "Status".into(),
        reference: "app://kv/entry?id=release.status&revision=v1".into(),
    });
    value
}

fn limits() -> ExecutionLimitsDraft {
    ExecutionLimitsDraft {
        max_attempts: 9,
        max_turns_per_attempt: 4,
        expires_at: "2999-01-01T00:00:00.000000000Z".into(),
    }
}

#[test]
fn export_is_deterministic_read_only_and_omits_nonportable_state() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(1000, semantic_draft()).unwrap();
    service
        .set_execution_limits(1000, &activity.id, None, limits())
        .unwrap();
    service
        .set_scheduling_policy(
            1000,
            &activity.id,
            None,
            ActivitySchedulingPriority::Background,
        )
        .unwrap();
    let before: (String, i64, i64, i64, i64) = service
        .lock()
        .unwrap()
        .query_row(
            "SELECT a.updated_at, c.revision,
                (SELECT COUNT(*) FROM activity_receipts WHERE activity_id = a.id),
                (SELECT COUNT(*) FROM activity_object_state WHERE activity_id = a.id),
                (SELECT COUNT(*) FROM activity_execution_reservations WHERE activity_id = a.id)
             FROM activities a JOIN activity_continuity c
               ON c.owner_uid = a.owner_uid AND c.activity_id = a.id
             WHERE a.owner_uid = ?1 AND a.id = ?2",
            params![1000, activity.id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();

    let first = service.export_continuity(1000, &activity.id).unwrap();
    let second = service.export_continuity(1000, &activity.id).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.references.len(), 1);
    assert_eq!(
        first.references[0].reference,
        "app://kv/entry?id=release.status&revision=v1"
    );
    assert_eq!(
        first.rules.scheduling.as_ref().unwrap().priority,
        ActivitySchedulingPriority::Background
    );
    assert_eq!(
        first.rules.execution_limits.as_ref().unwrap().max_attempts,
        9
    );
    let serialized = String::from_utf8(first.to_json().unwrap()).unwrap();
    for forbidden in [
        "/home/",
        "owner_uid",
        "used_attempts",
        "created_at",
        "updated_at",
        "job",
        "session",
        "receipt",
        "effect",
        "result",
        "object_state",
        "monetary",
        "capability",
        "completion_note",
        "state",
    ] {
        assert!(!serialized.contains(forbidden), "{forbidden}");
    }
    let after: (String, i64, i64, i64, i64) = service
        .lock()
        .unwrap()
        .query_row(
            "SELECT a.updated_at, c.revision,
                (SELECT COUNT(*) FROM activity_receipts WHERE activity_id = a.id),
                (SELECT COUNT(*) FROM activity_object_state WHERE activity_id = a.id),
                (SELECT COUNT(*) FROM activity_execution_reservations WHERE activity_id = a.id)
             FROM activities a JOIN activity_continuity c
               ON c.owner_uid = a.owner_uid AND c.activity_id = a.id
             WHERE a.owner_uid = ?1 AND a.id = ?2",
            params![1000, activity.id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn import_is_owner_scoped_paused_atomic_and_preserves_only_safe_rules() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let source = service.create(1000, semantic_draft()).unwrap();
    service
        .set_execution_limits(1000, &source.id, None, limits())
        .unwrap();
    service
        .set_scheduling_policy(
            1000,
            &source.id,
            None,
            ActivitySchedulingPriority::Foreground,
        )
        .unwrap();
    let document = service.export_continuity(1000, &source.id).unwrap();

    let imported = service
        .import_continuity(2000, ActivityExecutionPlacement::Local, document.clone())
        .unwrap();
    assert_ne!(imported.activity.id, source.id);
    assert_eq!(imported.activity.state, ActivityState::Paused);
    assert!(imported.activity.completion_note.is_none());
    assert_eq!(imported.continuity_id, document.lineage.id);
    assert_eq!(imported.continuity_revision, document.lineage.revision);
    assert_eq!(
        service
            .export_continuity(2000, &imported.activity.id)
            .unwrap(),
        document
    );
    let imported_limits = service
        .execution_limits(2000, &imported.activity.id)
        .unwrap()
        .unwrap();
    assert_eq!(imported_limits.revision, 1);
    assert_eq!(imported_limits.used_attempts, 0);
    assert_eq!(imported_limits.limits, limits());
    assert_eq!(
        service
            .scheduling_policy(2000, &imported.activity.id)
            .unwrap()
            .unwrap()
            .priority,
        ActivitySchedulingPriority::Foreground
    );
    assert!(service
        .import_continuity(2000, ActivityExecutionPlacement::Local, document.clone())
        .is_err());
    assert!(service
        .import_continuity(3000, ActivityExecutionPlacement::Local, document)
        .is_ok());
}

#[test]
fn failed_rule_application_rolls_back_activity_and_lineage() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let source = service.create(1000, semantic_draft()).unwrap();
    service
        .set_scheduling_policy(
            1000,
            &source.id,
            None,
            ActivitySchedulingPriority::Foreground,
        )
        .unwrap();
    let document = service.export_continuity(1000, &source.id).unwrap();
    service
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_continuity_scheduling
             BEFORE INSERT ON activity_scheduling_policies
             BEGIN SELECT RAISE(ABORT, 'injected scheduling failure'); END;",
        )
        .unwrap();
    assert!(service
        .import_continuity(2000, ActivityExecutionPlacement::Local, document)
        .is_err());
    assert!(service.list(2000, None, 100).unwrap().is_empty());
    let lineages: i64 = service
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM activity_continuity WHERE owner_uid = 2000",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(lineages, 0);
}

#[test]
fn continuity_revision_changes_only_with_portable_snapshot_fields() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(1000, semantic_draft()).unwrap();
    let initial = service.export_continuity(1000, &activity.id).unwrap();
    service
        .transition(1000, &activity.id, ActivityState::Paused, None)
        .unwrap();
    service
        .add_resource(
            1000,
            &activity.id,
            crate::activities::ActivityResource {
                label: "Local".into(),
                reference: "/home/user/local.txt".into(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .export_continuity(1000, &activity.id)
            .unwrap()
            .lineage
            .revision,
        initial.lineage.revision
    );
    service
        .update(
            1000,
            &activity.id,
            crate::activities::ActivityPatch {
                title: Some("Release revised".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let revised = service.export_continuity(1000, &activity.id).unwrap();
    assert_eq!(revised.lineage.revision, initial.lineage.revision + 1);

    let policy = service
        .set_execution_limits(1000, &activity.id, None, limits())
        .unwrap();
    let with_limits = service.export_continuity(1000, &activity.id).unwrap();
    assert_eq!(with_limits.lineage.revision, revised.lineage.revision + 1);
    service
        .set_execution_limits(1000, &activity.id, Some(policy.revision), limits())
        .unwrap();
    assert_eq!(
        service
            .export_continuity(1000, &activity.id)
            .unwrap()
            .lineage
            .revision,
        with_limits.lineage.revision
    );
}

fn migrate_legacy_document(
    owner_uid: u32,
    activity_id: &str,
) -> crate::activities::ActivityContinuityDocument {
    let directory = super::super::tests::TestDirectory::new();
    let path = directory.database();
    let original_id = {
        let service = SqliteActivityService::open(&path).unwrap();
        service.create(owner_uid, semantic_draft()).unwrap().id
    };
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF; DROP TABLE activity_continuity;")
        .unwrap();
    conn.execute(
        "UPDATE activities SET id = ?1 WHERE owner_uid = ?2 AND id = ?3",
        params![activity_id, i64::from(owner_uid), original_id],
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 7).unwrap();
    drop(conn);
    SqliteActivityService::open(&path)
        .unwrap()
        .export_continuity(owner_uid, activity_id)
        .unwrap()
}

#[test]
fn legacy_backfill_identity_depends_only_on_activity_id() {
    let activity_id = "00000000-0000-4000-8000-000000000077";
    let low_uid = migrate_legacy_document(77, activity_id);
    let high_uid = migrate_legacy_document(4_000_000_000, activity_id);
    assert_eq!(low_uid, high_uid);
    assert_eq!(low_uid.lineage.id, legacy_continuity_id(activity_id));
    assert_eq!(
        low_uid.lineage.revision, 1,
        "legacy backfill starts at the immutable first snapshot"
    );

    let different_id = "00000000-0000-4000-8000-000000000078";
    assert_ne!(
        legacy_continuity_id(activity_id),
        legacy_continuity_id(different_id)
    );
    let serialized = String::from_utf8(low_uid.to_json().unwrap()).unwrap();
    assert!(!serialized.contains("owner_uid"));
}

#[test]
fn schema_seven_migration_assigns_stable_deterministic_identities() {
    let directory = super::super::tests::TestDirectory::new();
    let path = directory.database();
    let activity = {
        let service = SqliteActivityService::open(&path).unwrap();
        service.create(77, semantic_draft()).unwrap()
    };
    let mut expected = None;
    for _ in 0..2 {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE activity_continuity; PRAGMA user_version = 7;")
            .unwrap();
        drop(conn);
        let service = SqliteActivityService::open(&path).unwrap();
        let id = service
            .export_continuity(77, &activity.id)
            .unwrap()
            .lineage
            .id;
        assert_eq!(id, legacy_continuity_id(&activity.id));
        if let Some(expected) = expected.as_deref() {
            assert_eq!(id, expected);
        } else {
            expected = Some(id);
        }
        assert_eq!(
            service
                .lock()
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            DATABASE_SCHEMA_VERSION
        );
    }
}

#[test]
fn corrupt_schema_seven_rows_roll_back_identity_migration_and_version() {
    let directory = super::super::tests::TestDirectory::new();
    let path = directory.database();
    {
        let service = SqliteActivityService::open(&path).unwrap();
        let activity = service.create(77, semantic_draft()).unwrap();
        drop(service);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF; DROP TABLE activity_continuity;")
            .unwrap();
        conn.execute(
            "UPDATE activities SET id = 'not-a-canonical-uuid' WHERE id = ?1",
            [activity.id],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Corrupt(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        7
    );
    let table: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'activity_continuity')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!table);
}

#[test]
fn missing_current_identity_is_corruption_not_implicit_repair() {
    let directory = super::super::tests::TestDirectory::new();
    let path = directory.database();
    {
        let service = SqliteActivityService::open(&path).unwrap();
        let activity = service.create(77, semantic_draft()).unwrap();
        service
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM activity_continuity WHERE owner_uid = 77 AND activity_id = ?1",
                [activity.id],
            )
            .unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Corrupt(_))
    ));
}
