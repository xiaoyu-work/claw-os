pub(super) fn receipt_report() -> ReceiptReport {
    ReceiptReport {
        id: uuid::Uuid::new_v4().to_string(),
        app_id: "sample".to_string(),
        operation: "read".to_string(),
        package_digest: format!("sha256:{}", "b".repeat(64)),
        outcome: super::super::ReceiptOutcome::Returned,
        result: Some(super::super::ResultSummary {
            kind: super::super::ResultKind::Text,
            sha256: format!("sha256:{}", "a".repeat(64)),
            bytes: 7,
            preview: "[redacted]".to_string(),
            preview_truncated: true,
        }),
        error: None,
    }
}

pub(super) fn receipt_declaration() -> ReceiptDeclaration {
    ReceiptDeclaration {
        app_version: "1.0.0".to_string(),
        operation_label: "Read".to_string(),
        effects: vec![super::super::ReceiptEffect {
            kind: crate::caps::manifest::EffectKind::Read,
            label: "App-declared read".to_string(),
            recovery: crate::caps::manifest::EffectRecovery::Unknown,
            target_arg: Some("path".to_string()),
        }],
    }
}

pub(super) fn unverified_receipt(
    service: &dyn ActivityService,
    owner: u32,
    activity: &str,
    report: ReceiptReport,
) -> Result<ActivityReceipt, ActivityError> {
    service.record_receipt(
        owner,
        activity,
        report,
        None,
        Some("Current signed package does not match the reported digest".into()),
    )
}

#[test]
fn receipt_storage_version_is_independent_of_the_activity_wire_contract() {
    assert_eq!(super::super::SCHEMA_VERSION, 1);
    assert_eq!(DATABASE_SCHEMA_VERSION, 3);
    let service = SqliteActivityService::open_in_memory().unwrap();
    let version: u32 = service
        .lock()
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, DATABASE_SCHEMA_VERSION);
}

