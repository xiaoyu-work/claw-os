//! SQLite FTS5-backed conversation history.
//!
//! Records every turn (user prompt, assistant message, tool results) under a
//! per-`ask` session UUID so the agent can later recall what happened across
//! conversations. Search uses FTS5; ordering uses `bm25`.
//!
//! Schema:
//! - `messages(id, session_id, role, content, ts_ms)` — durable rows.
//! - `messages_fts(content)` — FTS5 contentless virtual table indexed by triggers.
//!
//! The DB lives at `data_dir/agent/memory.db` by default and uses WAL mode so
//! concurrent readers (e.g. the `cos_recall` tool) don't block writers.
//!
//! Failures are designed to be non-fatal: if `record_message` errors, the
//! caller is expected to log and continue — losing a memory record is better
//! than crashing the agent loop.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags};

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("memory db poisoned: {0}")]
    Poisoned(String),
}

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub ts_ms: i64,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub row: MessageRow,
    /// FTS5 bm25 rank — lower is better. Included so the model can decide
    /// whether a match is strong enough to act on.
    pub rank: f64,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL,
    role        TEXT NOT NULL,
    content     TEXT NOT NULL,
    ts_ms       INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS messages_session_ts
    ON messages(session_id, ts_ms);

CREATE INDEX IF NOT EXISTS messages_ts
    ON messages(ts_ms);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content,
    content='messages',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS messages_ai
AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER IF NOT EXISTS messages_ad
AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content)
    VALUES('delete', old.id, old.content);
END;

CREATE TRIGGER IF NOT EXISTS messages_au
AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content)
    VALUES('delete', old.id, old.content);
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;
"#;

#[derive(Debug, Clone)]
pub struct MemoryDb {
    conn: Arc<Mutex<Connection>>,
}

impl MemoryDb {
    /// Open (or create) a memory DB at `path`, applying schema migrations.
    /// Parent directories are created as needed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// In-memory DB — used for tests and ephemeral sessions.
    #[allow(dead_code)]
    pub fn open_in_memory() -> Result<Self, MemoryError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open the default system memory DB at `agent_memory_db_path()`.
    pub fn open_default() -> Result<Self, MemoryError> {
        Self::open(default_path())
    }

