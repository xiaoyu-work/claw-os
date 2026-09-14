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
    assert_eq!(history[0].text, "second prompt");
    assert_eq!(history[1].text, "second answer");
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
    assert_eq!(history[1].tool_calls[0]["name"], "cos_app_fs");
    assert_eq!(
        history[2].tool_results[0]["text"],
        "first result\nsecond line"
    );
    assert_eq!(history[3].text, "first answer");
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
    assert!(page.messages.last().unwrap().text.starts_with("299: "));
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
    assert_eq!(page.messages[0].text, "retained answer");
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
