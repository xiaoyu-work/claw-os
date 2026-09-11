use super::*;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = PathBuf::from(format!(".activities-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn database(&self) -> PathBuf {
        self.0.join("activities.db")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove Activity test files");
    }
}

fn draft() -> ActivityDraft {
    ActivityDraft {
        title: "Release".to_string(),
        goal: "Prepare a release.\n\tAsk for review.".to_string(),
        completion_criteria: "Published and verified".to_string(),
        boundaries: "Do not publish without approval".to_string(),
        resources: vec![ActivityResource {
            label: "Draft".to_string(),
            reference: "https://example.invalid/release".to_string(),
        }],
    }
}

fn title_patch(title: &str) -> ActivityPatch {
    ActivityPatch {
        title: Some(title.to_string()),
        ..Default::default()
    }
}

#[test]
fn object_reference_attachment_is_atomic_idempotent_and_does_not_change_goal_state() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(1000, draft()).unwrap();
    let resource = ActivityResource {
        label: "Status".into(),
        reference: "app://kv/entry?id=release.status".into(),
    };
    let first = service.add_resource(1000, &activity.id, resource.clone()).unwrap();
    let mut renamed = resource.clone();
    renamed.label = "Current status".into();
    let second = service.add_resource(1000, &activity.id, renamed).unwrap();
    assert_eq!(first.resources.len(), 2);
    assert_eq!(second.resources.len(), 2);
    assert_eq!(second.resources[1].label, "Current status");
    assert_eq!(second.goal, activity.goal);
    assert_eq!(second.state, ActivityState::Active);
    assert!(matches!(service.add_resource(0, &activity.id, resource.clone()), Err(ActivityError::NotFound)));
    service.transition(1000, &activity.id, ActivityState::Completed, Some("Confirmed".into())).unwrap();
    assert!(service.add_resource(1000, &activity.id, resource).is_err());
    let version: u32 = service.lock().unwrap().pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
    assert_eq!(version, DATABASE_SCHEMA_VERSION);
}

#[test]
fn concurrent_object_attachments_do_not_lose_other_resource_updates() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let service = SqliteActivityService::open(&path).unwrap();
    let activity = service.create(1000, draft()).unwrap();
    let mut handles = Vec::new();
    for index in 0..8 {
        let path = path.clone();
        let id = activity.id.clone();
        handles.push(std::thread::spawn(move || {
            SqliteActivityService::open(&path)?.add_resource(1000, &id, ActivityResource {
                label: format!("Object {index}"),
                reference: format!("app://kv/entry?id=key{index}"),
            })
        }));
    }
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    assert_eq!(service.get(1000, &activity.id).unwrap().resources.len(), 9);
}

#[test]
fn object_attachment_quota_failure_preserves_all_existing_resources() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let mut value = draft();
    value.resources = (0..32).map(|index| ActivityResource {
        label: format!("Object {index}"),
        reference: format!("app://kv/entry?id=key{index}"),
    }).collect();
    let activity = service.create(1000, value).unwrap();
    assert!(service.add_resource(1000, &activity.id, ActivityResource {
        label: "Too many".into(),
        reference: "app://kv/entry?id=extra".into(),
    }).is_err());
    assert_eq!(service.get(1000, &activity.id).unwrap(), activity);
}

#[test]
fn persists_all_metadata_and_confirmation_across_reopen() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let expected = {
        let provider = SqliteActivityService::open(&path).unwrap();
        let service: &dyn ActivityService = &provider;
        let created = service.create(1000, draft()).unwrap();
        assert_eq!(created.state, ActivityState::Active);
        assert!(created.completion_note.is_none());
        assert_eq!(created.created_at, created.updated_at);
        assert!(uuid::Uuid::parse_str(&created.id).is_ok());
        assert!(DateTime::parse_from_rfc3339(&created.created_at).is_ok());
        service
            .transition(
                1000,
                &created.id,
                ActivityState::Completed,
                Some("  I reviewed and published it.\nThe release is available.  ".to_string()),
            )
            .unwrap()
    };
    let reopened = SqliteActivityService::open(&path).unwrap();
    assert_eq!(reopened.get(1000, &expected.id).unwrap(), expected);
    assert_eq!(
        reopened
            .list(1000, Some(ActivityState::Completed), DEFAULT_LIST_LIMIT)
            .unwrap(),
        vec![expected.clone()]
    );
    assert_eq!(
        expected.completion_note.as_deref(),
        Some("I reviewed and published it.\nThe release is available.")
    );
    assert!(expected.updated_at >= expected.created_at);
}