    /// Insert a single message. Returns the row id.
    pub fn record_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<i64, MemoryError> {
        let ts = current_ts_ms();
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO messages (session_id, role, content, ts_ms) VALUES (?, ?, ?, ?)",
            params![session_id, role, content, ts],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Most recent `limit` messages for `session_id`, oldest first.
    pub fn recent(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MessageRow>, MemoryError> {
        let conn = self.lock_conn()?;
        // Use id as a tiebreaker — ts_ms granularity is milliseconds, but
        // inserts on a fast machine can collide. id is monotonic.
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, ts_ms
             FROM (
                 SELECT id, session_id, role, content, ts_ms
                 FROM messages
                 WHERE session_id = ?
                 ORDER BY ts_ms DESC, id DESC
                 LIMIT ?
             )
             ORDER BY ts_ms ASC, id ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id, limit as i64], row_to_message)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// FTS5 search across all messages, ranked by bm25 (best first).
    /// `query` is sanitised — every whitespace-separated word becomes a
    /// quoted phrase, then ANDed together — so users can pass arbitrary
    /// prose without worrying about FTS5 operator syntax.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, MemoryError> {
        let escaped = fts5_escape(query);
        if escaped.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.role, m.content, m.ts_ms, bm25(messages_fts) AS rank
             FROM messages_fts
             JOIN messages m ON m.id = messages_fts.rowid
             WHERE messages_fts MATCH ?
             ORDER BY rank
             LIMIT ?",
        )?;
        let hits = stmt
            .query_map(params![escaped, limit as i64], |row| {
                Ok(SearchHit {
                    row: row_to_message(row)?,
                    rank: row.get::<_, f64>(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(hits)
    }

    /// FTS5 search constrained to a single session.
    pub fn search_session(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, MemoryError> {
        let escaped = fts5_escape(query);
        if escaped.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.role, m.content, m.ts_ms, bm25(messages_fts) AS rank
             FROM messages_fts
             JOIN messages m ON m.id = messages_fts.rowid
             WHERE messages_fts MATCH ?
               AND m.session_id = ?
             ORDER BY rank
             LIMIT ?",
        )?;
        let hits = stmt
            .query_map(params![escaped, session_id, limit as i64], |row| {
                Ok(SearchHit {
                    row: row_to_message(row)?,
                    rank: row.get::<_, f64>(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(hits)
    }

    pub fn count_total(&self) -> Result<i64, MemoryError> {
        let conn = self.lock_conn()?;
        let n = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))?;
        Ok(n)
    }

    pub fn count_session(&self, session_id: &str) -> Result<i64, MemoryError> {
        let conn = self.lock_conn()?;
        let n = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?",
            params![session_id],
            |r| r.get::<_, i64>(0),
        )?;
        Ok(n)
    }

    /// Delete every message in `session_id`. FTS index is kept in sync via
    /// the `messages_ad` trigger. Returns rows deleted.
    #[allow(dead_code)]
    pub fn clear_session(&self, session_id: &str) -> Result<usize, MemoryError> {
        let conn = self.lock_conn()?;
        let n = conn.execute(
            "DELETE FROM messages WHERE session_id = ?",
            params![session_id],
        )?;
        Ok(n)
    }

    /// List distinct session ids ordered by most-recent activity. Useful for
    /// "what conversations have I had" UI.
    pub fn sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, MemoryError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, MAX(ts_ms) AS last_ts, COUNT(*) AS n
             FROM messages
             GROUP BY session_id
             ORDER BY last_ts DESC
             LIMIT ?",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(SessionSummary {
                    session_id: row.get(0)?,
                    last_ts_ms: row.get(1)?,
                    message_count: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, MemoryError> {
        self.conn
            .lock()
            .map_err(|e| MemoryError::Poisoned(e.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    pub last_ts_ms: i64,
    pub message_count: i64,
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        ts_ms: row.get(4)?,
    })
}

fn current_ts_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn default_path() -> PathBuf {
    crate::paths::agent_memory_db_path()
}

/// Escape `query` for FTS5 MATCH. Each whitespace-separated word becomes a
/// quoted phrase ("foo"), with embedded double-quotes doubled per FTS5 rules.
/// Empty queries return an empty string (caller short-circuits).
pub(crate) fn fts5_escape(query: &str) -> String {
    let mut out = String::new();
    for word in query.split_whitespace() {
        // Strip FTS5 punctuation/operators that have no useful meaning inside
        // a phrase: parentheses, colons, asterisks, dashes (column filter,
        // prefix, NEAR, NOT). We treat the input as plain prose.
        let cleaned: String = word
            .chars()
            .filter(|c| !matches!(*c, '(' | ')' | ':' | '*' | '-' | '+' | '^'))
            .collect();
        if cleaned.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push('"');
        for ch in cleaned.chars() {
            if ch == '"' {
                out.push('"');
                out.push('"');
            } else {
                out.push(ch);
            }
        }
        out.push('"');
    }
    out
}

/// Render a Message's content blocks as a single searchable text payload.
/// Used by the runtime when recording into MemoryDb.
pub fn render_message_content(msg: &crate::agent::llm::Message) -> String {
    let mut out = String::new();
    for block in &msg.content {
        match block {
            crate::agent::llm::ContentBlock::Text { text } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
            crate::agent::llm::ContentBlock::ToolUse { name, input, .. } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str("[tool_use:");
                out.push_str(name);
                out.push_str("] ");
                out.push_str(&input.to_string());
            }
            crate::agent::llm::ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(if *is_error {
                    "[tool_result:error] "
                } else {
                    "[tool_result] "
                });
                out.push_str(content);
            }
            crate::agent::llm::ContentBlock::Image { media_type, .. } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str("[image:");
                out.push_str(media_type);
                out.push(']');
            }
        }
    }
    out
}

/// Map a Role enum into the role string we store in the DB. Kept centralised
/// so the cos_recall tool can pretty-print without re-deriving the strings.
pub fn role_str(role: crate::agent::llm::Role) -> &'static str {
    match role {
        crate::agent::llm::Role::System => "system",
        crate::agent::llm::Role::User => "user",
        crate::agent::llm::Role::Assistant => "assistant",
        crate::agent::llm::Role::Tool => "tool",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> MemoryDb {
        MemoryDb::open_in_memory().unwrap()
    }

    #[test]
    fn open_in_memory_is_clean() {
        let db = db();
        assert_eq!(db.count_total().unwrap(), 0);
    }

    #[test]
    fn record_then_recent_returns_in_order() {
        let db = db();
        db.record_message("s1", "user", "first").unwrap();
        db.record_message("s1", "assistant", "second").unwrap();
        db.record_message("s1", "user", "third").unwrap();
        let rows = db.recent("s1", 10).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].content, "first");
        assert_eq!(rows[2].content, "third");
    }

    #[test]
    fn recent_respects_limit_and_returns_latest() {
        let db = db();
        for i in 0..10 {
            db.record_message("s1", "user", &format!("msg-{i}"))
                .unwrap();
        }
        let rows = db.recent("s1", 3).unwrap();
        assert_eq!(rows.len(), 3);
        // 7, 8, 9 in chronological order
        assert_eq!(rows[0].content, "msg-7");
        assert_eq!(rows[2].content, "msg-9");
    }

    #[test]
    fn recent_isolates_by_session() {
        let db = db();
        db.record_message("a", "user", "alpha").unwrap();
        db.record_message("b", "user", "bravo").unwrap();
        let a = db.recent("a", 10).unwrap();
        let b = db.recent("b", 10).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].content, "alpha");
        assert_eq!(b[0].content, "bravo");
    }