#[test]
fn invalid_declaration_can_be_reported_without_losing_execution_metadata() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(7, draft()).unwrap();
    let report = receipt_report();
    let mut declaration = receipt_declaration();
    declaration.operation_label = "x".repeat(513);
    let error = declaration.validate().unwrap_err().to_string();
    let receipt = service
        .record_receipt(7, &activity.id, report.clone(), None, Some(error.clone()))
        .unwrap();
    assert_eq!(receipt.report, report);
    assert!(receipt.declaration.is_none());
    assert_eq!(receipt.declaration_error.as_deref(), Some(error.as_str()));
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn receipt_persistence_preserves_caller_source_and_manifest_distinction() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let (activity, expected) = {
        let service = SqliteActivityService::open(&path).unwrap();
        let activity = service.create(7, draft()).unwrap();
        let report = receipt_report();
        let receipt = service
            .record_receipt(
                7,
                &activity.id,
                report.clone(),
                Some(receipt_declaration()),
                None,
            )
            .unwrap();
        assert_eq!(receipt.id, report.id);
        assert_eq!(receipt.owner_uid, 7);
        assert_eq!(receipt.source, ReceiptSource::CallerReported);
        assert_eq!(receipt.report, report);
        assert!(DateTime::parse_from_rfc3339(&receipt.received_at).is_ok());
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
        (activity, receipt)
    };
    let reopened = SqliteActivityService::open(&path).unwrap();
    assert_eq!(
        reopened.receipts(7, &activity.id, 0).unwrap(),
        vec![expected]
    );
    assert_eq!(reopened.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn receipts_are_owner_scoped_including_root_and_uuid_keys_are_per_owner() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let report = receipt_report();
    let owners = [0, 7, 8];
    let activities: Vec<_> = owners
        .iter()
        .map(|owner| service.create(*owner, draft()).unwrap())
        .collect();
    let missing = uuid::Uuid::new_v4().to_string();
    for activity in &activities {
        let receipt =
            unverified_receipt(&service, activity.owner_uid, &activity.id, report.clone()).unwrap();
        assert_eq!(receipt.id, report.id);
        for foreign in owners
            .into_iter()
            .filter(|owner| *owner != activity.owner_uid)
        {
            for id in [&activity.id, &missing] {
                assert!(matches!(
                    service.receipts(foreign, id, 100),
                    Err(ActivityError::NotFound)
                ));
                assert!(matches!(
                    unverified_receipt(&service, foreign, id, report.clone()),
                    Err(ActivityError::NotFound)
                ));
            }
        }
    }
    for activity in activities {
        assert_eq!(
            service
                .receipts(activity.owner_uid, &activity.id, 100)
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn receipt_retry_returns_the_original_record_and_conflicting_keys_do_not_mutate_it() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(7, draft()).unwrap();
    let report = receipt_report();
    let original = service
        .record_receipt(
            7,
            &activity.id,
            report.clone(),
            Some(receipt_declaration()),
            None,
        )
        .unwrap();
    for id in [
        report.id.to_uppercase(),
        uuid::Uuid::parse_str(&report.id)
            .unwrap()
            .simple()
            .to_string(),
        uuid::Uuid::parse_str(&report.id).unwrap().urn().to_string(),
    ] {
        let mut retry = report.clone();
        retry.id = id;
        assert_eq!(
            unverified_receipt(&service, 7, &activity.id.to_uppercase(), retry).unwrap(),
            original
        );
    }
    let mut changed = report.clone();
    changed.result.as_mut().unwrap().preview = "different caller report".to_string();
    assert!(matches!(
        unverified_receipt(&service, 7, &activity.id, changed),
        Err(ActivityError::Conflict(_))
    ));
    let other = service.create(7, draft()).unwrap();
    assert!(matches!(
        unverified_receipt(&service, 7, &other.id, report),
        Err(ActivityError::Conflict(_))
    ));
    assert!(service.receipts(7, &other.id, 100).unwrap().is_empty());
    assert_eq!(
        service.receipts(7, &activity.id, 100).unwrap(),
        vec![original]
    );
}

#[test]
fn receipt_retry_does_not_upgrade_a_previously_unverified_declaration() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(7, draft()).unwrap();
    let report = receipt_report();
    let original = unverified_receipt(&service, 7, &activity.id, report.clone()).unwrap();
    let retry = service
        .record_receipt(7, &activity.id, report, Some(receipt_declaration()), None)
        .unwrap();
    assert_eq!(retry, original);
    assert!(retry.declaration.is_none());
    assert!(retry.declaration_error.is_some());
}

#[test]
fn ignored_or_rewritten_receipt_inserts_never_report_success() {
    for trigger in [
        "CREATE TRIGGER ignore_receipt BEFORE INSERT ON activity_receipts
         BEGIN SELECT RAISE(IGNORE); END;",
        "CREATE TRIGGER rewrite_receipt AFTER INSERT ON activity_receipts
         BEGIN UPDATE activity_receipts SET declaration_error = 'rewritten' WHERE sequence = NEW.sequence; END;",
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let activity = service.create(7, draft()).unwrap();
        service.lock().unwrap().execute_batch(trigger).unwrap();
        assert!(matches!(
            unverified_receipt(&service, 7, &activity.id, receipt_report()),
            Err(ActivityError::Corrupt(_))
        ));
        assert!(service.receipts(7, &activity.id, 100).unwrap().is_empty());
    }
}

#[test]
fn late_receipts_never_change_activity_state_or_confirmation() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    for state in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let mut activity = service.create(7, draft()).unwrap();
        if state != ActivityState::Active {
            activity = service
                .transition(
                    7,
                    &activity.id,
                    state,
                    (state == ActivityState::Completed)
                        .then(|| "Explicit user confirmation".into()),
                )
                .unwrap();
        }
        for outcome in [
            super::super::ReceiptOutcome::Returned,
            super::super::ReceiptOutcome::ReportedError,
            super::super::ReceiptOutcome::Indeterminate,
        ] {
            let mut report = receipt_report();
            report.outcome = outcome;
            if outcome != super::super::ReceiptOutcome::Returned {
                report.error = Some("Caller reports uncertain or failed execution".into());
            }
            if outcome == super::super::ReceiptOutcome::Indeterminate {
                report.result = None;
            }
            let receipt = unverified_receipt(&service, 7, &activity.id, report).unwrap();
            assert_eq!(receipt.source, ReceiptSource::CallerReported);
        }
        assert_eq!(service.receipts(7, &activity.id, 100).unwrap().len(), 3);
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
    }
}