#[test]
fn owners_are_independent_including_root_and_foreign_ids_are_not_found() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let owners = [0, 7, 8, u32::MAX];
    let records: Vec<_> = owners
        .iter()
        .map(|owner| service.create(*owner, draft()).unwrap())
        .collect();
    let missing = uuid::Uuid::new_v4().to_string();
    for (owner, record) in owners.into_iter().zip(&records) {
        assert_eq!(service.get(owner, &record.id).unwrap(), *record);
        assert_eq!(
            service.get(owner, &record.id.to_uppercase()).unwrap(),
            *record
        );
        assert_eq!(
            service.list(owner, None, 100).unwrap(),
            vec![record.clone()]
        );
        for foreign in records.iter().filter(|record| record.owner_uid != owner) {
            for id in [&foreign.id, &missing] {
                assert!(matches!(
                    service.get(owner, id),
                    Err(ActivityError::NotFound)
                ));
                assert!(matches!(
                    service.update(owner, id, title_patch("Changed")),
                    Err(ActivityError::NotFound)
                ));
                assert!(matches!(
                    service.transition(owner, id, ActivityState::Paused, None),
                    Err(ActivityError::NotFound)
                ));
            }
        }
    }
    for record in &records {
        assert_eq!(service.get(record.owner_uid, &record.id).unwrap(), *record);
    }
}

#[test]
fn invalid_identifiers_are_rejected_before_queries_or_mutations() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let record = service.create(7, draft()).unwrap();
    for id in ["", "not-a-uuid", "../activities.db", "00000000\0"] {
        assert!(matches!(service.get(7, id), Err(ActivityError::Invalid(_))));
        assert!(matches!(
            service.update(7, id, title_patch("Changed")),
            Err(ActivityError::Invalid(_))
        ));
        assert!(matches!(
            service.transition(7, id, ActivityState::Cancelled, None),
            Err(ActivityError::Invalid(_))
        ));
    }
    assert_eq!(service.get(7, &record.id).unwrap(), record);
}

#[test]
fn filtered_lists_have_default_and_maximum_bounds_and_stable_order() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    for index in 0..125 {
        let mut input = draft();
        input.title = format!("Goal {index}");
        let created = service.create(7, input).unwrap();
        if index < 10 {
            service
                .transition(7, &created.id, ActivityState::Paused, None)
                .unwrap();
        }
    }
    service.create(8, draft()).unwrap();
    assert_eq!(service.list(7, None, 0).unwrap().len(), DEFAULT_LIST_LIMIT);
    assert_eq!(
        service.list(7, None, usize::MAX).unwrap().len(),
        MAX_LIST_LIMIT
    );
    assert_eq!(service.list(7, None, 3).unwrap().len(), 3);
    let paused = service.list(7, Some(ActivityState::Paused), 100).unwrap();
    assert_eq!(paused.len(), 10);
    assert!(paused
        .iter()
        .all(|record| record.owner_uid == 7 && record.state == ActivityState::Paused));
    let active = service
        .list(7, Some(ActivityState::Active), usize::MAX)
        .unwrap();
    assert_eq!(active.len(), MAX_LIST_LIMIT);
    assert!(active
        .iter()
        .all(|record| record.owner_uid == 7 && record.state == ActivityState::Active));
    assert!(active
        .windows(2)
        .all(|pair| { (&pair[0].updated_at, &pair[0].id) >= (&pair[1].updated_at, &pair[1].id) }));
    assert!(service
        .list(7, Some(ActivityState::Completed), 100)
        .unwrap()
        .is_empty());
    assert!(service.list(0, None, 100).unwrap().is_empty());
}

