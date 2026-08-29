use super::*;
use crate::agent::memory::sqlite_fts::MemoryDb;
use std::io::{Seek, SeekFrom};

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

fn family_bytes(path: &Path) -> [Option<Vec<u8>>; 3] {
    [path.to_path_buf(), wal_path(path), shm_path(path)].map(|member| fs::read(member).ok())
}

fn create_live_wal(path: &Path) -> Connection {
    ensure_private_database_file(path).expect("private database file");
    let conn = Connection::open(path).expect("raw sqlite");
    initialize_connection(&conn).expect("initialize schema");
    conn.execute_batch("PRAGMA wal_autocheckpoint = 0;")
        .expect("disable autocheckpoint");
    conn.execute(
        "INSERT INTO messages(session_id, role, content, ts_ms)
         VALUES('wal-session', 'user', 'live wal row', 1)",
        [],
    )
    .expect("insert wal row");
    assert!(wal_path(path).metadata().expect("wal metadata").len() > 32);
    assert!(shm_path(path).exists());
    conn
}

struct StandaloneFailpointGuard;

impl Drop for StandaloneFailpointGuard {
    fn drop(&mut self) {
        STANDALONE_RECOVERY_FAILPOINT.with(|value| value.set(None));
    }
}

fn fail_standalone_recovery_at(stage: StandaloneRecoveryStage) -> StandaloneFailpointGuard {
    STANDALONE_RECOVERY_FAILPOINT.with(|value| value.set(Some(stage)));
    StandaloneFailpointGuard
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
    assert_eq!(report.compactions.status, "ok");
    assert_eq!(report.titles.status, "ok");
    assert_eq!(report.repair_lifecycle.status, "ok");
    assert_eq!(report.stats.expect("stats").total_messages, 1);
}

fn complete_test_compaction(db: &MemoryDb, session_id: &str) {
    use crate::agent::memory::compaction::{BeginCompaction, NewCompaction};

    let rows = db.recent_replayable(session_id, 10).expect("rows");
    assert!(rows.len() >= 3);
    let attempt = match db
        .begin_compaction(
            session_id,
            NewCompaction {
                source_start_id: rows[0].id,
                source_end_id: rows[1].id,
                source_count: 2,
                protected_tail_start_id: Some(rows[2].id),
                protected_user_message_id: Some(rows[2].id),
                algorithm: "test".to_string(),
                algorithm_version: 1,
                provider: "mock".to_string(),
                model: "mock-model".to_string(),
                previous_compaction_id: None,
                pruned_tool_results: 0,
            },
        )
        .expect("begin compaction")
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("unexpected compaction result: {other:?}"),
    };
    attempt
        .complete("[CONTEXT SUMMARY]\n\nrecovered summary")
        .expect("complete compaction");
}

