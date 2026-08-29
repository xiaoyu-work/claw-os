use super::*;

fn db() -> MemoryDb {
    MemoryDb::open_in_memory().unwrap()
}

fn seed_rows(db: &MemoryDb, session_id: &str, count: usize) -> Vec<i64> {
    (0..count)
        .map(|index| {
            let role = if index % 2 == 0 { "user" } else { "assistant" };
            db.record_message(session_id, role, &format!("message {index}"))
                .unwrap()
        })
        .collect()
}

fn spec(ids: &[i64], protected: i64) -> NewCompaction {
    NewCompaction {
        source_start_id: ids[0],
        source_end_id: *ids.last().unwrap(),
        source_count: ids.len(),
        protected_tail_start_id: Some(protected),
        protected_user_message_id: Some(protected),
        algorithm: "test-summary".to_string(),
        algorithm_version: 7,
        provider: "mock".to_string(),
        model: "mock-model".to_string(),
        previous_compaction_id: None,
        pruned_tool_results: 2,
    }
}

#[test]
fn completed_compaction_projects_summary_and_uncompacted_tail() {
    let db = db();
    let ids = seed_rows(&db, "session", 5);
    db.freeze_system_prompt("session", "frozen prompt", 1)
        .unwrap();

    let attempt = match db
        .begin_compaction("session", spec(&ids[..3], ids[4]))
        .unwrap()
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected started attempt, got {other:?}"),
    };
    let completed = attempt.complete("[CONTEXT SUMMARY]\n\nsummary").unwrap();

    assert_eq!(completed.record.generation, 1);
    assert_eq!(completed.record.state, CompactionState::Completed);
    assert_eq!(completed.record.source_start_id, ids[0]);
    assert_eq!(completed.record.source_end_id, ids[2]);
    assert_eq!(completed.record.source_count, 3);
    assert_eq!(completed.record.source_ids, ids[..3]);
    assert_eq!(completed.record.protected_tail_start_id, Some(ids[4]));
    assert_eq!(completed.record.protected_user_message_id, Some(ids[4]));
    assert!(completed.record.prompt_hash.is_some());
    assert!(completed.record.recovery_metadata.raw_rows_searchable);

    let projection = db.continuation_projection("session", 100, true).unwrap();
    assert_eq!(
        projection.summary.unwrap().summary,
        "[CONTEXT SUMMARY]\n\nsummary"
    );
    assert_eq!(
        projection.tail.iter().map(|row| row.id).collect::<Vec<_>>(),
        ids[3..].to_vec()
    );
    assert_eq!(db.count_session("session").unwrap(), 5);
    assert_eq!(
        db.search_session("session", "message", 10).unwrap().len(),
        5
    );
}

#[test]
fn completed_source_range_is_not_started_again() {
    let db = db();
    let ids = seed_rows(&db, "session", 4);
    let first = match db
        .begin_compaction("session", spec(&ids[..2], ids[2]))
        .unwrap()
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected started attempt, got {other:?}"),
    };
    first.complete("[CONTEXT SUMMARY]\n\nfirst").unwrap();

    assert!(matches!(
        db.begin_compaction("session", spec(&ids[..2], ids[2]))
            .unwrap(),
        BeginCompaction::AlreadyCovered
    ));
    assert_eq!(db.compactions_for_session("session").unwrap().len(), 1);
}

#[test]
fn compaction_retains_the_exact_frozen_prompt_blob_after_upgrade() {
    let db = db();
    let ids = seed_rows(&db, "session", 4);
    db.freeze_system_prompt("session", "prompt version one", 1)
        .unwrap();
    let attempt = match db
        .begin_compaction("session", spec(&ids[..2], ids[2]))
        .unwrap()
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected started attempt, got {other:?}"),
    };
    let completed = attempt.complete("[CONTEXT SUMMARY]\n\nsummary").unwrap();
    let old_prompt_hash = completed.record.prompt_hash.unwrap();

    db.freeze_system_prompt("session", "prompt version two", 2)
        .unwrap();
    let conn = db.lock_conn().unwrap();
    let old_prompt: String = conn
        .query_row(
            "SELECT prompt FROM system_prompts WHERE hash = ?",
            params![old_prompt_hash],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_prompt, "prompt version one");
}

#[test]
fn dropped_started_attempt_is_detected_and_closed_before_retry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.db");
    let db = MemoryDb::open(&path).unwrap();
    let ids = seed_rows(&db, "session", 4);
    let attempt = match db
        .begin_compaction("session", spec(&ids[..2], ids[2]))
        .unwrap()
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected started attempt, got {other:?}"),
    };
    drop(attempt);
    drop(db);

    let db = MemoryDb::open(&path).unwrap();
    let projection = db.continuation_projection("session", 100, true).unwrap();
    assert_eq!(projection.recovered_interrupted, 1);
    let records = db.compactions_for_session("session").unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, CompactionState::Failed);
    assert_eq!(
        records[0].failure_kind.as_deref(),
        Some("interrupted_before_completion")
    );

    let retry = match db
        .begin_compaction("session", spec(&ids[..2], ids[2]))
        .unwrap()
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected retry, got {other:?}"),
    };
    assert_eq!(retry.generation(), 2);
    retry.complete("[CONTEXT SUMMARY]\n\nretry").unwrap();
}