#[test]
fn partial_updates_preserve_omitted_fields_and_allow_explicit_clearing() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let before = service.create(7, draft()).unwrap();
    let after = service
        .update(
            7,
            &before.id,
            ActivityPatch {
                completion_criteria: Some("  Reviewed and signed  ".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    let mut expected = before.clone();
    expected.completion_criteria = "Reviewed and signed".to_string();
    expected.updated_at = after.updated_at.clone();
    assert_eq!(after, expected);
    service
        .transition(7, &before.id, ActivityState::Paused, None)
        .unwrap();
    let updated = service
        .update(
            7,
            &before.id,
            ActivityPatch {
                goal: Some("  A revised goal\nwith another step.  ".to_string()),
                boundaries: Some(" \n ".to_string()),
                resources: Some(Vec::new()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.state, ActivityState::Paused);
    assert_eq!(updated.goal, "A revised goal\nwith another step.");
    assert!(updated.boundaries.is_empty());
    assert!(updated.resources.is_empty());
    assert_eq!(updated.title, before.title);
    assert_eq!(updated.completion_criteria, after.completion_criteria);
    assert_eq!(updated.created_at, before.created_at);
    assert!(updated.updated_at >= after.updated_at);
}

#[test]
fn invalid_writes_leave_existing_records_unchanged() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let before = service.create(7, draft()).unwrap();
    let mut invalid = draft();
    invalid.resources = vec![invalid.resources[0].clone(); super::super::MAX_RESOURCES + 1];
    assert!(matches!(
        service.create(7, invalid),
        Err(ActivityError::Invalid(_))
    ));
    for patch in [
        ActivityPatch::default(),
        ActivityPatch {
            title: Some("Changed".to_string()),
            goal: Some("\0".to_string()),
            ..Default::default()
        },
        ActivityPatch {
            title: Some("Changed".to_string()),
            boundaries: Some("x".repeat(super::super::MAX_PLANNING_BYTES + 1)),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            service.update(7, &before.id, patch),
            Err(ActivityError::Invalid(_))
        ));
        assert_eq!(service.get(7, &before.id).unwrap(), before);
    }
    assert_eq!(service.list(7, None, 100).unwrap(), vec![before]);
}

#[test]
fn transition_matrix_rejects_no_ops_and_requires_reopen_for_terminal_records() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let states = [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ];
    for current in states {
        for next in states {
            let mut before = service.create(7, draft()).unwrap();
            if current != ActivityState::Active {
                before = service
                    .transition(
                        7,
                        &before.id,
                        current,
                        (current == ActivityState::Completed)
                            .then(|| "I confirmed completion".to_string()),
                    )
                    .unwrap();
            }
            let result = service.transition(
                7,
                &before.id,
                next,
                (next == ActivityState::Completed).then(|| "I confirmed completion".to_string()),
            );
            let allowed = match current {
                ActivityState::Active | ActivityState::Paused => current != next,
                ActivityState::Completed | ActivityState::Cancelled => {
                    next == ActivityState::Active
                }
            };
            if allowed {
                let after = result.unwrap();
                assert_eq!(after.state, next);
                assert_eq!(
                    after.completion_note.is_some(),
                    next == ActivityState::Completed
                );
                assert_eq!(after.created_at, before.created_at);
                assert_eq!(after.goal, before.goal);
            } else {
                assert!(matches!(result, Err(ActivityError::Conflict(_))));
                assert_eq!(service.get(7, &before.id).unwrap(), before);
            }
        }
    }
}

#[test]
fn terminal_metadata_requires_explicit_reopening_before_editing() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    for state in [ActivityState::Completed, ActivityState::Cancelled] {
        let created = service.create(7, draft()).unwrap();
        let terminal = service
            .transition(
                7,
                &created.id,
                state,
                (state == ActivityState::Completed).then(|| "Reviewed and published".to_string()),
            )
            .unwrap();
        assert!(matches!(
            service.update(7, &created.id, title_patch("New scope")),
            Err(ActivityError::Conflict(_))
        ));
        assert_eq!(service.get(7, &created.id).unwrap(), terminal);
        let reopened = service
            .transition(7, &created.id, ActivityState::Active, None)
            .unwrap();
        assert!(reopened.state.allows_work());
        assert!(reopened.completion_note.is_none());
        let edited = service
            .update(7, &created.id, title_patch("New scope"))
            .unwrap();
        assert_eq!(edited.title, "New scope");
        assert_eq!(edited.created_at, created.created_at);
    }
}

