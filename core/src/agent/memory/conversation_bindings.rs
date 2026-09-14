//! @agent-file
//! Responsibility: atomically bind persisted conversation rows to canonical Agent tasks.
//! Key dependencies: the canonical SQLite message store and immutable trust provenance.
//! Constraints: task membership survives ordinary message purges and never derives from text.

use rusqlite::{params, Connection, OptionalExtension};

use super::sqlite_fts::{current_ts_ms, insert_labeled_message_at, MemoryDb, MemoryError};

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS conversation_message_tasks (
    message_id             INTEGER PRIMARY KEY,
    session_id             TEXT NOT NULL,
    task_id                TEXT NOT NULL,
    user_message_id        INTEGER NOT NULL,
    source_session_id      TEXT NOT NULL,
    source_message_id      INTEGER NOT NULL,
    source_user_message_id INTEGER NOT NULL,
    is_user_prompt         INTEGER NOT NULL CHECK(is_user_prompt IN (0, 1))
);
CREATE INDEX IF NOT EXISTS conversation_message_tasks_session
    ON conversation_message_tasks(session_id, task_id, message_id);
"#;

#[derive(Debug, Clone)]
pub(crate) struct RecordedTaskTurn {
    session_id: String,
    task_id: String,
    user_message_id: i64,
}

impl RecordedTaskTurn {
    pub(crate) fn user_message_id(&self) -> i64 {
        self.user_message_id
    }
}

#[derive(Debug)]
struct StoredTaskBinding {
    session_id: String,
    task_id: String,
    user_message_id: i64,
    source_session_id: String,
    source_message_id: i64,
    source_user_message_id: i64,
    is_user_prompt: bool,
}

impl MemoryDb {
    pub(crate) fn record_task_user_message(
        &self,
        session_id: &str,
        task_id: &str,
        segment: &crate::agent::trust::LabeledSegment,
        content: &str,
    ) -> Result<RecordedTaskTurn, MemoryError> {
        validate_session_id(session_id)?;
        validate_task_id(task_id)?;

        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let message_id =
            insert_labeled_message_at(&tx, session_id, "user", segment, content, current_ts_ms())?;
        insert_binding(&tx, message_id, session_id, task_id, message_id, true)?;
        tx.commit()?;

        Ok(RecordedTaskTurn {
            session_id: session_id.to_string(),
            task_id: task_id.to_string(),
            user_message_id: message_id,
        })
    }

    pub(crate) fn record_task_message(
        &self,
        turn: &RecordedTaskTurn,
        role: &str,
        segment: &crate::agent::trust::LabeledSegment,
        content: &str,
    ) -> Result<i64, MemoryError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let outer = binding(&tx, turn.user_message_id)?.ok_or_else(|| {
            MemoryError::InvalidRecording("task user-message binding disappeared".to_string())
        })?;
        if !outer.is_user_prompt
            || outer.session_id != turn.session_id
            || outer.task_id != turn.task_id
            || outer.user_message_id != turn.user_message_id
            || outer.source_session_id != turn.session_id
            || outer.source_message_id != turn.user_message_id
            || outer.source_user_message_id != turn.user_message_id
        {
            return Err(MemoryError::InvalidRecording(
                "task user-message binding changed".to_string(),
            ));
        }

        let message_id = insert_labeled_message_at(
            &tx,
            &turn.session_id,
            role,
            segment,
            content,
            current_ts_ms(),
        )?;
        insert_binding(
            &tx,
            message_id,
            &turn.session_id,
            &turn.task_id,
            turn.user_message_id,
            false,
        )?;
        tx.commit()?;
        Ok(message_id)
    }
}

fn validate_session_id(value: &str) -> Result<(), MemoryError> {
    if value.is_empty() || value.len() > 128 {
        return Err(MemoryError::InvalidRecording(
            "invalid task session identity".to_string(),
        ));
    }
    Ok(())
}

fn validate_task_id(value: &str) -> Result<(), MemoryError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(MemoryError::InvalidRecording(
            "invalid explicit task id".to_string(),
        ));
    }
    Ok(())
}

fn binding(conn: &Connection, message_id: i64) -> rusqlite::Result<Option<StoredTaskBinding>> {
    conn.query_row(
        "SELECT binding.session_id, binding.task_id, binding.user_message_id,
                binding.source_session_id, binding.source_message_id,
                binding.source_user_message_id, binding.is_user_prompt
         FROM conversation_message_tasks AS binding
         JOIN messages ON messages.id = binding.message_id
         WHERE binding.message_id = ? AND messages.session_id = binding.session_id",
        [message_id],
        |row| {
            Ok(StoredTaskBinding {
                session_id: row.get(0)?,
                task_id: row.get(1)?,
                user_message_id: row.get(2)?,
                source_session_id: row.get(3)?,
                source_message_id: row.get(4)?,
                source_user_message_id: row.get(5)?,
                is_user_prompt: row.get(6)?,
            })
        },
    )
    .optional()
}

fn insert_binding(
    conn: &Connection,
    message_id: i64,
    session_id: &str,
    task_id: &str,
    user_message_id: i64,
    is_user_prompt: bool,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO conversation_message_tasks(
             message_id, session_id, task_id, user_message_id, source_session_id,
             source_message_id, source_user_message_id, is_user_prompt
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            message_id,
            session_id,
            task_id,
            user_message_id,
            session_id,
            message_id,
            user_message_id,
            is_user_prompt
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("../../../test/unit/agent/memory/conversation_bindings.rs");
}