#[test]
fn invalid_compaction_projection_is_repaired_without_losing_raw_messages() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    db.record_message("session", "user", "first searchable raw")
        .unwrap();
    db.record_message("session", "assistant", "second raw")
        .unwrap();
    db.record_message("session", "user", "protected tail")
        .unwrap();
    complete_test_compaction(&db, "session");
    {
        let conn = db.lock_conn().unwrap();
        conn.execute(
            "UPDATE compaction_summaries SET summary = 'tampered summary'",
            [],
        )
        .unwrap();
    }
    drop(db);

    let health = diagnose(&path).expect("diagnose");
    assert_eq!(health.compactions.status, "fail");
    assert!(!health.requires_quarantine);
    assert!(health
        .planned_repairs
        .iter()
        .any(|action| action == "repair_compaction_projection"));

    repair(&path, RepairOptions::default()).expect("repair projection");
    let repaired = MemoryDb::open(&path).expect("reopen");
    assert!(repaired
        .latest_valid_compaction("session")
        .unwrap()
        .0
        .is_none());
    assert_eq!(
        repaired
            .search_session("session", "searchable", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn protected_row_mutation_is_detected_and_repaired_by_shared_validation() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    db.record_message("session", "user", "first raw").unwrap();
    db.record_message("session", "assistant", "second raw")
        .unwrap();
    let protected = db
        .record_message("session", "user", "protected user")
        .unwrap();
    complete_test_compaction(&db, "session");
    {
        let conn = db.lock_conn().unwrap();
        conn.execute(
            "UPDATE messages SET content = 'changed protected user' WHERE id = ?",
            params![protected],
        )
        .unwrap();
    }
    drop(db);

    let health = diagnose(&path).expect("diagnose");
    assert_eq!(health.compactions.status, "fail");
    repair(&path, RepairOptions::default()).expect("repair projection");
    let repaired = MemoryDb::open(&path).expect("reopen");
    assert!(repaired.compactions_for_session("session").unwrap().is_empty());
    drop(repaired);
    assert_eq!(diagnose(&path).unwrap().compactions.status, "ok");
}

#[test]
fn quarantine_recovery_preserves_valid_compaction_and_raw_sources() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    db.record_message("session", "user", "first recovery source")
        .unwrap();
    db.record_message("session", "assistant", "second recovery source")
        .unwrap();
    db.record_message("session", "user", "protected tail")
        .unwrap();
    complete_test_compaction(&db, "session");
    db.freeze_system_prompt("session", "trusted prompt", 1)
        .unwrap();
    {
        let conn = db.lock_conn().unwrap();
        conn.execute("UPDATE system_prompts SET prompt = 'tampered prompt'", [])
            .unwrap();
    }
    drop(db);

    let report = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect("quarantine repair");
    assert_eq!(report.recovered.compactions, 1);
    let repaired = MemoryDb::open(&path).expect("replacement");
    let (summary, rejected) = repaired.latest_valid_compaction("session").unwrap();
    assert_eq!(rejected, 0);
    assert!(summary
        .expect("recovered summary")
        .summary
        .contains("recovered summary"));
    assert_eq!(
        repaired
            .search_session("session", "recovery source", 10)
            .unwrap()
            .len(),
        2
    );
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

#[cfg(unix)]
#[test]
fn dry_run_does_not_change_live_database_wal_or_shm_bytes() {
    let (_directory, path) = database_path();
    let connection = create_live_wal(&path);
    fs::remove_file(shm_path(&path)).expect("remove live shm before diagnosis");
    let before = family_bytes(&path);
    assert!(before[1].is_some(), "fixture must retain a WAL");
    assert!(before[2].is_none(), "fixture must start without SHM");

    let preview = repair(
        &path,
        RepairOptions {
            dry_run: true,
            ..RepairOptions::default()
        },
    )
    .expect("dry-run");

    assert_eq!(preview.dry_run, true);
    assert_eq!(family_bytes(&path), before);
    drop(connection);
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
        let probe = quarantine.with_extension("permission-probe");
        fs::write(&probe, b"probe").expect("write permission probe");
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o600))
            .expect("set probe permissions");
        let mode_is_enforced = fs::metadata(&probe)
            .expect("probe metadata")
            .permissions()
            .mode()
            & 0o777
            == 0o600;
        fs::remove_file(probe).expect("remove permission probe");
        if mode_is_enforced {
            assert_eq!(
                fs::metadata(&quarantine)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
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
fn damaged_secondary_projections_cannot_rollback_authoritative_message_recovery() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    let first = db
        .record_message("session", "user", "authoritative apricot")
        .expect("record first");
    db.record_message("session", "assistant", "authoritative plum")
        .expect("record second");
    db.set_title("session", "damaged title source")
        .expect("set title");
    db.freeze_system_prompt("session", "trusted prompt", 1)
        .expect("freeze prompt");
    {
        let conn = db.lock_conn().expect("connection");
        conn.execute(
            "INSERT INTO messages_fts(messages_fts, rowid, content)
             VALUES('delete', ?, ?)",
            params![first, "authoritative apricot"],
        )
        .expect("damage fts");
        conn.execute(
            "INSERT INTO messages_fts(rowid, content) VALUES(?, 'wrong index text')",
            params![first],
        )
        .expect("replace fts terms");
        conn.execute_batch(
            "DROP INDEX messages_session_ts;
             UPDATE session_titles SET title = x'80';
             UPDATE system_prompts SET prompt = x'80';",
        )
        .expect("damage optional projections");
    }
    drop(db);

    let health = diagnose(&path).expect("diagnose");
    assert!(health.requires_quarantine);
    let report = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect("recover readable messages");
    assert_eq!(report.recovered.messages, 2);
    assert_eq!(report.recovered.titles, 0);
    assert_eq!(report.recovered.prompt_references, 0);
    assert_eq!(report.recovered.skipped_prompt_references, 0);
    assert!(report
        .recovery_warning
        .as_deref()
        .is_some_and(|warning| warning.contains("session titles")));
    assert!(report
        .recovery_warning
        .as_deref()
        .is_some_and(|warning| warning.contains("session prompts")));

    let replacement = MemoryDb::open(&path).expect("replacement");
    assert_eq!(replacement.count_total().expect("count"), 2);
    assert_eq!(replacement.search("apricot", 10).expect("search").len(), 1);
    assert_eq!(replacement.search("plum", 10).expect("search").len(), 1);
}

#[test]
fn corrupt_secondary_index_with_failed_integrity_check_still_recovers_messages() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    for index in 0..32 {
        db.record_message(
            &format!("session-{index}"),
            "user",
            &format!("authoritative index row {index}"),
        )
        .expect("record");
    }
    drop(db);

    let conn = Connection::open(&path).expect("raw connection");
    checkpoint_wal(&conn).expect("checkpoint");
    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .expect("page size");
    let root_page: i64 = conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = 'messages_session_ts'",
            [],
            |row| row.get(0),
        )
        .expect("index root page");
    drop(conn);

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open database bytes");
    file.seek(SeekFrom::Start(((root_page - 1) * page_size) as u64))
        .expect("seek index page");
    file.write_all(&[0xff]).expect("corrupt index page");
    file.sync_all().expect("sync corruption");
    drop(file);

    let health = diagnose(&path).expect("diagnose");
    assert_eq!(health.sqlite.status, "fail");
    assert!(health.requires_quarantine);
    let report = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect("recover despite secondary index corruption");
    assert_eq!(report.recovered.messages, 32);
    assert!(report
        .recovery_warning
        .as_deref()
        .is_some_and(|warning| warning.contains("global integrity_check")));
    let replacement = MemoryDb::open(&path).expect("replacement");
    assert_eq!(replacement.count_total().expect("count"), 32);
    assert_eq!(
        replacement
            .search("authoritative", 100)
            .expect("search")
            .len(),
        32
    );
}