#[test]
fn completion_confirmation_cannot_be_omitted_or_reused_implicitly() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let before = service.create(7, draft()).unwrap();
    for note in [
        None,
        Some(String::new()),
        Some(" \r\n\t ".to_string()),
        Some("x".repeat(super::super::MAX_COMPLETION_NOTE_BYTES + 1)),
        Some("confirmed\0".to_string()),
    ] {
        assert!(matches!(
            service.transition(7, &before.id, ActivityState::Completed, note),
            Err(ActivityError::Invalid(_))
        ));
        assert_eq!(service.get(7, &before.id).unwrap(), before);
    }
    assert!(matches!(
        service.transition(
            7,
            &before.id,
            ActivityState::Paused,
            Some("This is not a completion".to_string())
        ),
        Err(ActivityError::Invalid(_))
    ));
    let completed = service
        .transition(
            7,
            &before.id,
            ActivityState::Completed,
            Some("  I checked the result.\nIt meets the criteria.  ".to_string()),
        )
        .unwrap();
    assert_eq!(
        completed.completion_note.as_deref(),
        Some("I checked the result.\nIt meets the criteria.")
    );
    assert!(matches!(
        service.transition(7, &before.id, ActivityState::Completed, None),
        Err(ActivityError::Invalid(_))
    ));
    service
        .transition(7, &before.id, ActivityState::Active, None)
        .unwrap();
    assert!(matches!(
        service.transition(7, &before.id, ActivityState::Completed, None),
        Err(ActivityError::Invalid(_))
    ));
    let reopened = service.get(7, &before.id).unwrap();
    assert_eq!(reopened.state, ActivityState::Active);
    assert!(reopened.completion_note.is_none());
}

#[test]
fn execution_result_text_never_automatically_completes_a_goal() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let mut input = draft();
    input.goal = "The execution returned ok; the model says the work is done".to_string();
    input.resources[0].reference = "not-a-real-file-to-read-or-execute".to_string();
    let created = service.create(7, input).unwrap();
    let updated = service
        .update(
            7,
            &created.id,
            ActivityPatch {
                completion_criteria: Some("All job steps succeeded".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    for record in [
        updated,
        service.get(7, &created.id).unwrap(),
        service.list(7, None, 1).unwrap().remove(0),
    ] {
        assert_eq!(record.state, ActivityState::Active);
        assert!(record.completion_note.is_none());
        let value = serde_json::to_value(record).unwrap();
        assert!(value.get("job_ids").is_none());
        assert!(value.get("task_ids").is_none());
        assert!(value.get("session_ids").is_none());
    }
}

#[test]
fn all_records_count_toward_the_owner_limit_without_pruning_other_owners() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let first = service.create(7, draft()).unwrap();
    for _ in 1..MAX_ACTIVITIES_PER_OWNER {
        service.create(7, draft()).unwrap();
    }
    service
        .transition(7, &first.id, ActivityState::Cancelled, None)
        .unwrap();
    assert!(matches!(
        service.create(7, draft()),
        Err(ActivityError::LimitReached)
    ));
    assert_eq!(
        service.get(7, &first.id).unwrap().state,
        ActivityState::Cancelled
    );
    assert!(service.create(0, draft()).is_ok());
    assert!(service.create(8, draft()).is_ok());
}

#[test]
fn independent_connections_update_only_their_supplied_fields() {
    let directory = TestDirectory::new();
    let first = SqliteActivityService::open(directory.database()).unwrap();
    let second = SqliteActivityService::open(directory.database()).unwrap();
    let created = first.create(7, draft()).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let id = created.id.clone();
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        second
            .update(
                7,
                &id,
                ActivityPatch {
                    goal: Some("The new goal".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
    });
    barrier.wait();
    first
        .update(7, &created.id, title_patch("New title"))
        .unwrap();
    worker.join().unwrap();
    let persisted = first.get(7, &created.id).unwrap();
    assert_eq!(persisted.title, "New title");
    assert_eq!(persisted.goal, "The new goal");
    assert_eq!(persisted.boundaries, created.boundaries);
    assert_eq!(persisted.resources, created.resources);
}

#[test]
fn database_write_failure_rolls_back_without_reporting_success() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let created = service.create(7, draft()).unwrap();
    service
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_update BEFORE UPDATE ON activities
             BEGIN SELECT RAISE(ABORT, 'test write failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        service.update(7, &created.id, title_patch("Not persisted")),
        Err(ActivityError::Database(_))
    ));
    assert!(matches!(
        service.transition(7, &created.id, ActivityState::Cancelled, None),
        Err(ActivityError::Database(_))
    ));
    assert_eq!(service.get(7, &created.id).unwrap(), created);
}

