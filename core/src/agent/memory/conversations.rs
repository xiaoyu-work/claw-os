//! @agent-file
//! Responsibility: project bounded, user-visible conversation rows and recency from MemoryDb.
//! Key dependencies: the canonical SQLite message store and shared history parser.
//! Constraints: filter private rows before limits and never return a partial serialized row.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension};

use super::conversation_bindings::{self, BindingState};
use super::history::{parse_stored_content, sanitize_stored_content, HistoryMessage};
use super::sqlite_fts::{row_to_message, MemoryDb, MemoryError, INJECTED_ROLE};

pub const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ConversationMemoryError {
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialize conversation history: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("one conversation message exceeds the response byte limit")]
    MessageTooLarge,
    #[error("conversation message count is invalid")]
    InvalidMessageCount,
}

#[derive(Clone, Debug, Default)]
pub struct ConversationMetadata {
    pub title: Option<String>,
    pub updated_at_ms: Option<i64>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct ConversationHistoryPage {
    pub messages: Vec<ConversationMessage>,
    pub message_count: u64,
    pub messages_truncated: bool,
    #[serde(skip)]
    pub(crate) bindings: BindingState,
}

#[derive(Debug, serde::Serialize)]
pub struct ConversationMessage {
    pub id: i64,
    #[serde(flatten)]
    pub message: HistoryMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_user_message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_user_prompt: Option<bool>,
}

impl MemoryDb {
    /// Return only user-visible conversation rows, newest page first and
    /// ordered oldest-to-newest within that page.
    pub fn conversation_history_page(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<ConversationHistoryPage, ConversationMemoryError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let message_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages
             WHERE session_id = ? AND role <> ? AND role <> 'system'",
            params![session_id, INJECTED_ROLE],
            |row| row.get(0),
        )?;
        let message_count = u64::try_from(message_count)
            .map_err(|_| ConversationMemoryError::InvalidMessageCount)?;
        let bindings = conversation_bindings::state(&tx, session_id)?;
        let binding_sizes = bindings
            .members
            .iter()
            .map(|binding| Ok((binding.message_id, serde_json::to_vec(binding)?.len())))
            .collect::<Result<BTreeMap<_, _>, serde_json::Error>>()?;
        let limit =
            i64::try_from(limit).map_err(|_| ConversationMemoryError::InvalidMessageCount)?;
        let mut stmt = tx.prepare(
            "SELECT id, session_id, role, content, ts_ms
             FROM messages
             WHERE session_id = ? AND role <> ? AND role <> 'system'
             ORDER BY ts_ms DESC, id DESC
             LIMIT ?",
        )?;
        let mut messages = Vec::new();
        let mut response_bytes = 2usize;
        for row in stmt.query_map(params![session_id, INJECTED_ROLE, limit], row_to_message)? {
            let row = row?;
            let content = sanitize_stored_content(&row.role, &row.content);
            let parsed = parse_stored_content(&row.role, &content);
            let message = ConversationMessage {
                id: row.id,
                message: HistoryMessage {
                    role: row.role,
                    content,
                    text: parsed.text,
                    tool_calls: parsed.tool_calls,
                    tool_results: parsed.tool_results,
                    ts_ms: row.ts_ms,
                },
                task_id: None,
                source_session_id: None,
                source_message_id: None,
                source_user_message_id: None,
                is_user_prompt: None,
            };
            let separator = usize::from(!messages.is_empty());
            let message_bytes = serde_json::to_vec(&message)?
                .len()
                .saturating_add(binding_sizes.get(&message.id).copied().unwrap_or(0));
            if response_bytes
                .saturating_add(separator)
                .saturating_add(message_bytes)
                > MAX_HISTORY_BYTES
            {
                if messages.is_empty() {
                    return Err(ConversationMemoryError::MessageTooLarge);
                }
                break;
            }
            response_bytes = response_bytes
                .saturating_add(separator)
                .saturating_add(message_bytes);
            messages.push(message);
        }
        messages.reverse();
        drop(stmt);
        tx.commit()?;
        Ok(ConversationHistoryPage {
            messages_truncated: message_count > messages.len() as u64,
            message_count,
            messages,
            bindings,
        })
    }

    pub fn conversation_metadata(
        &self,
        session_id: &str,
    ) -> Result<ConversationMetadata, ConversationMemoryError> {
        let conn = self.lock_conn()?;
        let title: Option<(String, i64)> = conn
            .query_row(
                "SELECT title, ts_ms FROM session_titles WHERE session_id = ?",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let message_updated_at: Option<i64> = conn.query_row(
            "SELECT MAX(ts_ms) FROM messages WHERE session_id = ?",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(ConversationMetadata {
            updated_at_ms: [
                title.as_ref().map(|(_, timestamp)| *timestamp),
                message_updated_at,
            ]
            .into_iter()
            .flatten()
            .max(),
            title: title.map(|(title, _)| title),
        })
    }
}

#[cfg(test)]
mod tests {
    include!("../../../test/unit/agent/memory/conversations.rs");
}
