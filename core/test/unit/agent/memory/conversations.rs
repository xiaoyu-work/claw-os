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