#[test]
fn poisoned_provider_lock_is_an_error_for_every_operation() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let created = service.create(7, draft()).unwrap();
    let clone = service.clone();
    assert!(std::thread::spawn(move || {
        let _guard = clone.conn.lock().unwrap();
        panic!("poison the Activity connection lock");
    })
    .join()
    .is_err());
    assert!(matches!(
        service.create(7, draft()),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.get(7, &created.id),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.list(7, None, 100),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.update(7, &created.id, title_patch("Changed")),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.transition(7, &created.id, ActivityState::Paused, None),
        Err(ActivityError::Poisoned)
    ));
}

#[test]
fn corrupt_file_is_not_replaced_or_reset() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let bytes = b"This is not a SQLite database. Preserve it for recovery.";
    fs::write(&path, bytes).unwrap();
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Database(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn orphaned_journals_are_preserved_without_initializing_a_replacement_database() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let wal = sidecar_path(&path, "-wal");
    fs::write(&wal, b"unrecovered journal").unwrap();
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Corrupt(_))
    ));
    assert!(!path.exists());
    assert_eq!(fs::read(&wal).unwrap(), b"unrecovered journal");
}

#[test]
fn future_schema_is_rejected_without_changing_version_or_existing_data() {
    let directory = TestDirectory::new();
    let path = directory.database();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 3;
             CREATE TABLE future_data (value TEXT NOT NULL);
             INSERT INTO future_data VALUES ('preserve this');",
        )
        .unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::SchemaVersion {
            found: 3,
            supported: 2
        })
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        conn.query_row("SELECT value FROM future_data", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "preserve this"
    );
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
}

#[test]
fn unversioned_existing_schema_is_not_adopted_or_reinitialized() {
    let directory = TestDirectory::new();
    let path = directory.database();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sqlitebackup (value TEXT); INSERT INTO sqlitebackup VALUES ('keep');",
        )
        .unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Corrupt(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("SELECT value FROM sqlitebackup", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "keep"
    );
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn missing_schema_columns_are_not_repaired_silently() {
    let directory = TestDirectory::new();
    let path = directory.database();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 1; CREATE TABLE activities (id TEXT);")
            .unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Database(_))
    ));
    let conn = Connection::open(&path).unwrap();
    let columns: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('activities')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 1);
}

#[test]
fn malformed_resources_fail_reads_and_updates_instead_of_becoming_empty() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let created = service.create(7, draft()).unwrap();
    for value in [
        "not JSON",
        r#"[{"label":"Draft","reference":"inert","unknown":true}]"#,
    ] {
        service
            .lock()
            .unwrap()
            .execute(
                "UPDATE activities SET resources_json = ?1 WHERE id = ?2",
                params![value, created.id],
            )
            .unwrap();
        assert!(matches!(
            service.get(7, &created.id),
            Err(ActivityError::Serialization(_))
        ));
        assert!(matches!(
            service.list(7, None, 100),
            Err(ActivityError::Serialization(_))
        ));
        assert!(matches!(
            service.update(7, &created.id, title_patch("Not a repair")),
            Err(ActivityError::Serialization(_))
        ));
        assert!(service.list(8, None, 100).unwrap().is_empty());
    }
}

