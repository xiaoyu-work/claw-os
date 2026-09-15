//! @agent-file
//! Responsibility: project bounded, user-visible conversation rows and recency from MemoryDb.
//! Key dependencies: the canonical SQLite message store and shared history parser.
//! Constraints: filter private rows before limits and never return a partial serialized row.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

use super::conversation_bindings::{self, BindingState};
use super::history::{parse_stored_content, sanitize_stored_content, HistoryMessage};
use super::sqlite_fts::{row_to_message, MemoryDb, MemoryError, MessageRow, INJECTED_ROLE};

pub const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS conversation_replay_exclusions (
    message_id      INTEGER PRIMARY KEY,
    session_id      TEXT NOT NULL,
    reverted_at_ms  INTEGER NOT NULL,
    FOREIGN KEY(message_id) REFERENCES messages(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS conversation_replay_exclusions_session
    ON conversation_replay_exclusions(session_id, reverted_at_ms);
"#;

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
    #[error("conversation snapshot exceeds the bounded row or byte limit")]
    SnapshotTooLarge,
    #[error("requested {requested} user turns, but the conversation has {available}")]
    InvalidUserTurns { requested: u32, available: usize },
    #[error("conversation snapshot destination already contains memory records")]
    DestinationExists,
    #[error("conversation snapshot membership changed")]
    SnapshotChanged,
    #[error("conversation replay changed before the checked mutation")]
    Changed,
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

#[derive(Debug)]
struct FrozenPrompt {
    hash: String,
    version: u32,
    ts_ms: i64,
}

#[derive(Debug, Default)]
pub(crate) struct ConversationSnapshot {
    rows: Vec<MessageRow>,
    prompt: Option<FrozenPrompt>,
    pub(crate) bindings: BindingState,
    pub(crate) revision: String,
}

impl ConversationSnapshot {
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.prompt.is_none()
    }

    pub(crate) fn visible_count(&self) -> u64 {
        self.rows.len() as u64
    }
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
        let filter = active_replay_filter(&tx)?;
        let message_count: i64 = tx.query_row(
            &format!(
                "SELECT COUNT(*) FROM messages
                 WHERE session_id = ? AND role <> ? AND role <> 'system' {filter}"
            ),
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
        let mut stmt = tx.prepare(&format!(
            "SELECT id, session_id, role, content, ts_ms
             FROM messages
             WHERE session_id = ? AND role <> ? AND role <> 'system'
               {filter}
             ORDER BY ts_ms DESC, id DESC
             LIMIT ?"
        ))?;
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
        let replay_updated_at: Option<i64> = if has_replay_exclusions(&conn)? {
            conn.query_row(
                "SELECT MAX(reverted_at_ms) FROM conversation_replay_exclusions
                 WHERE session_id = ?",
                params![session_id],
                |row| row.get(0),
            )?
        } else {
            None
        };
        Ok(ConversationMetadata {
            updated_at_ms: [
                title.as_ref().map(|(_, timestamp)| *timestamp),
                message_updated_at,
                replay_updated_at,
            ]
            .into_iter()
            .flatten()
            .max(),
            title: title.map(|(title, _)| title),
        })
    }

    pub(crate) fn conversation_snapshot(
        &self,
        session_id: &str,
        before_user_turn: Option<u32>,
    ) -> Result<ConversationSnapshot, ConversationMemoryError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let mut bindings = conversation_bindings::state(&tx, session_id)?;
        if bindings.oversized {
            return Err(ConversationMemoryError::SnapshotTooLarge);
        }
        let mut rows = read_active_rows(&tx, session_id, false)?;
        let replay_rows = read_active_rows(&tx, session_id, true)?;
        let revision = replay_revision(&replay_rows, &bindings)?;

        if let Some(requested) = before_user_turn {
            let prompt_ids = bindings
                .members
                .iter()
                .filter(|binding| binding.is_user_prompt)
                .map(|binding| binding.message_id)
                .collect::<BTreeSet<_>>();
            let starts = rows
                .iter()
                .enumerate()
                .filter_map(|(index, row)| prompt_ids.contains(&row.id).then_some(index))
                .collect::<Vec<_>>();
            if requested as usize > starts.len() {
                return Err(ConversationMemoryError::InvalidUserTurns {
                    requested,
                    available: starts.len(),
                });
            }
            let end = if requested == 0 {
                0
            } else if requested as usize == starts.len() {
                rows.len()
            } else {
                starts[requested as usize]
            };
            rows.truncate(end);
        }

        let retained_ids = rows.iter().map(|row| row.id).collect::<BTreeSet<_>>();
        bindings
            .members
            .retain(|binding| retained_ids.contains(&binding.message_id));
        let prompt = tx
            .query_row(
                "SELECT prompt_hash, prompt_version, ts_ms
                 FROM session_system_prompts
                 WHERE session_id = ?",
                [session_id],
                |row| {
                    Ok(FrozenPrompt {
                        hash: row.get(0)?,
                        version: row.get(1)?,
                        ts_ms: row.get(2)?,
                    })
                },
            )
            .optional()?;
        tx.commit()?;
        Ok(ConversationSnapshot {
            rows,
            prompt,
            bindings,
            revision,
        })
    }

    pub(crate) fn install_conversation_snapshot(
        &self,
        session_id: &str,
        snapshot: &ConversationSnapshot,
    ) -> Result<(), ConversationMemoryError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id = ?1)
                 OR EXISTS(SELECT 1 FROM session_titles WHERE session_id = ?1)
                 OR EXISTS(SELECT 1 FROM session_system_prompts WHERE session_id = ?1)
                 OR EXISTS(SELECT 1 FROM tool_invocations WHERE session_id = ?1)
                 OR EXISTS(
                     SELECT 1 FROM conversation_message_tasks WHERE session_id = ?1
                 )",
            [session_id],
            |row| row.get(0),
        )?;
        if exists {
            return Err(ConversationMemoryError::DestinationExists);
        }

        let mut mapped = BTreeMap::new();
        {
            let mut insert = tx.prepare(
                "INSERT INTO messages(
                     session_id, role, content, ts_ms,
                     trust_class, trust_source, trust_lineage
                 ) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )?;
            for row in &snapshot.rows {
                insert.execute(params![
                    session_id,
                    row.role,
                    row.content,
                    row.ts_ms,
                    row.trust_class,
                    row.trust_source,
                    row.trust_lineage
                ])?;
                mapped.insert(row.id, tx.last_insert_rowid());
            }
        }
        for binding in &snapshot.bindings.members {
            let Some(message_id) = mapped.get(&binding.message_id) else {
                return Err(ConversationMemoryError::SnapshotChanged);
            };
            let Some(user_message_id) = mapped.get(&binding.user_message_id) else {
                return Err(ConversationMemoryError::SnapshotChanged);
            };
            let mut copied = binding.clone();
            copied.message_id = *message_id;
            copied.user_message_id = *user_message_id;
            copied.session_id = session_id.to_string();
            conversation_bindings::insert_copied_binding(&tx, &copied)?;
        }
        if let Some(prompt) = &snapshot.prompt {
            tx.execute(
                "INSERT INTO session_system_prompts(
                     session_id, prompt_hash, prompt_version, ts_ms
                 ) VALUES (?, ?, ?, ?)",
                params![session_id, prompt.hash, prompt.version, prompt.ts_ms],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn conversation_revert_snapshot(
        &self,
        session_id: &str,
        user_turns: u32,
    ) -> Result<ConversationSnapshot, ConversationMemoryError> {
        let mut snapshot = self.conversation_snapshot(session_id, None)?;
        let start = revert_start(&snapshot.rows, user_turns, &snapshot.bindings)?;
        snapshot.rows.truncate(start);
        let retained_ids = snapshot
            .rows
            .iter()
            .map(|row| row.id)
            .collect::<BTreeSet<_>>();
        snapshot
            .bindings
            .members
            .retain(|binding| retained_ids.contains(&binding.message_id));
        Ok(snapshot)
    }

    pub(crate) fn revert_conversation_checked(
        &self,
        session_id: &str,
        user_turns: u32,
        reverted_at_ms: i64,
        expected_revision: &str,
    ) -> Result<(), ConversationMemoryError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = read_active_rows(&tx, session_id, false)?;
        let replay_rows = read_active_rows(&tx, session_id, true)?;
        let bindings = conversation_bindings::state(&tx, session_id)?;
        if replay_revision(&replay_rows, &bindings)? != expected_revision {
            return Err(ConversationMemoryError::Changed);
        }
        let start = revert_start(&rows, user_turns, &bindings)?;
        let retained_end = start
            .checked_sub(1)
            .map(|index| (rows[index].ts_ms, rows[index].id));
        {
            let mut exclude = tx.prepare(
                "INSERT INTO conversation_replay_exclusions(
                     message_id, session_id, reverted_at_ms
                 ) VALUES (?, ?, ?)",
            )?;
            for row in replay_rows
                .iter()
                .filter(|row| retained_end.is_none_or(|end| (row.ts_ms, row.id) > end))
            {
                exclude.execute(params![row.id, session_id, reverted_at_ms])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

pub(crate) fn has_replay_exclusions(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema
             WHERE type = 'table' AND name = 'conversation_replay_exclusions'
         )",
        [],
        |row| row.get(0),
    )
}