#[test]
fn readable_message_scan_failure_does_not_install_empty_replacement() {
    let (_directory, path) = database_path();
    let db = MemoryDb::open(&path).expect("open memory db");
    db.record_message("session", "user", "recoverable before bad row")
        .expect("record");
    db.freeze_system_prompt("session", "trusted prompt", 1)
        .expect("freeze");
    {
        let conn = db.lock_conn().expect("connection");
        conn.execute_batch(
            "DROP TRIGGER messages_ai;
             INSERT INTO messages(session_id, role, content, ts_ms)
             VALUES('session', 'user', x'80', 2);
             UPDATE system_prompts SET prompt = 'tampered prompt';",
        )
        .expect("inject unreadable message");
    }
    drop(db);

    let error = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect_err("message scan failure must abort");
    assert!(
        error.to_string().contains("Invalid column type")
            || error.to_string().contains("invalid type"),
        "unexpected error: {error}"
    );
    let log = read_repair_log(&path).expect("repair log");
    let failed = log.last_applied.expect("failed attempt");
    let quarantine = PathBuf::from(failed.quarantine_path.expect("quarantine path"));
    assert!(
        quarantine.exists(),
        "source evidence must remain quarantined"
    );
    assert!(
        !path.exists(),
        "an empty replacement must not be installed after a message scan failure"
    );
}

#[test]
fn operational_standalone_failures_do_not_install_empty_replacements() {
    for (index, stage) in [
        StandaloneRecoveryStage::Copy,
        StandaloneRecoveryStage::Open,
        StandaloneRecoveryStage::Configure,
        StandaloneRecoveryStage::SchemaRead,
    ]
    .into_iter()
    .enumerate()
    {
        let (_directory, path) = database_path();
        let db = MemoryDb::open(&path).expect("open memory db");
        db.record_message("session", "user", "must remain in quarantine")
            .expect("record");
        db.freeze_system_prompt("session", "trusted prompt", 1)
            .expect("freeze");
        {
            let conn = db.lock_conn().expect("connection");
            conn.execute("UPDATE system_prompts SET prompt = 'tampered prompt'", [])
                .expect("force quarantine");
        }
        drop(db);

        let _failpoint = fail_standalone_recovery_at(stage);
        let error = repair(
            &path,
            RepairOptions {
                allow_quarantine: true,
                ..RepairOptions::default()
            },
        )
        .expect_err("operational failure must abort");
        let expected = match stage {
            StandaloneRecoveryStage::Copy => "No space left on device",
            StandaloneRecoveryStage::Open => "Too many open files",
            StandaloneRecoveryStage::Configure => "interrupted",
            StandaloneRecoveryStage::SchemaRead => "temporary I/O failure",
        };
        assert!(
            error.to_string().contains(expected),
            "stage {index} returned unexpected error: {error}"
        );
        let log = read_repair_log(&path).expect("repair log");
        let failed = log.last_applied.expect("failed repair");
        let quarantine = PathBuf::from(failed.quarantine_path.expect("quarantine path"));
        assert!(quarantine.exists(), "quarantined source must survive");
        assert!(
            !path.exists(),
            "operational failure must not install an empty active database"
        );
    }
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
    assert_eq!(replacement.count_total().expect("count"), 1);
    assert_eq!(replacement.search("base", 10).expect("search").len(), 1);
}