#[test]
fn invalid_stored_metadata_is_not_returned_or_implicitly_repaired() {
    for (column, value) in [
        ("title", "   "),
        ("title", " untrimmed "),
        ("goal", "invalid\0"),
        ("state", "running"),
        ("completion_note", "not a completed activity"),
        ("created_at", "not a timestamp"),
        ("updated_at", "2000-01-01T00:00:00.000000000Z"),
        ("updated_at", "2026-01-01T00:00:00+01:00"),
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let created = service.create(7, draft()).unwrap();
        service
            .lock()
            .unwrap()
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute(
                &format!("UPDATE activities SET {column} = ?1 WHERE id = ?2"),
                params![value, created.id],
            )
            .unwrap();
        assert!(matches!(
            service.get(7, &created.id),
            Err(ActivityError::Corrupt(_))
        ));
        assert!(matches!(
            service.list(7, None, 100),
            Err(ActivityError::Corrupt(_))
        ));
        assert!(matches!(
            service.update(7, &created.id, title_patch("Not a repair")),
            Err(ActivityError::Corrupt(_))
        ));
    }
}

#[test]
fn unavailable_parent_is_an_explicit_io_error() {
    let directory = TestDirectory::new();
    let parent = directory.0.join("not-a-directory");
    fs::write(&parent, b"preserve").unwrap();
    assert!(matches!(
        SqliteActivityService::open(parent.join("activities.db")),
        Err(ActivityError::Io(_))
    ));
    assert_eq!(fs::read(parent).unwrap(), b"preserve");
}

#[test]
fn disk_provider_uses_full_sync_wal_busy_timeout_and_current_schema() {
    let directory = TestDirectory::new();
    let service = SqliteActivityService::open(directory.database()).unwrap();
    let conn = service.lock().unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    for (pragma, expected) in [
        ("synchronous", 2),
        ("busy_timeout", 5000),
        ("user_version", i64::from(DATABASE_SCHEMA_VERSION)),
        ("foreign_keys", 1),
        ("temp_store", 2),
    ] {
        assert_eq!(
            conn.pragma_query_value(None, pragma, |row| row.get::<_, i64>(0))
                .unwrap(),
            expected
        );
    }
}

#[cfg(unix)]
fn unix_modes_supported(directory: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let probe = directory.join("mode-probe");
    fs::write(&probe, []).unwrap();
    crate::storage::set_private_file(&probe).unwrap();
    let supported = fs::metadata(&probe).unwrap().permissions().mode() & 0o777 == 0o600;
    fs::remove_file(probe).unwrap();
    if !supported {
        eprintln!("filesystem does not support Unix private modes; skipping mode assertions");
    }
    supported
}

#[cfg(unix)]
#[test]
fn database_and_sidecars_are_private_without_breaking_daemon_traversal() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TestDirectory::new();
    if !unix_modes_supported(&directory.0) {
        return;
    }
    let private = directory.0.join("private");
    let path = private.join("activities.db");
    let service = SqliteActivityService::open(&path).unwrap();
    service.create(7, draft()).unwrap();
    assert_eq!(
        fs::metadata(&private).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for file in [
        path.clone(),
        sidecar_path(&path, "-wal"),
        sidecar_path(&path, "-shm"),
    ] {
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    fs::set_permissions(&private, fs::Permissions::from_mode(0o711)).unwrap();
    let second = SqliteActivityService::open(&path).unwrap();
    assert_eq!(
        fs::metadata(&private).unwrap().permissions().mode() & 0o777,
        0o711
    );
    assert_eq!(second.list(7, None, 10).unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn symlink_database_parent_and_sidecars_are_rejected() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let target = directory.0.join("target");
    fs::write(&target, b"preserve").unwrap();
    let database = directory.database();
    symlink("target", &database).unwrap();
    assert!(matches!(
        SqliteActivityService::open(&database),
        Err(ActivityError::Io(_))
    ));
    assert_eq!(fs::read(&target).unwrap(), b"preserve");
    fs::remove_file(&database).unwrap();

    let parent_link = directory.0.join("parent-link");
    symlink(".", &parent_link).unwrap();
    assert!(matches!(
        SqliteActivityService::open(parent_link.join("activities.db")),
        Err(ActivityError::Io(_))
    ));
    let wal = sidecar_path(&database, "-wal");
    symlink("target", &wal).unwrap();
    assert!(matches!(
        SqliteActivityService::open(&database),
        Err(ActivityError::Io(_))
    ));
    assert_eq!(fs::read(&target).unwrap(), b"preserve");
}
