use super::*;
use crate::agent::trust::{LabeledSegment, SourceKind};

fn segment(kind: SourceKind) -> LabeledSegment {
    LabeledSegment::of(kind, "")
}

#[test]
fn task_messages_and_provenance_are_bound_transactionally() {
    let db = MemoryDb::open_in_memory().unwrap();
    let user = segment(SourceKind::UserMessage);
    let model = segment(SourceKind::ModelResponse);
    let tool = segment(SourceKind::BuiltinToolResult);
    let turn = db
        .record_task_user_message("session", "real-task", &user, "prompt")
        .unwrap();
    let assistant = db
        .record_task_message(&turn, "assistant", &model, "working")
        .unwrap();
    let result = db
        .record_task_message(&turn, "user", &tool, "[tool_result] done")
        .unwrap();

    let conn = db.lock_conn().unwrap();
    let mut statement = conn
        .prepare(
            "SELECT b.message_id, b.task_id, b.user_message_id, b.is_user_prompt,
                    m.trust_source
             FROM conversation_message_tasks AS b
             JOIN messages AS m ON m.id = b.message_id
             ORDER BY b.message_id",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, turn.user_message_id());
    assert_eq!(rows[1].0, assistant);
    assert_eq!(rows[2].0, result);
    assert!(rows[0].3);
    assert!(!rows[1].3);
    assert!(rows
        .iter()
        .all(|row| row.1 == "real-task" && row.2 == turn.user_message_id()));
    assert_eq!(rows[0].4, SourceKind::UserMessage.tag());
    assert_eq!(rows[1].4, SourceKind::ModelResponse.tag());
    assert_eq!(rows[2].4, SourceKind::BuiltinToolResult.tag());
}

#[test]
fn failed_binding_insert_rolls_back_the_message() {
    let db = MemoryDb::open_in_memory().unwrap();
    db.lock_conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_task_binding
             BEFORE INSERT ON conversation_message_tasks
             BEGIN SELECT RAISE(ABORT, 'binding fixture failure'); END;",
        )
        .unwrap();

    let result = db.record_task_user_message(
        "session",
        "task",
        &segment(SourceKind::UserMessage),
        "not partially recorded",
    );
    assert!(result.is_err());
    assert_eq!(db.count_total().unwrap(), 0);
}

#[test]
fn changed_or_missing_outer_binding_refuses_child_messages() {
    let db = MemoryDb::open_in_memory().unwrap();
    let user = segment(SourceKind::UserMessage);
    let model = segment(SourceKind::ModelResponse);

    let changed = db
        .record_task_user_message("session", "task-one", &user, "first")
        .unwrap();
    db.lock_conn()
        .unwrap()
        .execute(
            "UPDATE conversation_message_tasks SET task_id = 'other'
             WHERE message_id = ?",
            [changed.user_message_id()],
        )
        .unwrap();
    assert!(matches!(
        db.record_task_message(&changed, "assistant", &model, "must not persist"),
        Err(MemoryError::InvalidRecording(_))
    ));

    let missing = db
        .record_task_user_message("session", "task-two", &user, "second")
        .unwrap();
    db.lock_conn()
        .unwrap()
        .execute(
            "DELETE FROM conversation_message_tasks WHERE message_id = ?",
            [missing.user_message_id()],
        )
        .unwrap();
    assert!(matches!(
        db.record_task_message(&missing, "assistant", &model, "must not persist"),
        Err(MemoryError::InvalidRecording(_))
    ));
    assert_eq!(db.count_total().unwrap(), 2);
}

#[test]
fn ordinary_history_clear_preserves_task_membership_evidence() {
    let db = MemoryDb::open_in_memory().unwrap();
    let turn = db
        .record_task_user_message(
            "session",
            "task",
            &segment(SourceKind::UserMessage),
            "prompt",
        )
        .unwrap();
    db.record_task_message(
        &turn,
        "assistant",
        &segment(SourceKind::ModelResponse),
        "answer",
    )
    .unwrap();

    assert_eq!(db.clear_session("session").unwrap(), 2);
    assert_eq!(db.count_total().unwrap(), 0);
    assert_eq!(
        db.lock_conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM conversation_message_tasks",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
}

#[test]
fn invalid_task_or_session_identity_fails_before_insert() {
    let db = MemoryDb::open_in_memory().unwrap();
    let user = segment(SourceKind::UserMessage);

    assert!(matches!(
        db.record_task_user_message("session", "not a task", &user, "prompt"),
        Err(MemoryError::InvalidRecording(_))
    ));
    assert!(matches!(
        db.record_task_user_message("", "task", &user, "prompt"),
        Err(MemoryError::InvalidRecording(_))
    ));
    assert!(matches!(
        db.record_task_user_message(&"s".repeat(129), "task", &user, "prompt"),
        Err(MemoryError::InvalidRecording(_))
    ));
    assert_eq!(db.count_total().unwrap(), 0);
}

#[test]
fn writable_open_adds_binding_schema_to_an_existing_database() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("memory.db");
    {
        let db = MemoryDb::open(&path).unwrap();
        db.lock_conn()
            .unwrap()
            .execute_batch("DROP TABLE conversation_message_tasks;")
            .unwrap();
    }

    let db = MemoryDb::open(&path).unwrap();
    db.record_task_user_message(
        "session",
        "task",
        &segment(SourceKind::UserMessage),
        "prompt",
    )
    .unwrap();
}