    #[test]
    fn search_finds_substring_match_via_fts() {
        let db = db();
        db.record_message("s", "user", "I love pineapples on pizza")
            .unwrap();
        db.record_message("s", "assistant", "noted: dislikes mushrooms")
            .unwrap();
        let hits = db.search("pineapples", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].row.content.contains("pineapples"));
    }

    #[test]
    fn search_multi_word_is_implicit_and() {
        let db = db();
        db.record_message("s", "user", "hot soup is delicious").unwrap();
        db.record_message("s", "user", "cold lemonade is refreshing")
            .unwrap();
        let hits = db.search("hot soup", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].row.content.contains("hot soup"));
    }

    #[test]
    fn search_returns_empty_for_no_match() {
        let db = db();
        db.record_message("s", "user", "hello world").unwrap();
        let hits = db.search("xyzzynotfound", 10).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn search_handles_punctuation_safely() {
        let db = db();
        db.record_message("s", "user", "use cos_sysinfo to inspect things")
            .unwrap();
        // Inputs like `(foo)` or `bar*` previously broke FTS5 — must be sanitised.
        let hits = db.search("cos_sysinfo (inspect)", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn search_with_empty_query_returns_empty_not_error() {
        let db = db();
        db.record_message("s", "user", "x").unwrap();
        assert!(db.search("", 10).unwrap().is_empty());
        assert!(db.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn search_session_constrains_to_session() {
        let db = db();
        db.record_message("alpha", "user", "shared word").unwrap();
        db.record_message("bravo", "user", "shared word").unwrap();
        let alpha_only = db.search_session("alpha", "shared", 10).unwrap();
        assert_eq!(alpha_only.len(), 1);
        assert_eq!(alpha_only[0].row.session_id, "alpha");
    }

    #[test]
    fn clear_session_removes_rows_and_fts() {
        let db = db();
        db.record_message("s", "user", "delete me please").unwrap();
        db.record_message("t", "user", "keep me please").unwrap();
        let n = db.clear_session("s").unwrap();
        assert_eq!(n, 1);
        assert_eq!(db.count_session("s").unwrap(), 0);
        assert_eq!(db.count_session("t").unwrap(), 1);
        // FTS must also have been cleared
        let hits = db.search("delete me please", 10).unwrap();
        assert!(hits.is_empty(), "FTS index should be cleared by trigger");
    }

    #[test]
    fn sessions_lists_most_recent_first() {
        let db = db();
        db.record_message("old", "user", "first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.record_message("new", "user", "second").unwrap();
        let sessions = db.sessions(10).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "new");
        assert_eq!(sessions[1].session_id, "old");
    }

    #[test]
    fn open_persists_and_reopens_cleanly() {
        let dir = std::env::temp_dir()
            .join(format!("cos-mem-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("memory.db");

        {
            let db = MemoryDb::open(&path).unwrap();
            db.record_message("s", "user", "persist me").unwrap();
        }
        {
            let db = MemoryDb::open(&path).unwrap();
            assert_eq!(db.count_total().unwrap(), 1);
            let hits = db.search("persist", 10).unwrap();
            assert_eq!(hits.len(), 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fts5_escape_handles_quotes_and_operators() {
        // Bare alphanumeric words.
        assert_eq!(fts5_escape("foo bar"), r#""foo" "bar""#);
        // Strip operators that would otherwise break MATCH.
        assert_eq!(fts5_escape("foo* (bar)"), r#""foo" "bar""#);
        // Embedded double quote is doubled.
        assert_eq!(fts5_escape(r#"a"b"#), r#""a""b""#);
        // All-whitespace returns empty.
        assert_eq!(fts5_escape("   "), "");
    }

    #[test]
    fn render_message_content_extracts_text_and_tool_blocks() {
        use crate::agent::llm::{ContentBlock, Message, Role};
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "let me look".into(),
                },
                ContentBlock::ToolUse {
                    id: "abc".into(),
                    name: "cos_sysinfo".into(),
                    input: serde_json::json!({"command":"info"}),
                },
            ],
        };
        let s = render_message_content(&msg);
        assert!(s.contains("let me look"));
        assert!(s.contains("[tool_use:cos_sysinfo]"));
        assert!(s.contains("\"command\""));
    }
}
