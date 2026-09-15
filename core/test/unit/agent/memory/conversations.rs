use super::*;

fn seed(db: &MemoryDb, session_id: &str) {
    for (index, role, content) in [
        (0, "user", "first prompt"),
        (
            1,
            "assistant",
            "checking\n[tool_use:cos_app_fs] {\"path\":\"notes\"}",
        ),
        (2, "user", "[tool_result] first result\nsecond line"),
        (3, "assistant", "first answer"),
        (4, "user", "second prompt"),
        (5, "assistant", "second answer"),
    ] {
        db.record_message_at(session_id, role, content, 100 + index)
            .unwrap();
    }
    db.record_injected(session_id, "context_packet", "audit-only context")
        .unwrap();
}

#[test]
fn conversation_history_filters_private_rows_before_applying_the_limit() {
    let db = MemoryDb::open_in_memory().unwrap();
    seed(&db, "session");
    db.record_message_at("session", "system", "private system state", 1_000)
        .unwrap();

    let history = db.conversation_history_page("session", 2).unwrap().messages;
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].message.text, "second prompt");
    assert_eq!(history[1].message.text, "second answer");
}

#[test]
fn conversation_history_preserves_structured_tool_rows() {
    let db = MemoryDb::open_in_memory().unwrap();
    seed(&db, "session");

    let history = db
        .conversation_history_page("session", 100)
        .unwrap()
        .messages;
    assert_eq!(history.len(), 6);
    assert_eq!(history[1].message.tool_calls[0]["name"], "cos_app_fs");
    assert_eq!(
        history[2].message.tool_results[0]["text"],
        "first result\nsecond line"
    );
    assert_eq!(history[3].message.text, "first answer");
}

#[test]
fn conversation_history_byte_bound_keeps_latest_complete_rows() {
    let db = MemoryDb::open_in_memory().unwrap();
    let text = "x".repeat(32 * 1024);
    for index in 0..300 {
        db.record_message_at("session", "assistant", &format!("{index}: {text}"), index)
            .unwrap();
    }

    let page = db.conversation_history_page("session", 300).unwrap();
    assert!(!page.messages.is_empty());
    assert!(page.messages.len() < 300);
    assert!(page
        .messages
        .last()
        .unwrap()
        .message
        .text
        .starts_with("299: "));
    assert_eq!(page.message_count, 300);
    assert!(page.messages_truncated);
    assert!(serde_json::to_vec(&page.messages).unwrap().len() <= MAX_HISTORY_BYTES);
}

#[test]
fn conversation_history_rejects_a_single_oversized_row() {
    let db = MemoryDb::open_in_memory().unwrap();
    db.lock_conn()
        .unwrap()
        .execute(
            "INSERT INTO messages(session_id, role, content, ts_ms)
             VALUES ('session', 'assistant', ?, 1)",
            rusqlite::params!["x".repeat(MAX_HISTORY_BYTES)],
        )
        .unwrap();

    assert!(matches!(
        db.conversation_history_page("session", 1),
        Err(ConversationMemoryError::MessageTooLarge)
    ));
}

#[test]
fn conversation_metadata_combines_title_and_message_recency() {
    let db = MemoryDb::open_in_memory().unwrap();
    db.set_title("session", "Stored title").unwrap();
    db.record_message_at("session", "assistant", "answer", 42)
        .unwrap();

    let metadata = db.conversation_metadata("session").unwrap();
    assert_eq!(metadata.title.as_deref(), Some("Stored title"));
    assert!(metadata.updated_at_ms.unwrap() > 42);
}