#[test]
fn wal_restart_ignores_stale_physical_and_partial_tail() {
    let (_directory, path) = database_path();
    let connection = create_live_wal(&path);
    let old_wal = fs::read(wal_path(&path)).expect("old wal");
    checkpoint_wal(&connection).expect("checkpoint old wal");
    connection
        .execute(
            "INSERT INTO messages(session_id, role, content, ts_ms)
             VALUES('wal-session', 'user', 'after restart', 2)",
            [],
        )
        .expect("insert after restart");

    let current = inspect_wal(&wal_path(&path), &path)
        .expect("current wal")
        .expect("wal");
    let stale_frame_len = 24 + current.page_size as usize;
    let mut file = OpenOptions::new()
        .append(true)
        .open(wal_path(&path))
        .expect("open wal for stale tail");
    file.write_all(&old_wal[32..32 + stale_frame_len])
        .expect("append stale frame");
    file.write_all(&old_wal[32..45])
        .expect("append partial stale frame");
    file.sync_all().expect("sync stale tail");

    let validation = inspect_wal(&wal_path(&path), &path)
        .expect("stale tail is valid")
        .expect("wal");
    assert_eq!(validation.frames, current.frames);
    assert!(validation.physical_frames > validation.frames);
    assert!(validation.stale_tail_bytes > 0);
    let health = diagnose(&path).expect("diagnose stale tail");
    assert_ne!(health.wal.status, "fail");
    repair(&path, RepairOptions::default()).expect("checkpoint logical WAL prefix");
    drop(connection);
    let reopened = MemoryDb::open(&path).expect("reopen");
    assert_eq!(reopened.count_total().expect("count"), 2);
    assert_eq!(reopened.search("restart", 10).expect("search").len(), 1);
}

#[cfg(unix)]
#[test]
fn wal_scan_without_shm_uses_last_committed_prefix() {
    let (_directory, path) = database_path();
    let connection = create_live_wal(&path);
    let old_wal = fs::read(wal_path(&path)).expect("old wal");
    checkpoint_wal(&connection).expect("checkpoint old wal");
    connection
        .execute(
            "INSERT INTO messages(session_id, role, content, ts_ms)
             VALUES('wal-session', 'user', 'new generation', 2)",
            [],
        )
        .expect("insert new generation");
    let logical = inspect_wal(&wal_path(&path), &path)
        .expect("validate wal")
        .expect("wal");
    let stale_frame_len = 24 + logical.page_size as usize;
    let mut file = OpenOptions::new()
        .append(true)
        .open(wal_path(&path))
        .expect("open wal");
    file.write_all(&old_wal[32..32 + stale_frame_len])
        .expect("append stale frame");
    file.write_all(&old_wal[32..41])
        .expect("append partial tail");
    file.sync_all().expect("sync tail");
    fs::remove_file(shm_path(&path)).expect("remove wal index");

    let recovered = inspect_wal(&wal_path(&path), &path)
        .expect("recover logical prefix")
        .expect("wal");
    assert_eq!(recovered.frames, logical.frames);
    assert!(recovered.stale_tail_bytes > 0);
    drop(connection);
}

