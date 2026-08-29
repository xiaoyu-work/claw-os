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

fn complete_chain(db: &MemoryDb, ids: &[i64]) -> Vec<CompactionSummary> {
    let mut completed = Vec::new();
    for end in [2_usize, 4, 6] {
        let mut next = spec(&ids[..end], ids[end]);
        next.previous_compaction_id = completed.last().map(|summary: &CompactionSummary| {
            summary.record.id
        });
        let attempt = match db.begin_compaction("session", next).unwrap() {
            BeginCompaction::Started(attempt) => attempt,
            other => panic!("expected chain attempt, got {other:?}"),
        };
        completed.push(
            attempt
                .complete(&format!("[CONTEXT SUMMARY]\n\ngeneration {end}"))
                .unwrap(),
        );
    }
    completed
}

#[test]
fn completed_compaction_projects_summary_and_uncompacted_tail() {
    let db = db();
    let ids = seed_rows(&db, "session", 5);
    db.freeze_system_prompt("session", "frozen prompt", 1)
        .unwrap();

    let mut first_spec = spec(&ids[..3], ids[3]);
    first_spec.protected_user_message_id = Some(ids[4]);
    let attempt = match db.begin_compaction("session", first_spec).unwrap() {
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
    assert_eq!(completed.record.protected_tail_start_id, Some(ids[3]));
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
fn first_compaction_cannot_skip_earlier_replayable_rows() {
    let db = db();
    let ids = seed_rows(&db, "session", 5);
    let error = db
        .begin_compaction("session", spec(&ids[1..3], ids[4]))
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("first compaction must start at earliest replayable row"));
    assert!(db.compactions_for_session("session").unwrap().is_empty());
}

#[test]
fn first_compaction_cannot_claim_a_predecessor() {
    let db = db();
    let ids = seed_rows(&db, "session", 4);
    let mut invalid = spec(&ids[..2], ids[2]);
    invalid.previous_compaction_id = Some(42);
    let error = db.begin_compaction("session", invalid).unwrap_err();
    assert!(error
        .to_string()
        .contains("first compaction cannot reference a predecessor"));
}

#[test]
fn successor_must_extend_and_reference_the_latest_valid_predecessor() {
    let db = db();
    let ids = seed_rows(&db, "session", 6);
    let first = match db
        .begin_compaction("session", spec(&ids[..2], ids[2]))
        .unwrap()
    {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected first attempt, got {other:?}"),
    };
    let first = first.complete("[CONTEXT SUMMARY]\n\nfirst").unwrap();

    let missing_predecessor = spec(&ids[..4], ids[4]);
    let error = db
        .begin_compaction("session", missing_predecessor)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("must reference the latest valid predecessor"));

    let mut shifted_start = spec(&ids[1..4], ids[4]);
    shifted_start.previous_compaction_id = Some(first.record.id);
    let error = db.begin_compaction("session", shifted_start).unwrap_err();
    assert!(error
        .to_string()
        .contains("must retain predecessor source start"));

    let mut valid = spec(&ids[..4], ids[4]);
    valid.previous_compaction_id = Some(first.record.id);
    let successor = match db.begin_compaction("session", valid).unwrap() {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected successor attempt, got {other:?}"),
    };
    let successor = successor
        .complete("[CONTEXT SUMMARY]\n\nsuccessor")
        .unwrap();
    assert_eq!(
        successor.record.previous_compaction_id,
        Some(first.record.id)
    );
    assert_eq!(successor.record.source_start_id, ids[0]);
    assert_eq!(successor.record.source_end_id, ids[3]);
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
    let ids = seed_rows(&db, "session", 2);
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
fn persisted_compaction_requires_both_protected_ids() {
    let db = db();
    let ids = seed_rows(&db, "session", 4);

    let mut missing_tail = spec(&ids[..2], ids[2]);
    missing_tail.protected_tail_start_id = None;
    assert!(db
        .begin_compaction("session", missing_tail)
        .unwrap_err()
        .to_string()
        .contains("requires a protected tail boundary"));

    let mut missing_user = spec(&ids[..2], ids[2]);
    missing_user.protected_user_message_id = None;
    assert!(db
        .begin_compaction("session", missing_user)
        .unwrap_err()
        .to_string()
        .contains("requires a protected real-user anchor"));
}

#[test]
fn protected_rows_must_be_ordered_owned_and_start_the_tail() {
    let db = db();
    let ids = seed_rows(&db, "session", 5);
    let foreign = db.record_message("other", "user", "foreign").unwrap();

    let mut skipped_tail = spec(&ids[..2], ids[3]);
    skipped_tail.protected_user_message_id = Some(ids[4]);
    assert!(db
        .begin_compaction("session", skipped_tail)
        .unwrap_err()
        .to_string()
        .contains("first replayable row"));

    let mut foreign_tail = spec(&ids[..2], ids[2]);
    foreign_tail.protected_tail_start_id = Some(foreign);
    foreign_tail.protected_user_message_id = Some(foreign);
    assert!(db
        .begin_compaction("session", foreign_tail)
        .unwrap_err()
        .to_string()
        .contains("protected tail boundary is missing"));

    let mut reversed = spec(&ids[..2], ids[2]);
    reversed.protected_user_message_id = Some(ids[1]);
    assert!(db
        .begin_compaction("session", reversed)
        .unwrap_err()
        .to_string()
        .contains("precedes the protected tail boundary"));
}

#[test]
fn compaction_boundary_cannot_split_a_persisted_tool_pair() {
    let db = db();
    let first = db.record_message("session", "user", "inspect").unwrap();
    let call = db
        .record_message("session", "assistant", "[tool_use:lookup] {}")
        .unwrap();
    let result = db
        .record_message("session", "user", "[tool_result] output")
        .unwrap();
    let anchor = db.record_message("session", "user", "continue").unwrap();
    let mut split = spec(&[first, call], result);
    split.protected_user_message_id = Some(anchor);

    let error = db.begin_compaction("session", split).unwrap_err();
    assert!(error
        .to_string()
        .contains("splits or strands a tool call/result pair"));
}

#[test]
fn completed_projection_rejects_changed_or_missing_protected_identity() {
    let db = db();
    let ids = seed_rows(&db, "session", 5);
    let mut initial = spec(&ids[..3], ids[3]);
    initial.protected_user_message_id = Some(ids[4]);
    let attempt = match db.begin_compaction("session", initial).unwrap() {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected attempt, got {other:?}"),
    };
    attempt.complete("[CONTEXT SUMMARY]\n\nsummary").unwrap();

    {
        let conn = db.lock_conn().unwrap();
        conn.execute(
            "UPDATE messages SET content = 'changed tail identity' WHERE id = ?",
            params![ids[3]],
        )
        .unwrap();
    }
    let (summary, rejected) = db.latest_valid_compaction("session").unwrap();
    assert!(summary.is_none());
    assert_eq!(rejected, 1);

    {
        let conn = db.lock_conn().unwrap();
        conn.execute("DELETE FROM session_compactions", []).unwrap();
        conn.execute("DELETE FROM compaction_summaries", []).unwrap();
        conn.execute(
            "UPDATE messages SET content = 'message 3' WHERE id = ?",
            params![ids[3]],
        )
        .unwrap();
    }
    let mut retry = spec(&ids[..3], ids[3]);
    retry.protected_user_message_id = Some(ids[4]);
    let attempt = match db.begin_compaction("session", retry).unwrap() {
        BeginCompaction::Started(attempt) => attempt,
        other => panic!("expected attempt, got {other:?}"),
    };
    attempt.complete("[CONTEXT SUMMARY]\n\nsummary").unwrap();
    {
        let conn = db.lock_conn().unwrap();
        conn.execute("DELETE FROM messages WHERE id = ?", params![ids[4]])
            .unwrap();
    }
    let (summary, rejected) = db.latest_valid_compaction("session").unwrap();
    assert!(summary.is_none());
    assert_eq!(rejected, 1);
}

#[test]
fn repair_reroots_valid_latest_projection_around_invalid_middle() {
    let db = db();
    let ids = seed_rows(&db, "session", 8);
    let chain = complete_chain(&db, &ids);
    let root_id = chain[0].record.id;
    let middle_id = chain[1].record.id;
    let latest_id = chain[2].record.id;
    {
        let conn = db.lock_conn().unwrap();
        conn.execute(
            "UPDATE compaction_summaries SET summary = 'tampered middle'
             WHERE hash = ?",
            params![chain[1].record.summary_hash.as_deref().unwrap()],
        )
        .unwrap();
        repair_projection(&conn).unwrap();
    }

    let records = db.compactions_for_session("session").unwrap();
    assert_eq!(
        records.iter().map(|record| record.id).collect::<Vec<_>>(),
        vec![root_id, latest_id]
    );
    let latest = records.last().unwrap();
    assert_eq!(latest.previous_compaction_id, Some(root_id));
    assert_eq!(
        latest.recovery_metadata.previous_compaction_id,
        Some(root_id)
    );
    assert!(latest
        .recovery_metadata
        .rerooted_from_compaction_ids
        .contains(&middle_id));
    assert_eq!(
        db.latest_valid_compaction("session")
            .unwrap()
            .0
            .unwrap()
            .record
            .id,
        latest_id
    );
    assert_eq!(inspect_projection(&db.lock_conn().unwrap()).unwrap().invalid_records, 0);
}

#[test]
fn repair_heals_prior_set_null_without_deleting_valid_successor() {
    let db = db();
    let ids = seed_rows(&db, "session", 8);
    let chain = complete_chain(&db, &ids);
    let root_id = chain[0].record.id;
    let successor_id = chain[1].record.id;
    {
        let conn = db.lock_conn().unwrap();
        conn.execute(
            "DELETE FROM session_compactions WHERE id = ?",
            params![root_id],
        )
        .unwrap();
    }
    let broken = db
        .compactions_for_session("session")
        .unwrap()
        .into_iter()
        .find(|record| record.id == successor_id)
        .unwrap();
    assert_eq!(broken.previous_compaction_id, None);
    assert_eq!(
        broken.recovery_metadata.previous_compaction_id,
        Some(root_id)
    );
    assert!(db.latest_valid_compaction("session").unwrap().0.is_none());

    {
        let conn = db.lock_conn().unwrap();
        repair_projection(&conn).unwrap();
    }
    let healed = db
        .compactions_for_session("session")
        .unwrap()
        .into_iter()
        .find(|record| record.id == successor_id)
        .expect("valid successor should survive repair");
    assert_eq!(healed.previous_compaction_id, None);
    assert_eq!(healed.recovery_metadata.previous_compaction_id, None);
    assert!(healed
        .recovery_metadata
        .rerooted_from_compaction_ids
        .contains(&root_id));
    assert_eq!(
        db.latest_valid_compaction("session")
            .unwrap()
            .0
            .unwrap()
            .record
            .id,
        chain[2].record.id
    );
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