#[test]
fn conversation_read_only_view_supports_legacy_message_columns_without_migrating() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("memory.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE messages (
                 id INTEGER PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 content TEXT NOT NULL,
                 ts_ms INTEGER NOT NULL
             );
             CREATE TABLE session_titles (
                 session_id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 ts_ms INTEGER NOT NULL
             );
             INSERT INTO messages(session_id, role, content, ts_ms)
             VALUES ('legacy', 'assistant', 'retained answer', 11);
             INSERT INTO session_titles(session_id, title, ts_ms)
             VALUES ('legacy', 'Legacy title', 12);",
        )
        .unwrap();
    }

    let db = MemoryDb::open_read_only(&path).unwrap();
    let page = db.conversation_history_page("legacy", 10).unwrap();
    assert_eq!(page.messages[0].message.text, "retained answer");
    assert_eq!(page.message_count, 1);
    assert_eq!(
        db.conversation_metadata("legacy").unwrap().title.as_deref(),
        Some("Legacy title")
    );
    drop(db);
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('messages')
             WHERE name = 'trust_class'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'table' AND name = 'conversation_replay_exclusions'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
}

#[test]
fn conversation_snapshot_copies_one_whole_task_with_provenance_and_prompt() {
    let db = MemoryDb::open_in_memory().unwrap();
    let user =
        crate::agent::trust::LabeledSegment::of(crate::agent::trust::SourceKind::UserMessage, "");
    let model =
        crate::agent::trust::LabeledSegment::of(crate::agent::trust::SourceKind::ModelResponse, "");
    let first = db
        .record_task_user_message("source", "task-one", &user, "first prompt")
        .unwrap();
    db.record_task_message(&first, "assistant", &model, "first answer")
        .unwrap();
    let second = db
        .record_task_user_message("source", "task-two", &user, "second prompt")
        .unwrap();
    db.record_task_message(&second, "assistant", &model, "second answer")
        .unwrap();
    db.record_injected("source", "context_packet", "private context")
        .unwrap();
    db.record_message_at("source", "system", "private system row", 999)
        .unwrap();
    db.set_title("source", "Source title").unwrap();
    db.freeze_system_prompt("source", "frozen policy", 7)
        .unwrap();

    let snapshot = db.conversation_snapshot("source", Some(1)).unwrap();
    assert_eq!(snapshot.visible_count(), 2);
    assert_eq!(snapshot.bindings.members.len(), 2);
    db.install_conversation_snapshot("child", &snapshot)
        .unwrap();

    let child = db.recent("child", 10).unwrap();
    assert_eq!(child.len(), 2);
    assert_eq!(child[0].content, "first prompt");
    assert_eq!(
        child[0].trust_source,
        Some(
            crate::agent::trust::SourceKind::UserMessage
                .tag()
                .to_string()
        )
    );
    assert_eq!(child[1].content, "first answer");
    assert_eq!(
        child[1].trust_source,
        Some(
            crate::agent::trust::SourceKind::ModelResponse
                .tag()
                .to_string()
        )
    );
    assert_eq!(
        db.system_prompt_for("child", 7).unwrap().as_deref(),
        Some("frozen policy")
    );
    assert!(db.conversation_metadata("child").unwrap().title.is_none());

    let page = db.conversation_history_page("child", 10).unwrap();
    assert_eq!(page.message_count, 2);
    assert!(page
        .bindings
        .members
        .iter()
        .all(|binding| binding.session_id == "child" && binding.source_session_id == "source"));
    assert!(matches!(
        db.install_conversation_snapshot("child", &snapshot),
        Err(ConversationMemoryError::DestinationExists)
    ));
}

#[test]
fn conversation_snapshot_rejects_unknown_user_turn_boundaries() {
    let db = MemoryDb::open_in_memory().unwrap();
    let user =
        crate::agent::trust::LabeledSegment::of(crate::agent::trust::SourceKind::UserMessage, "");
    db.record_task_user_message("source", "task-one", &user, "first prompt")
        .unwrap();

    assert!(matches!(
        db.conversation_snapshot("source", Some(2)),
        Err(ConversationMemoryError::InvalidUserTurns {
            requested: 2,
            available: 1
        })
    ));
}