#[cfg(unix)]
#[test]
fn wal_without_shm_rejects_current_generation_frame_corruption() {
    let (_directory, path) = database_path();
    let connection = create_live_wal(&path);
    let original = fs::read(wal_path(&path)).expect("original wal");
    fs::remove_file(shm_path(&path)).expect("remove wal index");
    let directory = path.parent().expect("parent");

    let mut bad_checksum = original.clone();
    bad_checksum[48] ^= 1;
    let candidate = directory.join("no-shm-bad-checksum.wal");
    fs::write(&candidate, bad_checksum).unwrap();
    assert!(inspect_wal(&candidate, &path)
        .unwrap_err()
        .contains("rolling checksum"));

    let mut bad_page = original.clone();
    bad_page[32..36].copy_from_slice(&0_u32.to_be_bytes());
    rewrite_first_frame_checksum(&mut bad_page);
    let candidate = directory.join("no-shm-bad-page.wal");
    fs::write(&candidate, bad_page).unwrap();
    assert!(inspect_wal(&candidate, &path)
        .unwrap_err()
        .contains("page number"));

    let mut bad_commit = original;
    bad_commit[36..40].copy_from_slice(&u32::MAX.to_be_bytes());
    rewrite_first_frame_checksum(&mut bad_commit);
    let candidate = directory.join("no-shm-bad-commit.wal");
    fs::write(&candidate, bad_commit).unwrap();
    assert!(inspect_wal(&candidate, &path)
        .unwrap_err()
        .contains("commit size"));
    drop(connection);
}

#[test]
fn wal_validation_rejects_version_checksum_salt_page_and_commit_damage() {
    let (_directory, path) = database_path();
    let connection = create_live_wal(&path);
    let original = fs::read(wal_path(&path)).expect("read wal");
    let validation = inspect_wal(&wal_path(&path), &path)
        .expect("validate wal")
        .expect("wal");
    assert!(validation.frames > 0);
    assert!(validation.commits > 0);
    let directory = path.parent().expect("parent");

    let mut bad_version = original.clone();
    bad_version[4..8].copy_from_slice(&(WAL_FORMAT_VERSION + 1).to_be_bytes());
    let candidate = directory.join("bad-version.wal");
    fs::write(&candidate, bad_version).unwrap();
    assert!(inspect_wal(&candidate, &path)
        .unwrap_err()
        .contains("format version"));

    let mut bad_header_checksum = original.clone();
    bad_header_checksum[24] ^= 1;
    let candidate = directory.join("bad-header-checksum.wal");
    fs::write(&candidate, bad_header_checksum).unwrap();
    assert!(inspect_wal(&candidate, &path)
        .unwrap_err()
        .contains("header checksum"));

    let mut bad_salt = original.clone();
    bad_salt[40] ^= 1;
    let candidate = directory.join("bad-salt.wal");
    fs::write(&candidate, bad_salt).unwrap();
    assert!(inspect_wal(&candidate, &path)
        .unwrap_err()
        .contains("salts"));

    let mut bad_page = original.clone();
    bad_page[32..36].copy_from_slice(&0_u32.to_be_bytes());
    rewrite_first_frame_checksum(&mut bad_page);
    let candidate = directory.join("bad-page.wal");
    fs::write(&candidate, bad_page).unwrap();
    assert!(inspect_wal(&candidate, &path)
        .unwrap_err()
        .contains("page number"));

    let mut bad_commit = original.clone();
    bad_commit[36..40].copy_from_slice(&u32::MAX.to_be_bytes());
    rewrite_first_frame_checksum(&mut bad_commit);
    let candidate = directory.join("bad-commit.wal");
    fs::write(&candidate, bad_commit).unwrap();
    assert!(inspect_wal(&candidate, &path)
        .unwrap_err()
        .contains("commit size"));

    let mut bad_frame_checksum = original;
    bad_frame_checksum[48] ^= 1;
    let candidate = directory.join("bad-frame-checksum.wal");
    fs::write(&candidate, bad_frame_checksum).unwrap();
    assert!(inspect_wal(&candidate, &path)
        .unwrap_err()
        .contains("rolling checksum"));
    drop(connection);
}

fn create_quarantine_required_writer(path: &Path) -> Connection {
    let db = MemoryDb::open(path).expect("open memory db");
    db.record_message("session", "user", "checkpointed base")
        .expect("record");
    db.freeze_system_prompt("session", "trusted prompt", 1)
        .expect("freeze");
    {
        let conn = db.lock_conn().expect("connection");
        conn.execute("UPDATE system_prompts SET prompt = 'tampered prompt'", [])
            .expect("damage prompt");
    }
    drop(db);

    let writer = Connection::open(path).expect("open raw writer");
    writer
        .execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .expect("configure writer");
    checkpoint_wal(&writer).expect("checkpoint base");
    writer
}

