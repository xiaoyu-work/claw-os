use super::*;
use crate::agent::memory::sqlite_fts::MemoryDb;

fn database_path() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("memory.db");
    (directory, path)
}

fn create_message_database(path: &Path, content: &str) {
    let db = MemoryDb::open(path).expect("open memory db");
    db.record_message("session", "user", content)
        .expect("record message");
}

#[test]
fn clean_database_reports_separate_health_checks() {
    let (_directory, path) = database_path();
    create_message_database(&path, "healthy searchable text");

    let report = diagnose(&path).expect("diagnose");
    assert_eq!(report.status, "ok");
    assert_eq!(report.sqlite.status, "ok");
    assert_eq!(report.wal.status, "ok");
    assert_eq!(report.schema.status, "ok");
    assert_eq!(report.fts.status, "ok");
    assert_eq!(report.prompt_references.status, "ok");
    assert_eq!(report.prompt_hashes.status, "ok");
    assert_eq!(report.titles.status, "ok");
    assert_eq!(report.repair_lifecycle.status, "ok");
    assert_eq!(report.stats.expect("stats").total_messages, 1);
}

#[test]
fn corrupt_fts_is_detected_dry_run_is_read_only_and_rebuild_restores_search() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    let row_id = db
        .record_message("session", "user", "authoritative pineapple")
        .expect("record");
    {
        let conn = db.lock_conn().expect("connection");
        conn.execute(
            "INSERT INTO messages_fts(messages_fts, rowid, content)
             VALUES('delete', ?, ?)",
            params![row_id, "authoritative pineapple"],
        )
        .expect("delete old index terms");
        conn.execute(
            "INSERT INTO messages_fts(rowid, content) VALUES(?, ?)",
            params![row_id, "incorrect projection"],
        )
        .expect("insert corrupt index terms");
    }
    drop(db);

    let unhealthy = diagnose(&path).expect("diagnose corrupt fts");
    assert_eq!(unhealthy.sqlite.status, "ok");
    assert_eq!(unhealthy.fts.status, "fail");
    assert!(!unhealthy.requires_quarantine);

    let dry_run = repair(
        &path,
        RepairOptions {
            dry_run: true,
            ..RepairOptions::default()
        },
    )
    .expect("dry run");
    assert_eq!(dry_run.status, "planned");
    assert!(!dry_run.changed);
    assert!(dry_run
        .actions
        .iter()
        .any(|action| action == "rebuild_fts_and_triggers"));
    assert!(
        !repair_log_path(&path).exists(),
        "dry-run must not create repair lifecycle state"
    );
    let still_corrupt = MemoryDb::open(&path).expect("open after dry run");
    assert!(still_corrupt
        .search("pineapple", 10)
        .expect("search")
        .is_empty());
    drop(still_corrupt);

    let applied = repair(&path, RepairOptions::default()).expect("repair fts");
    assert_eq!(applied.status, "ok");
    let repaired = MemoryDb::open(&path).expect("open repaired");
    assert_eq!(repaired.search("pineapple", 10).expect("search").len(), 1);
    assert!(repaired.search("incorrect", 10).expect("search").is_empty());
}

#[test]
fn missing_trigger_requires_explicit_repair_and_is_restored() {
    let (_directory, path) = database_path();
    create_message_database(&path, "first indexed row");
    let conn = Connection::open(&path).expect("raw connection");
    conn.execute_batch("DROP TRIGGER messages_ai;")
        .expect("drop trigger");
    drop(conn);

    let health = diagnose(&path).expect("diagnose");
    assert_eq!(health.fts.status, "fail");
    assert!(health
        .fts
        .issues
        .iter()
        .any(|issue| issue.contains("messages_ai")));
    let open_error = MemoryDb::open(&path).expect_err("runtime open must not self-heal");
    assert!(open_error.is_integrity_failure());

    repair(&path, RepairOptions::default()).expect("repair trigger");
    let repaired = MemoryDb::open(&path).expect("open repaired");
    repaired
        .record_message("session", "user", "second searchable row")
        .expect("record after repair");
    assert_eq!(repaired.search("second", 10).expect("search").len(), 1);
}