#[test]
fn conversation_revert_excludes_replay_without_deleting_canonical_evidence() {
    let db = MemoryDb::open_in_memory().unwrap();
    let user =
        crate::agent::trust::LabeledSegment::of(crate::agent::trust::SourceKind::UserMessage, "");
    let model =
        crate::agent::trust::LabeledSegment::of(crate::agent::trust::SourceKind::ModelResponse, "");
    let first = db
        .record_task_user_message("session", "task-one", &user, "first prompt")
        .unwrap();
    db.record_task_message(&first, "assistant", &model, "first answer")
        .unwrap();
    let branch_context_id = db
        .record_message("session", "system", "retry branch context")
        .unwrap();
    let second = db
        .record_task_user_message("session", "task-two", &user, "second prompt")
        .unwrap();
    db.record_task_message(&second, "assistant", &model, "second answer")
        .unwrap();

    let current = db.conversation_snapshot("session", None).unwrap();
    let retained = db.conversation_revert_snapshot("session", 1).unwrap();
    assert_eq!(retained.visible_count(), 2);
    db.revert_conversation_checked("session", 1, 1_234, &current.revision)
        .unwrap();

    let page = db.conversation_history_page("session", 10).unwrap();
    assert_eq!(page.message_count, 2);
    assert_eq!(page.messages[0].message.text, "first prompt");
    assert_eq!(page.messages[1].message.text, "first answer");
    assert_eq!(page.bindings.members.len(), 2);
    assert_eq!(page.bindings.excluded.len(), 2);
    assert!(db.replayable_message(branch_context_id).unwrap().is_none());
    assert!(db
        .replayable_message(second.user_message_id())
        .unwrap()
        .is_none());
    assert!(db.message(second.user_message_id()).unwrap().is_some());
    assert!(db
        .search_history("second", Some("session"), 10)
        .unwrap()
        .is_empty());
    assert_eq!(db.search_session("session", "second", 10).unwrap().len(), 2);
    assert_eq!(
        db.recent_replayable("session", 10)
            .unwrap()
            .into_iter()
            .map(|row| row.content)
            .collect::<Vec<_>>(),
        ["first prompt", "first answer"]
    );
    let conn = db.lock_conn().unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = 'session'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        5
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM conversation_message_tasks
             WHERE session_id = 'session'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        4
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM conversation_replay_exclusions
             WHERE session_id = 'session'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        3
    );
    drop(conn);
    assert!(matches!(
        db.revert_conversation_checked("session", 1, 2_345, &current.revision),
        Err(ConversationMemoryError::Changed)
    ));

    let next = db.conversation_snapshot("session", None).unwrap();
    db.lock_conn()
        .unwrap()
        .execute(
            "UPDATE conversation_message_tasks
             SET task_id = 'changed-excluded-task'
             WHERE message_id = ?",
            [second.user_message_id()],
        )
        .unwrap();
    assert!(matches!(
        db.revert_conversation_checked("session", 1, 3_456, &next.revision),
        Err(ConversationMemoryError::Changed)
    ));
    db.lock_conn()
        .unwrap()
        .execute(
            "UPDATE conversation_message_tasks
             SET task_id = 'task-two'
             WHERE message_id = ?",
            [second.user_message_id()],
        )
        .unwrap();
    let next = db.conversation_snapshot("session", None).unwrap();
    db.revert_conversation_checked("session", 1, 3_456, &next.revision)
        .unwrap();
    let page = db.conversation_history_page("session", 10).unwrap();
    assert_eq!(page.message_count, 0);
    assert!(page.messages.is_empty());
    assert!(page.bindings.members.is_empty());
    assert_eq!(page.bindings.excluded.len(), 4);
    assert_eq!(page.bindings.originals.len(), 4);
    assert!(db.recent_replayable("session", 10).unwrap().is_empty());
    assert_eq!(db.recent("session", 10).unwrap().len(), 5);
    assert_eq!(db.search_session("session", "second", 10).unwrap().len(), 2);
    assert_eq!(
        db.lock_conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM conversation_replay_exclusions
                 WHERE session_id = 'session'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        5
    );
}