#[test]
fn receipt_limits_are_per_activity_and_retries_work_at_the_limit() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(7, draft()).unwrap();
    let first = unverified_receipt(&service, 7, &activity.id, receipt_report()).unwrap();
    let mut last = first.clone();
    for _ in 1..MAX_RECEIPTS_PER_ACTIVITY {
        last = unverified_receipt(&service, 7, &activity.id, receipt_report()).unwrap();
    }
    assert!(matches!(
        unverified_receipt(&service, 7, &activity.id, receipt_report()),
        Err(ActivityError::LimitReached)
    ));
    assert_eq!(
        unverified_receipt(&service, 7, &activity.id, first.report.clone()).unwrap(),
        first
    );
    assert_eq!(
        service.receipts(7, &activity.id, 0).unwrap().len(),
        DEFAULT_LIST_LIMIT
    );
    assert_eq!(
        service.receipts(7, &activity.id, usize::MAX).unwrap().len(),
        MAX_LIST_LIMIT
    );
    assert_eq!(service.receipts(7, &activity.id, 1).unwrap(), vec![last]);
    let other = service.create(7, draft()).unwrap();
    unverified_receipt(&service, 7, &other.id, receipt_report()).unwrap();
}

#[test]
fn concurrent_receipt_retries_share_one_immutable_record() {
    let directory = TestDirectory::new();
    let first = SqliteActivityService::open(directory.database()).unwrap();
    let second = SqliteActivityService::open(directory.database()).unwrap();
    let activity = first.create(7, draft()).unwrap();
    let report = receipt_report();
    let activity_id = activity.id.clone();
    let copy = report.clone();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let other_barrier = Arc::clone(&barrier);
    let writer = std::thread::spawn(move || {
        other_barrier.wait();
        unverified_receipt(&second, 7, &activity_id, copy).unwrap()
    });
    barrier.wait();
    let original = first
        .record_receipt(7, &activity.id, report, Some(receipt_declaration()), None)
        .unwrap();
    assert_eq!(writer.join().unwrap(), original);
    assert_eq!(
        first.receipts(7, &activity.id, 100).unwrap(),
        vec![original]
    );
}

#[test]
fn invalid_receipts_fail_without_writing_or_repairing_data() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(7, draft()).unwrap();
    let mut invalid = receipt_report();
    invalid.result.as_mut().unwrap().preview = "\0".into();
    assert!(matches!(
        unverified_receipt(&service, 7, &activity.id, invalid),
        Err(ActivityError::Invalid(_))
    ));
    for (declaration, error) in [
        (None, None),
        (
            Some(receipt_declaration()),
            Some("both present".to_string()),
        ),
        (None, Some("x".repeat(2049))),
    ] {
        assert!(matches!(
            service.record_receipt(7, &activity.id, receipt_report(), declaration, error),
            Err(ActivityError::Invalid(_))
        ));
    }
    assert!(matches!(
        service.receipts(7, "invalid-uuid", 100),
        Err(ActivityError::Invalid(_))
    ));
    assert!(service.receipts(7, &activity.id, 100).unwrap().is_empty());
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn receipt_database_write_errors_and_poisoned_locks_are_explicit() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(7, draft()).unwrap();
    service
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_receipt BEFORE INSERT ON activity_receipts
         BEGIN SELECT RAISE(ABORT, 'receipt write failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        unverified_receipt(&service, 7, &activity.id, receipt_report()),
        Err(ActivityError::Database(_))
    ));
    assert!(service.receipts(7, &activity.id, 100).unwrap().is_empty());
    let clone = service.clone();
    assert!(std::thread::spawn(move || {
        let _guard = clone.conn.lock().unwrap();
        panic!("poison receipt database lock");
    })
    .join()
    .is_err());
    assert!(matches!(
        unverified_receipt(&service, 7, &activity.id, receipt_report()),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.receipts(7, &activity.id, 100),
        Err(ActivityError::Poisoned)
    ));
}