#[test]
fn orphaned_title_and_prompt_projections_are_removed_transactionally() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    db.record_message("live", "user", "authoritative")
        .expect("record");
    db.set_title("orphan", "No backing messages")
        .expect("title");
    db.freeze_system_prompt("orphan", "valid but orphaned", 1)
        .expect("prompt");
    drop(db);

    let health = diagnose(&path).expect("diagnose");
    assert_eq!(health.titles.status, "warn");
    assert_eq!(health.prompt_references.status, "warn");
    assert!(!health.requires_quarantine);

    repair(&path, RepairOptions::default()).expect("repair projections");
    let repaired = MemoryDb::open(&path).expect("open repaired");
    assert!(repaired
        .title_for("orphan")
        .expect("title lookup")
        .is_none());
    assert!(repaired
        .system_prompt_for("orphan", 1)
        .expect("prompt lookup")
        .is_none());
    assert_eq!(repaired.count_session("live").expect("live count"), 1);
}

#[test]
fn dangling_prompt_reference_is_detected_and_never_returned() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    db.record_message("session", "user", "history")
        .expect("record");
    db.freeze_system_prompt("session", "trusted prompt", 1)
        .expect("freeze");
    {
        let conn = db.lock_conn().expect("connection");
        conn.execute_batch("PRAGMA foreign_keys = OFF;")
            .expect("disable foreign keys");
        conn.execute(
            "UPDATE session_system_prompts
             SET prompt_hash = 'missing-prompt'
             WHERE session_id = 'session'",
            [],
        )
        .expect("damage reference");
    }

    let error = db
        .system_prompt_for("session", 1)
        .expect_err("dangling prompt must fail");
    assert!(error.is_integrity_failure());
    drop(db);

    let health = diagnose(&path).expect("diagnose");
    assert_eq!(health.prompt_references.status, "fail");
    assert!(health.requires_quarantine);
    let preview = repair(
        &path,
        RepairOptions {
            dry_run: true,
            ..RepairOptions::default()
        },
    )
    .expect("preview");
    assert_eq!(preview.status, "requires_quarantine");
}

#[test]
fn wrong_prompt_hash_is_quarantined_while_recoverable_messages_survive() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    db.record_message("session", "user", "recoverable kiwi")
        .expect("record");
    db.freeze_system_prompt("session", "trusted prompt", 1)
        .expect("freeze");
    {
        let conn = db.lock_conn().expect("connection");
        conn.execute("UPDATE system_prompts SET prompt = 'tampered prompt'", [])
            .expect("damage prompt");
    }
    let error = db
        .system_prompt_for("session", 1)
        .expect_err("hash mismatch must fail");
    assert!(error.is_integrity_failure());
    drop(db);

    let health = diagnose(&path).expect("diagnose");
    assert_eq!(health.prompt_hashes.status, "fail");
    assert!(health.requires_quarantine);
    let refusal = repair(&path, RepairOptions::default()).expect_err("consent required");
    assert!(refusal.to_string().contains("--quarantine --yes"));

    let repaired = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect("quarantine repair");
    let quarantine = PathBuf::from(
        repaired
            .quarantine_path
            .as_deref()
            .expect("quarantine path"),
    );
    assert!(quarantine.exists());
    assert_eq!(repaired.recovered.messages, 1);
    assert_eq!(repaired.recovered.skipped_prompt_references, 1);
    let repair_log = fs::read_to_string(repair_log_path(&path)).expect("repair log");
    assert!(!repair_log.contains("recoverable kiwi"));
    assert!(!repair_log.contains("trusted prompt"));
    assert!(!repair_log.contains("tampered prompt"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&quarantine)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let replacement = MemoryDb::open(&path).expect("replacement");
    assert_eq!(replacement.count_total().expect("count"), 1);
    assert_eq!(replacement.search("kiwi", 10).expect("search").len(), 1);
    assert!(replacement
        .system_prompt_for("session", 1)
        .expect("prompt query")
        .is_none());
}