fn materialize_valid_wal_family(path: &Path) -> (Vec<u8>, Vec<u8>) {
    let writer = create_quarantine_required_writer(path);
    let checkpointed_main = fs::read(path).expect("checkpointed main");
    writer
        .execute(
            "INSERT INTO messages(session_id, role, content, ts_ms)
             VALUES('session', 'user', 'committed wal-only row', 3)",
            [],
        )
        .expect("commit wal-only row");
    let wal = fs::read(wal_path(path)).expect("valid wal");
    let shm = fs::read(shm_path(path)).expect("valid shm");
    drop(writer);

    fs::write(path, checkpointed_main).expect("restore checkpointed main");
    fs::write(wal_path(path), &wal).expect("restore wal");
    fs::write(shm_path(path), &shm).expect("restore shm");
    inspect_wal(&wal_path(path), path)
        .expect("validate restored wal")
        .expect("wal");
    (wal, shm)
}

fn append_interrupted_quarantine(
    path: &Path,
    attempt_id: &str,
    checkpoint_source: bool,
) -> PathBuf {
    let quarantine = quarantine_base(path, attempt_id);
    append_repair_event(
        path,
        &RepairEvent {
            version: REPAIR_LOG_VERSION,
            attempt_id: attempt_id.to_string(),
            ts_ms: current_ts_ms(),
            phase: RepairPhase::Started,
            mode: RepairMode::Quarantine,
            planned_actions: vec!["quarantine_and_initialize_replacement".to_string()],
            quarantine_path: Some(quarantine.display().to_string()),
            checkpoint_source,
            recovered: RecoveredRecords::default(),
            recovery_warning: None,
            error: None,
        },
    )
    .expect("record interrupted quarantine");
    quarantine
}

fn assert_failed_quarantine_did_not_move_database(path: &Path) {
    assert!(path.exists(), "live database must remain in place");
    let log = read_repair_log(path).expect("repair log");
    let failed = log.last_applied.expect("failed repair event");
    assert_eq!(failed.phase, RepairPhase::Failed);
    assert_eq!(failed.mode, RepairMode::Quarantine);
    let quarantine = PathBuf::from(failed.quarantine_path.expect("quarantine path"));
    assert!(
        !quarantine.exists(),
        "checkpoint failure must happen before quarantine rename"
    );
}

#[test]
fn retry_recomputes_valid_wal_that_became_malformed() {
    let (_directory, path) = database_path();
    let (mut wal, _shm) = materialize_valid_wal_family(&path);
    let quarantine = append_interrupted_quarantine(&path, "valid-to-malformed", true);
    wal[24] ^= 1;
    fs::write(wal_path(&path), &wal).expect("corrupt current wal");

    let report = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect("repair");
    assert_eq!(report.recovered.messages, 1);
    assert_eq!(
        fs::read(wal_path(&quarantine)).expect("quarantined wal"),
        wal
    );
    let log = read_repair_log(&path).expect("repair log");
    assert!(!log.last_applied.expect("completed event").checkpoint_source);
    let replacement = MemoryDb::open(&path).expect("replacement");
    assert_eq!(replacement.count_total().expect("count"), 1);
    assert!(replacement
        .search("wal-only", 10)
        .expect("search")
        .is_empty());
}

#[test]
fn retry_recomputes_malformed_wal_that_became_valid() {
    let (_directory, path) = database_path();
    let (wal, shm) = materialize_valid_wal_family(&path);
    let quarantine = append_interrupted_quarantine(&path, "malformed-to-valid", false);
    fs::write(wal_path(&path), [0_u8; 32]).expect("temporary malformed wal");
    fs::remove_file(shm_path(&path)).expect("remove stale shm");

    fs::write(wal_path(&path), &wal).expect("restore valid wal");
    fs::write(shm_path(&path), &shm).expect("restore valid shm");
    let report = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect("repair");
    assert_eq!(report.recovered.messages, 2);
    assert!(quarantine.exists());
    let log = read_repair_log(&path).expect("repair log");
    assert!(log.last_applied.expect("completed event").checkpoint_source);
    let replacement = MemoryDb::open(&path).expect("replacement");
    assert_eq!(replacement.count_total().expect("count"), 2);
    assert_eq!(replacement.search("committed", 10).expect("search").len(), 1);
}