#[test]
fn corrupt_receipt_target_arguments_cannot_be_read_or_repaired_by_retry() {
    for target in [String::new(), "x".repeat(129), "path\n".to_string()] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let activity = service.create(7, draft()).unwrap();
        let report = receipt_report();
        let mut declaration = receipt_declaration();
        service
            .record_receipt(
                7,
                &activity.id,
                report.clone(),
                Some(declaration.clone()),
                None,
            )
            .unwrap();
        declaration.effects[0].target_arg = Some(target);
        let invalid_json = serde_json::to_string(&declaration).unwrap();
        service
            .lock()
            .unwrap()
            .execute(
                "UPDATE activity_receipts SET declaration_json = ?1",
                [&invalid_json],
            )
            .unwrap();
        assert!(matches!(
            service.receipts(7, &activity.id, 100),
            Err(ActivityError::Corrupt(_))
        ));
        assert!(matches!(
            unverified_receipt(&service, 7, &activity.id, report),
            Err(ActivityError::Corrupt(_))
        ));
        let stored: String = service
            .lock()
            .unwrap()
            .query_row(
                "SELECT declaration_json FROM activity_receipts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, invalid_json);
    }
}

#[test]
fn corrupt_receipt_rows_fail_reads_and_cannot_be_repaired_by_retry() {
    for (field, value) in [
        ("source", "os_confirmed".to_string()),
        ("received_at", "not a timestamp".to_string()),
        ("report_json", "{}".to_string()),
        ("report_json", "not JSON".to_string()),
        ("declaration_json", "{}".to_string()),
        (
            "declaration_error",
            "unexpected second declaration".to_string(),
        ),
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let activity = service.create(7, draft()).unwrap();
        let report = receipt_report();
        service
            .record_receipt(
                7,
                &activity.id,
                report.clone(),
                Some(receipt_declaration()),
                None,
            )
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute(
                &format!("UPDATE activity_receipts SET {field} = ?1"),
                [value],
            )
            .unwrap();
        assert!(matches!(
            service.receipts(7, &activity.id, 100),
            Err(ActivityError::Corrupt(_) | ActivityError::Serialization(_))
        ));
        assert!(matches!(
            unverified_receipt(&service, 7, &activity.id, report),
            Err(ActivityError::Corrupt(_) | ActivityError::Serialization(_))
        ));
    }
    for change in ["id", "digest", "size", "preview"] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let activity = service.create(7, draft()).unwrap();
        let report = receipt_report();
        unverified_receipt(&service, 7, &activity.id, report.clone()).unwrap();
        let mut value = serde_json::to_value(&report).unwrap();
        match change {
            "id" => value["id"] = serde_json::json!(uuid::Uuid::new_v4().to_string()),
            "digest" => value["package_digest"] = serde_json::json!("invalid"),
            "size" => value["result"]["bytes"] = serde_json::json!(16 * 1024 * 1024 + 1),
            _ => value["result"]["preview"] = serde_json::json!("x".repeat(2049)),
        }
        service
            .lock()
            .unwrap()
            .execute(
                "UPDATE activity_receipts SET report_json = ?1",
                [serde_json::to_string(&value).unwrap()],
            )
            .unwrap();
        assert!(matches!(
            service.receipts(7, &activity.id, 100),
            Err(ActivityError::Corrupt(_))
        ));
    }
}