pub(crate) fn active_replay_filter(conn: &Connection) -> rusqlite::Result<&'static str> {
    if has_replay_exclusions(conn)? {
        Ok("AND NOT EXISTS (
            SELECT 1 FROM conversation_replay_exclusions AS replay_exclusion
            WHERE replay_exclusion.message_id = messages.id
              AND replay_exclusion.session_id = messages.session_id
        )")
    } else {
        Ok("")
    }
}

fn read_active_rows(
    conn: &Connection,
    session_id: &str,
    include_system: bool,
) -> Result<Vec<MessageRow>, ConversationMemoryError> {
    let filter = active_replay_filter(conn)?;
    let system_filter = if include_system {
        ""
    } else {
        "AND role <> 'system'"
    };
    let mut statement = conn.prepare(&format!(
        "SELECT id, session_id, role, content, ts_ms,
                trust_class, trust_source, trust_lineage
         FROM messages
         WHERE session_id = ? AND role <> ? {system_filter} {filter}
         ORDER BY ts_ms, id
         LIMIT {}",
        conversation_bindings::MAX_BINDING_ROWS + 1
    ))?;
    let rows = statement
        .query_map(params![session_id, INJECTED_ROLE], row_to_message)?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.len() > conversation_bindings::MAX_BINDING_ROWS
        || rows
            .iter()
            .map(|row| serde_json::to_vec(row).map(|bytes| bytes.len()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .fold(0usize, usize::saturating_add)
            > MAX_SNAPSHOT_BYTES
    {
        return Err(ConversationMemoryError::SnapshotTooLarge);
    }
    Ok(rows)
}

fn revert_start(
    rows: &[MessageRow],
    user_turns: u32,
    bindings: &BindingState,
) -> Result<usize, ConversationMemoryError> {
    let prompts = bindings
        .members
        .iter()
        .filter(|binding| binding.is_user_prompt)
        .map(|binding| binding.message_id)
        .collect::<BTreeSet<_>>();
    let starts = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| prompts.contains(&row.id).then_some(index))
        .collect::<Vec<_>>();
    if user_turns == 0 || user_turns as usize > starts.len() {
        return Err(ConversationMemoryError::InvalidUserTurns {
            requested: user_turns,
            available: starts.len(),
        });
    }
    let retained = starts.len() - user_turns as usize;
    Ok(if retained == 0 { 0 } else { starts[retained] })
}

fn replay_revision(
    rows: &[MessageRow],
    bindings: &BindingState,
) -> Result<String, ConversationMemoryError> {
    let mut digest = Sha256::new();
    digest.update(b"conversation-replay-v1\0");
    for row in rows {
        digest.update(serde_json::to_vec(row)?);
        digest.update([0]);
    }
    for (label, group) in [
        (b"active".as_slice(), bindings.members.as_slice()),
        (b"excluded".as_slice(), bindings.excluded.as_slice()),
        (b"original".as_slice(), bindings.originals.as_slice()),
    ] {
        digest.update(label);
        digest.update([0]);
        for binding in group {
            digest.update(serde_json::to_vec(binding)?);
            digest.update([0]);
        }
    }
    digest.update([u8::from(bindings.oversized)]);
    Ok(hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    include!("../../../test/unit/agent/memory/conversations.rs");
}