#[test]
fn quarantine_aborts_before_rename_when_reader_blocks_checkpoint() {
    let (_directory, path) = database_path();
    let writer = create_quarantine_required_writer(&path);
    let reader = Connection::open(&path).expect("open reader");
    reader.execute_batch("BEGIN").expect("begin read");
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .expect("establish read snapshot");
    writer
        .execute(
            "INSERT INTO messages(session_id, role, content, ts_ms)
             VALUES('session', 'user', 'committed wal row', 2)",
            [],
        )
        .expect("commit wal row");

    let error = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect_err("reader must block truncate checkpoint");
    assert!(error.to_string().contains("checkpoint"));
    assert_failed_quarantine_did_not_move_database(&path);
    reader.execute_batch("ROLLBACK").expect("end read");
    assert_eq!(
        writer
            .query_row("SELECT COUNT(*) FROM messages", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count"),
        2
    );
}

#[test]
fn quarantine_aborts_before_rename_when_writer_is_active() {
    let (_directory, path) = database_path();
    let writer = create_quarantine_required_writer(&path);
    writer
        .execute_batch("BEGIN IMMEDIATE")
        .expect("begin write");
    writer
        .execute(
            "INSERT INTO messages(session_id, role, content, ts_ms)
             VALUES('session', 'user', 'uncommitted row', 2)",
            [],
        )
        .expect("write uncommitted row");

    let error = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect_err("writer must block truncate checkpoint");
    assert!(
        error.to_string().contains("locked") || error.to_string().contains("checkpoint"),
        "unexpected error: {error}"
    );
    assert_failed_quarantine_did_not_move_database(&path);
    writer.execute_batch("ROLLBACK").expect("rollback writer");
}

fn rewrite_first_frame_checksum(wal: &mut [u8]) {
    let magic = u32::from_be_bytes(wal[0..4].try_into().unwrap());
    let big_endian = magic & 1 != 0;
    let page_size = u32::from_be_bytes(wal[8..12].try_into().unwrap()) as usize;
    let seed = wal_checksum(&wal[..24], big_endian, [0, 0]);
    let checksum = wal_checksum(&wal[32..40], big_endian, seed);
    let checksum = wal_checksum(&wal[56..56 + page_size], big_endian, checksum);
    wal[48..52].copy_from_slice(&checksum[0].to_be_bytes());
    wal[52..56].copy_from_slice(&checksum[1].to_be_bytes());
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
    assert!(repaired
        .recovery_warning
        .as_deref()
        .is_some_and(|warning| warning.contains("standalone main-database recovery failed")));
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
        checkpoint_source: true,
        recovered: RecoveredRecords::default(),
        recovery_warning: None,
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
fn interrupted_quarantine_blocks_open_and_rejects_unbound_live_database() {
    let (_directory, path) = database_path();
    create_message_database(&path, "preserve this checkpointed row");
    let attempt_id = "quarantine-interrupted";
    let quarantine = quarantine_base(&path, attempt_id);
    let attempt = RepairEvent {
        version: REPAIR_LOG_VERSION,
        attempt_id: attempt_id.to_string(),
        ts_ms: current_ts_ms(),
        phase: RepairPhase::Started,
        mode: RepairMode::Quarantine,
        planned_actions: vec!["quarantine_and_initialize_replacement".to_string()],
        quarantine_path: Some(quarantine.display().to_string()),
        checkpoint_source: true,
        recovered: RecoveredRecords::default(),
        recovery_warning: None,
        error: None,
    };
    {
        let _lock = acquire_exclusive_lifecycle_lock(&path).expect("lifecycle lock");
        append_repair_event(&path, &attempt).expect("repair start");
        let mut moved = Vec::new();
        move_family(&path, &quarantine, &mut moved).expect("move to quarantine");
    }

    let open_error = MemoryDb::open(&path).expect_err("open must not replace interrupted repair");
    assert!(open_error.is_integrity_failure());
    assert!(!path.exists());

    // Simulate an older binary recreating an unrelated empty database. The
    // retry must preserve it and rebuild a replacement bound to this attempt.
    ensure_private_database_file(&path).expect("create unbound database");
    let connection = Connection::open(&path).expect("open unbound database");
    initialize_connection(&connection).expect("initialize unbound database");
    drop(connection);

    let staged = staged_replacement_path(&path, attempt_id);
    ensure_private_database_file(&staged).expect("create wrong staged database");
    let staged_connection = Connection::open(&staged).expect("open wrong staged database");
    initialize_connection(&staged_connection).expect("initialize wrong staged database");
    write_replacement_marker(
        &staged_connection,
        &ReplacementMarker {
            attempt_id: "different-attempt".to_string(),
            quarantine_path: quarantine.display().to_string(),
            source_main_sha256: file_sha256_optional(&quarantine).unwrap(),
            complete: true,
            salvage_succeeded: false,
            recovered: RecoveredRecords::default(),
            recovery_warning: None,
        },
    )
    .expect("write wrong marker");
    checkpoint_wal(&staged_connection).expect("checkpoint staged database");
    drop(staged_connection);

    let report = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect("resume quarantine");
    assert!(report.resumed_interrupted_repair);
    assert_eq!(report.recovered.messages, 1);
    assert!(report
        .quarantined_files
        .iter()
        .any(|value| value.contains("unbound-live")));
    assert!(report
        .quarantined_files
        .iter()
        .any(|value| value.contains("unbound-staged")));

    let marker = read_replacement_marker(&path)
        .expect("read marker")
        .expect("marker");
    assert_eq!(marker.attempt_id, attempt_id);
    assert!(marker.complete);
    let replacement = MemoryDb::open(&path).expect("open replacement");
    assert_eq!(
        replacement
            .search("checkpointed", 10)
            .expect("search")
            .len(),
        1
    );
}

#[test]
fn staged_replacement_marker_in_wal_is_not_treated_as_durable() {
    let (_directory, path) = database_path();
    create_message_database(&path, "quarantined authoritative row");
    let attempt_id = "staged-wal-window";
    let quarantine = quarantine_base(&path, attempt_id);
    let attempt = RepairEvent {
        version: REPAIR_LOG_VERSION,
        attempt_id: attempt_id.to_string(),
        ts_ms: current_ts_ms(),
        phase: RepairPhase::Started,
        mode: RepairMode::Quarantine,
        planned_actions: vec!["quarantine_and_initialize_replacement".to_string()],
        quarantine_path: Some(quarantine.display().to_string()),
        checkpoint_source: true,
        recovered: RecoveredRecords::default(),
        recovery_warning: None,
        error: None,
    };
    {
        let _lock = acquire_exclusive_lifecycle_lock(&path).expect("lifecycle lock");
        append_repair_event(&path, &attempt).expect("repair start");
        let mut moved = Vec::new();
        move_family(&path, &quarantine, &mut moved).expect("move to quarantine");
    }

    let donor = path.with_extension("staged-donor");
    ensure_private_database_file(&donor).expect("create donor");
    let donor_connection = Connection::open(&donor).expect("open donor");
    initialize_connection(&donor_connection).expect("initialize donor");
    checkpoint_wal(&donor_connection).expect("checkpoint donor schema");
    donor_connection
        .execute_batch("PRAGMA wal_autocheckpoint = 0;")
        .expect("disable donor checkpoint");
    donor_connection
        .execute(
            "INSERT INTO messages(session_id, role, content, ts_ms)
             VALUES('fake', 'user', 'sidecar-only fake row', 1)",
            [],
        )
        .expect("write sidecar-only row");
    write_replacement_marker(
        &donor_connection,
        &ReplacementMarker {
            attempt_id: attempt_id.to_string(),
            quarantine_path: quarantine.display().to_string(),
            source_main_sha256: file_sha256_optional(&quarantine).unwrap(),
            complete: true,
            salvage_succeeded: true,
            recovered: RecoveredRecords {
                messages: 1,
                ..RecoveredRecords::default()
            },
            recovery_warning: None,
        },
    )
    .expect("write sidecar-only marker");

    let staged = staged_replacement_path(&path, attempt_id);
    copy_snapshot_file(&donor, &staged).expect("copy staged main");
    copy_snapshot_file(&wal_path(&donor), &wal_path(&staged)).expect("copy staged wal");
    copy_snapshot_file(&shm_path(&donor), &shm_path(&staged)).expect("copy staged shm");
    drop(donor_connection);

    let report = repair(
        &path,
        RepairOptions {
            allow_quarantine: true,
            ..RepairOptions::default()
        },
    )
    .expect("resume quarantine");
    assert_eq!(report.recovered.messages, 1);
    assert!(report
        .quarantined_files
        .iter()
        .any(|value| { value.contains("unbound-staged") && value.ends_with("-wal") }));

    let replacement = MemoryDb::open(&path).expect("replacement");
    assert_eq!(
        replacement
            .search("authoritative", 10)
            .expect("search")
            .len(),
        1
    );
    assert!(replacement
        .search("sidecar-only", 10)
        .expect("search")
        .is_empty());
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
        checkpoint_source: true,
        recovered: RecoveredRecords::default(),
        recovery_warning: None,
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