#[test]
fn concurrent_attempts_are_serialized_per_session() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.db");
    let first_db = MemoryDb::open(&path).unwrap();
    let ids = seed_rows(&first_db, "session", 4);
    let second_db = MemoryDb::open(&path).unwrap();
    let first = match first_db
        .begin_compaction("session", spec(&ids[..2], ids[2]))
        .unwrap()
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected started attempt, got {other:?}"),
    };
    assert!(matches!(
        second_db
            .begin_compaction("session", spec(&ids[..2], ids[2]))
            .unwrap(),
        BeginCompaction::Busy
    ));
    first.fail("test_finished").unwrap();
    assert!(matches!(
        second_db
            .begin_compaction("session", spec(&ids[..2], ids[2]))
            .unwrap(),
        BeginCompaction::Started(_)
    ));
}

#[test]
fn invalid_newest_summary_falls_back_to_latest_valid_projection() {
    let db = db();
    let ids = seed_rows(&db, "session", 5);
    let first = match db
        .begin_compaction("session", spec(&ids[..2], ids[2]))
        .unwrap()
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected first attempt, got {other:?}"),
    };
    let first = first.complete("[CONTEXT SUMMARY]\n\nfirst").unwrap();

    let mut second_spec = spec(&ids[..4], ids[4]);
    second_spec.previous_compaction_id = Some(first.record.id);
    let second = match db.begin_compaction("session", second_spec).unwrap() {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected second attempt, got {other:?}"),
    };
    let second = second.complete("[CONTEXT SUMMARY]\n\nsecond").unwrap();
    {
        let conn = db.lock_conn().unwrap();
        conn.execute(
            "UPDATE compaction_summaries SET summary = 'tampered'
             WHERE hash = ?",
            params![second.record.summary_hash.as_deref().unwrap()],
        )
        .unwrap();
    }

    let projection = db.continuation_projection("session", 100, true).unwrap();
    assert_eq!(projection.rejected_invalid, 1);
    assert_eq!(projection.summary.unwrap().record.id, first.record.id);
    assert_eq!(
        projection.tail.iter().map(|row| row.id).collect::<Vec<_>>(),
        ids[2..].to_vec()
    );
}

#[test]
fn source_digest_rejects_a_summary_after_raw_row_mutation() {
    let db = db();
    let ids = seed_rows(&db, "session", 4);
    let attempt = match db
        .begin_compaction("session", spec(&ids[..2], ids[2]))
        .unwrap()
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected attempt, got {other:?}"),
    };
    attempt.complete("[CONTEXT SUMMARY]\n\nsummary").unwrap();
    {
        let conn = db.lock_conn().unwrap();
        conn.execute(
            "UPDATE messages SET content = 'mutated' WHERE id = ?",
            params![ids[0]],
        )
        .unwrap();
    }

    let (summary, rejected) = db.latest_valid_compaction("session").unwrap();
    assert!(summary.is_none());
    assert_eq!(rejected, 1);
}

#[test]
fn malformed_recovery_metadata_is_rejected_without_hiding_raw_tail() {
    let db = db();
    let ids = seed_rows(&db, "session", 4);
    let attempt = match db
        .begin_compaction("session", spec(&ids[..2], ids[2]))
        .unwrap()
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected attempt, got {other:?}"),
    };
    attempt.complete("[CONTEXT SUMMARY]\n\nsummary").unwrap();
    {
        let conn = db.lock_conn().unwrap();
        conn.execute(
            "UPDATE session_compactions SET recovery_metadata = '{bad json}'",
            [],
        )
        .unwrap();
    }

    let projection = db.continuation_projection("session", 100, true).unwrap();
    assert!(projection.summary.is_none());
    assert_eq!(projection.rejected_invalid, 1);
    assert_eq!(
        projection.tail.iter().map(|row| row.id).collect::<Vec<_>>(),
        ids
    );
}

#[test]
fn protected_anchor_must_be_a_real_user_row() {
    let db = db();
    let ids = seed_rows(&db, "session", 3);
    let tool_result = db
        .record_message("session", "user", "[tool_result] output")
        .unwrap();
    let error = db
        .begin_compaction("session", spec(&ids[..2], tool_result))
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("protected user anchor is not a real user message"));
}

#[test]
fn clearing_a_session_removes_only_its_compaction_projection() {
    let db = db();
    let first_ids = seed_rows(&db, "first", 4);
    let second_ids = seed_rows(&db, "second", 4);
    for (session_id, ids) in [("first", &first_ids), ("second", &second_ids)] {
        let attempt = match db
            .begin_compaction(session_id, spec(&ids[..2], ids[2]))
            .unwrap()
        {
            BeginCompaction::Started(attempt) => attempt,
            other => panic!("expected attempt, got {other:?}"),
        };
        attempt
            .complete(&format!("[CONTEXT SUMMARY]\n\n{session_id}"))
            .unwrap();
    }

    db.clear_session("first").unwrap();
    assert!(db.compactions_for_session("first").unwrap().is_empty());
    assert_eq!(db.compactions_for_session("second").unwrap().len(), 1);
    assert_eq!(db.count_session("first").unwrap(), 0);
    assert_eq!(db.count_session("second").unwrap(), 4);
}