#[test]
fn malformed_wal_is_quarantined_without_trusting_its_contents() {
    let (_directory, path) = database_path();
    create_message_database(&path, "base row");
    let wal = wal_path(&path);
    fs::write(&wal, [0_u8; 32]).expect("write malformed wal");

    let health = diagnose(&path).expect("diagnose");
    assert_eq!(health.wal.status, "fail");
    assert!(health.requires_quarantine);

    let repaired = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect("quarantine malformed wal");
    assert!(repaired
        .quarantined_files
        .iter()
        .any(|value| value.ends_with("-wal")));
    let replacement = MemoryDb::open(&path).expect("replacement");
    assert_eq!(replacement.count_total().expect("count"), 0);
}

#[test]
fn unreadable_sqlite_file_is_preserved_and_replaced() {
    let (_directory, path) = database_path();
    fs::write(&path, b"not a sqlite database").expect("write corrupt database");

    let health = diagnose(&path).expect("diagnose");
    assert_eq!(health.sqlite.status, "fail");
    assert!(health.requires_quarantine);

    let repaired = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect("replace corrupt database");
    let quarantine = PathBuf::from(repaired.quarantine_path.expect("quarantine"));
    assert_eq!(
        fs::read(&quarantine).expect("read quarantine"),
        b"not a sqlite database"
    );
    assert_eq!(
        MemoryDb::open(&path)
            .expect("replacement")
            .count_total()
            .expect("count"),
        0
    );
}

#[test]
fn interrupted_repair_is_detected_and_resumed() {
    let (_directory, path) = database_path();
    create_message_database(&path, "stable row");
    let attempt = RepairEvent {
        version: REPAIR_LOG_VERSION,
        attempt_id: "interrupted-test".to_string(),
        ts_ms: current_ts_ms(),
        phase: RepairPhase::Started,
        mode: RepairMode::InPlace,
        planned_actions: vec!["checkpoint_wal".to_string()],
        quarantine_path: None,
        salvage_source: true,
        recovered: RecoveredRecords::default(),
        error: None,
    };
    append_repair_event(&path, &attempt).expect("record interrupted start");

    let health = diagnose(&path).expect("diagnose");
    assert_eq!(health.repair_lifecycle.status, "fail");

    let report = repair(&path, RepairOptions::default()).expect("resume repair");
    assert!(report.resumed_interrupted_repair);
    assert_eq!(
        report
            .after
            .as_ref()
            .expect("after")
            .repair_lifecycle
            .status,
        "ok"
    );
}

#[test]
fn failed_repair_result_remains_visible_until_a_successful_retry() {
    let (_directory, path) = database_path();
    create_message_database(&path, "stable row");
    let failed = RepairEvent {
        version: REPAIR_LOG_VERSION,
        attempt_id: "failed-test".to_string(),
        ts_ms: current_ts_ms(),
        phase: RepairPhase::Failed,
        mode: RepairMode::InPlace,
        planned_actions: vec!["checkpoint_wal".to_string()],
        quarantine_path: None,
        salvage_source: true,
        recovered: RecoveredRecords::default(),
        error: Some("simulated repair failure".to_string()),
    };
    append_repair_event(&path, &failed).expect("record failed repair");

    let failed_health = diagnose(&path).expect("diagnose failed repair");
    assert_eq!(failed_health.repair_lifecycle.status, "fail");
    assert!(failed_health.repair_lifecycle.issues[0].contains("simulated"));

    repair(&path, RepairOptions::default()).expect("retry");
    assert_eq!(
        diagnose(&path)
            .expect("diagnose retry")
            .repair_lifecycle
            .status,
        "ok"
    );
}

#[cfg(unix)]
#[test]
fn repair_waits_for_open_memory_handles() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    db.record_message("session", "user", "held open")
        .expect("record");
    let repair_path = path.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        started_tx.send(()).expect("signal");
        repair(&repair_path, RepairOptions::default())
    });
    started_rx.recv().expect("started");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !handle.is_finished(),
        "repair must wait while a MemoryDb shared lifecycle lock is held"
    );
    drop(db);
    handle.join().expect("join").expect("repair");
}