pub(super) fn schema_one_database(path: &Path) -> Vec<Activity> {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    let mut records = Vec::new();
    for (owner, state) in [
        (0, ActivityState::Active),
        (7, ActivityState::Paused),
        (7, ActivityState::Completed),
        (8, ActivityState::Cancelled),
    ] {
        let draft = draft();
        let activity = Activity {
            id: uuid::Uuid::new_v4().to_string(),
            owner_uid: owner,
            title: draft.title,
            goal: draft.goal,
            completion_criteria: draft.completion_criteria,
            boundaries: draft.boundaries,
            resources: draft.resources,
            state,
            completion_note: (state == ActivityState::Completed).then(|| "User confirmed".into()),
            created_at: "2026-01-01T00:00:00.000000000Z".into(),
            updated_at: "2026-01-02T00:00:00.000000000Z".into(),
        };
        conn.execute(
            "INSERT INTO activities (
                id, owner_uid, title, goal, completion_criteria, boundaries, resources_json,
                state, completion_note, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                activity.id,
                owner,
                activity.title,
                activity.goal,
                activity.completion_criteria,
                activity.boundaries,
                serde_json::to_string(&activity.resources).unwrap(),
                state.as_str(),
                activity.completion_note,
                activity.created_at,
                activity.updated_at,
            ],
        )
        .unwrap();
        records.push(activity);
    }
    records
}

#[test]
fn receipt_schema_migration_preserves_every_activity_field_and_owner() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let records = schema_one_database(&path);
    let service = SqliteActivityService::open(&path).unwrap();
    assert_eq!(
        service
            .lock()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        DATABASE_SCHEMA_VERSION
    );
    for activity in records {
        assert_eq!(
            service.get(activity.owner_uid, &activity.id).unwrap(),
            activity
        );
        assert!(service
            .receipts(activity.owner_uid, &activity.id, 100)
            .unwrap()
            .is_empty());
        unverified_receipt(&service, activity.owner_uid, &activity.id, receipt_report()).unwrap();
        assert_eq!(
            service.get(activity.owner_uid, &activity.id).unwrap(),
            activity
        );
    }
}

#[test]
fn receipt_schema_migration_failure_rolls_back_schema_and_version() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let records = schema_one_database(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE activity_receipts (sentinel TEXT);
             INSERT INTO activity_receipts VALUES ('preserve');",
        )
        .unwrap();
    }

    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Database(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'activities_owner_id'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT sentinel FROM activity_receipts", [], |row| row
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "preserve"
    );
    for activity in records {
        assert_eq!(
            load_activity(&conn, activity.owner_uid, &activity.id).unwrap(),
            activity
        );
    }
}

#[test]
fn receipt_schema_rejects_unsupported_old_versions_without_resetting_data() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let records = schema_one_database(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", -1).unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::SchemaVersion {
            found: -1,
            supported: 3
        })
    ));
    let conn = Connection::open(&path).unwrap();
    for activity in records {
        assert_eq!(
            load_activity(&conn, activity.owner_uid, &activity.id).unwrap(),
            activity
        );
    }
}

#[test]
fn receipt_schema_migration_crash_worker() {
    let Some(path) = std::env::var_os("COS_TEST_RECEIPT_MIGRATION_DB") else {
        return;
    };
    let mut conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "synchronous", "FULL").unwrap();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    tx.execute_batch(MIGRATE_TO_V2).unwrap();
    tx.pragma_update(None, "user_version", 2).unwrap();
    tx.cache_flush().unwrap();
    std::process::exit(91);
}

#[test]
fn receipt_schema_migration_recovers_after_process_exit_before_commit() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let records = schema_one_database(&path);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "activities::sqlite::tests::receipt_schema_migration_crash_worker",
            "--test-threads=1",
        ])
        .env("COS_TEST_RECEIPT_MIGRATION_DB", &path)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(91),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    {
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'activity_receipts'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    let service = SqliteActivityService::open(&path).unwrap();
    for activity in records {
        assert_eq!(
            service.get(activity.owner_uid, &activity.id).unwrap(),
            activity
        );
    }
}

#[test]
fn receipt_foreign_keys_reject_cross_owner_links_and_detect_corruption_on_reopen() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let activity = {
        let service = SqliteActivityService::open(&path).unwrap();
        let activity = service.create(7, draft()).unwrap();
        unverified_receipt(&service, 7, &activity.id, receipt_report()).unwrap();
        assert!(service
            .lock()
            .unwrap()
            .execute("UPDATE activity_receipts SET owner_uid = 0", [],)
            .is_err());
        activity
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute("UPDATE activity_receipts SET owner_uid = 0", [])
            .unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Corrupt(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(load_activity(&conn, 7, &activity.id).unwrap(), activity);
}
