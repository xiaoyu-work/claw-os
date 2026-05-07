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

/// Summary returned by [`MemoryDb::purge_older_than_ms`] /
/// [`MemoryDb::count_older_than_ms`]. The two methods share this
/// shape so callers can switch between dry-run and apply paths
/// without re-shaping their UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgeStats {
    /// Number of `messages` rows deleted (or that would be deleted
    /// for the dry-run / count counterpart).
    pub messages_deleted: usize,
    /// Number of session ids that lost their last remaining
    /// message — i.e., sessions that fully disappeared.
    pub sessions_emptied: usize,
    /// Number of orphaned `session_titles` rows removed (titles for
    /// sessions whose message rows are now all gone).
    pub titles_deleted: usize,
}

/// Aggregate health/usage view returned by [`MemoryDb::stats`].
/// Powers `cos agent sessions stats` and is structured so the doctor
/// command can re-use it later. Recency buckets are caller-provided
/// (via `now_ms`) so the database does not implicitly pull from the
/// system clock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryStats {
    pub total_messages: usize,
    pub total_sessions: usize,
    pub titled_sessions: usize,
    pub messages_last_1d: usize,
    pub messages_last_7d: usize,
    pub messages_last_30d: usize,
    /// `(role, count)` pairs ordered by count desc.
    pub by_role: Vec<(String, usize)>,
    pub oldest_ts_ms: Option<i64>,
    pub newest_ts_ms: Option<i64>,
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

CREATE TABLE IF NOT EXISTS session_titles (
    session_id  TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    ts_ms       INTEGER NOT NULL
);
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
        self.record_message_at(session_id, role, content, current_ts_ms())
    }

    /// Record a message with an explicit timestamp. Surfaces the
    /// underlying clock-injection point that [`record_message`]
    /// abstracts away — useful for tests that need to seed rows
    /// older than the current wall-clock (e.g., purge / retention
    /// behaviour) and for backfilling imported history.
    pub fn record_message_at(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        ts_ms: i64,
    ) -> Result<i64, MemoryError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO messages (session_id, role, content, ts_ms) VALUES (?, ?, ?, ?)",
            params![session_id, role, content, ts_ms],
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

    /// Delete every message older than `cutoff_ts_ms` (strictly less
    /// than). Returns `(messages_deleted, sessions_fully_emptied)` so
    /// callers can report a meaningful summary. The FTS5 mirror is
    /// kept in sync via the `messages_ad` trigger; orphaned rows in
    /// `session_titles` (titles for sessions whose every message was
    /// purged) are also removed.
    pub fn purge_older_than_ms(
        &self,
        cutoff_ts_ms: i64,
    ) -> Result<PurgeStats, MemoryError> {
        let conn = self.lock_conn()?;
        // Count distinct sessions that will be fully emptied so we
        // can report it before the DELETE wipes the rows.
        let sessions_before: usize = conn
            .query_row(
                "SELECT COUNT(DISTINCT session_id) FROM messages",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0);
        let messages_deleted = conn.execute(
            "DELETE FROM messages WHERE ts_ms < ?",
            params![cutoff_ts_ms],
        )?;
        let sessions_after: usize = conn
            .query_row(
                "SELECT COUNT(DISTINCT session_id) FROM messages",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0);
        let sessions_emptied = sessions_before.saturating_sub(sessions_after);
        // Drop titles for sessions that no longer have messages.
        let titles_deleted = conn.execute(
            "DELETE FROM session_titles
             WHERE session_id NOT IN (SELECT DISTINCT session_id FROM messages)",
            [],
        )?;
        Ok(PurgeStats {
            messages_deleted,
            sessions_emptied,
            titles_deleted,
        })
    }

    /// Read-only counterpart of [`purge_older_than_ms`]. Returns
    /// what *would* be deleted without mutating any rows, so callers
    /// can implement `--dry-run`.
    pub fn count_older_than_ms(
        &self,
        cutoff_ts_ms: i64,
    ) -> Result<PurgeStats, MemoryError> {
        let conn = self.lock_conn()?;
        let messages_deleted: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE ts_ms < ?",
                params![cutoff_ts_ms],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0);
        // Sessions that would be FULLY emptied = sessions whose max
        // ts_ms is below the cutoff.
        let sessions_emptied: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM (
                    SELECT session_id FROM messages
                    GROUP BY session_id
                    HAVING MAX(ts_ms) < ?
                 )",
                params![cutoff_ts_ms],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0);
        // Titles that would be dropped = titles for sessions that
        // would be fully emptied.
        let titles_deleted: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM session_titles t
                 WHERE NOT EXISTS (
                     SELECT 1 FROM messages m
                     WHERE m.session_id = t.session_id
                       AND m.ts_ms >= ?
                 )",
                params![cutoff_ts_ms],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0);
        Ok(PurgeStats {
            messages_deleted,
            sessions_emptied,
            titles_deleted,
        })
    }

    /// Aggregate health/usage stats. `now_ms` is injected so callers
    /// (or tests) control the recency buckets. The DB does not itself
    /// read the system clock.
    pub fn stats(&self, now_ms: i64) -> Result<MemoryStats, MemoryError> {
        const DAY_MS: i64 = 86_400_000;
        let conn = self.lock_conn()?;
        let total_messages: usize = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .unwrap_or(0);
        let total_sessions: usize = conn
            .query_row(
                "SELECT COUNT(DISTINCT session_id) FROM messages",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0);
        let titled_sessions: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM session_titles",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0);
        let mut count_since = |cutoff: i64| -> usize {
            conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE ts_ms >= ?",
                params![cutoff],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0)
        };
        let messages_last_1d = count_since(now_ms.saturating_sub(DAY_MS));
        let messages_last_7d = count_since(now_ms.saturating_sub(7 * DAY_MS));
        let messages_last_30d = count_since(now_ms.saturating_sub(30 * DAY_MS));
        // by_role tally, count desc.
        let mut stmt = conn.prepare(
            "SELECT role, COUNT(*) AS n FROM messages GROUP BY role ORDER BY n DESC",
        )?;
        let by_role: Vec<(String, usize)> = stmt
            .query_map([], |row| {
                let role: String = row.get(0)?;
                let n: i64 = row.get(1)?;
                Ok((role, n as usize))
            })?
            .filter_map(Result::ok)
            .collect();
        let (oldest_ts_ms, newest_ts_ms) = if total_messages == 0 {
            (None, None)
        } else {
            let row = conn
                .query_row(
                    "SELECT MIN(ts_ms), MAX(ts_ms) FROM messages",
                    [],
                    |r| {
                        let lo: Option<i64> = r.get(0)?;
                        let hi: Option<i64> = r.get(1)?;
                        Ok((lo, hi))
                    },
                )
                .unwrap_or((None, None));
            row
        };
        Ok(MemoryStats {
            total_messages,
            total_sessions,
            titled_sessions,
            messages_last_1d,
            messages_last_7d,
            messages_last_30d,
            by_role,
            oldest_ts_ms,
            newest_ts_ms,
        })
    }

    /// List distinct session ids ordered by most-recent activity. Useful for
    /// "what conversations have I had" UI.
    pub fn sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, MemoryError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.session_id,
                    MAX(m.ts_ms) AS last_ts,
                    COUNT(*)     AS n,
                    t.title      AS title
             FROM messages AS m
             LEFT JOIN session_titles AS t ON t.session_id = m.session_id
             GROUP BY m.session_id
             ORDER BY last_ts DESC
             LIMIT ?",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(SessionSummary {
                    session_id: row.get(0)?,
                    last_ts_ms: row.get(1)?,
                    message_count: row.get(2)?,
                    title: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Idempotently set the human-readable title for a session.
    /// Overwrites existing titles — callers decide whether to call this
    /// at most once per session (the runtime currently does so via
    /// [`Self::title_for`] guard).
    pub fn set_title(&self, session_id: &str, title: &str) -> Result<(), MemoryError> {
        let conn = self.lock_conn()?;
        let ts = current_ts_ms();
        conn.execute(
            "INSERT INTO session_titles(session_id, title, ts_ms)
             VALUES (?, ?, ?)
             ON CONFLICT(session_id) DO UPDATE SET title = excluded.title, ts_ms = excluded.ts_ms",
            params![session_id, title, ts],
        )?;
        Ok(())
    }

    /// Look up the stored title for a session. Returns `Ok(None)` when
    /// no title has been set yet.
    pub fn title_for(&self, session_id: &str) -> Result<Option<String>, MemoryError> {
        let conn = self.lock_conn()?;
        let mut stmt =
            conn.prepare("SELECT title FROM session_titles WHERE session_id = ? LIMIT 1")?;
        let mut rows = stmt.query(params![session_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
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
    /// Human-readable title for the session, when one has been
    /// generated and stored via [`MemoryDb::set_title`].
    pub title: Option<String>,
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
    fn purge_older_than_ms_drops_only_below_cutoff() {
        let db = db();
        db.record_message_at("a", "user", "ancient", 100).unwrap();
        db.record_message_at("a", "user", "less ancient", 500).unwrap();
        db.record_message_at("b", "user", "fresh", 5000).unwrap();
        let stats = db.purge_older_than_ms(1000).unwrap();
        assert_eq!(stats.messages_deleted, 2);
        assert_eq!(stats.sessions_emptied, 1);
        assert_eq!(stats.titles_deleted, 0);
        assert_eq!(db.count_total().unwrap(), 1);
        // FTS index must also be in sync (trigger drops mirror rows).
        let hits = db.search("ancient", 10).unwrap();
        assert!(hits.is_empty(), "FTS should drop purged rows");
    }

    #[test]
    fn purge_older_than_ms_drops_orphaned_titles() {
        let db = db();
        db.record_message_at("old", "user", "x", 100).unwrap();
        db.set_title("old", "Old").unwrap();
        db.record_message_at("new", "user", "y", 5000).unwrap();
        db.set_title("new", "New").unwrap();
        let stats = db.purge_older_than_ms(1000).unwrap();
        assert_eq!(stats.messages_deleted, 1);
        assert_eq!(stats.sessions_emptied, 1);
        assert_eq!(stats.titles_deleted, 1);
        assert!(db.title_for("old").unwrap().is_none());
        assert_eq!(
            db.title_for("new").unwrap().as_deref(),
            Some("New"),
            "non-orphaned title must be preserved"
        );
    }

    #[test]
    fn count_older_than_ms_does_not_mutate() {
        let db = db();
        db.record_message_at("old", "user", "x", 100).unwrap();
        db.set_title("old", "Old").unwrap();
        db.record_message_at("new", "user", "y", 5000).unwrap();
        let count = db.count_older_than_ms(1000).unwrap();
        assert_eq!(count.messages_deleted, 1);
        assert_eq!(count.sessions_emptied, 1);
        assert_eq!(count.titles_deleted, 1);
        // Nothing actually removed.
        assert_eq!(db.count_total().unwrap(), 2);
        assert_eq!(db.title_for("old").unwrap().as_deref(), Some("Old"));
    }

    #[test]
    fn purge_older_than_ms_boundary_is_strict_less_than() {
        let db = db();
        db.record_message_at("s", "user", "exact", 1000).unwrap();
        let stats = db.purge_older_than_ms(1000).unwrap();
        assert_eq!(stats.messages_deleted, 0, "row at exact cutoff must be kept");
        assert_eq!(db.count_total().unwrap(), 1);
    }

    #[test]
    fn purge_older_than_ms_partial_session_keeps_session() {
        let db = db();
        db.record_message_at("s", "user", "first", 100).unwrap();
        db.record_message_at("s", "user", "second", 5000).unwrap();
        let stats = db.purge_older_than_ms(1000).unwrap();
        assert_eq!(stats.messages_deleted, 1);
        assert_eq!(
            stats.sessions_emptied, 0,
            "session still has a remaining row, must not count as emptied"
        );
        assert_eq!(db.count_session("s").unwrap(), 1);
    }

    #[test]
    fn stats_empty_db_returns_zero_with_no_extremes() {
        let db = db();
        let s = db.stats(10_000).unwrap();
        assert_eq!(s.total_messages, 0);
        assert_eq!(s.total_sessions, 0);
        assert_eq!(s.titled_sessions, 0);
        assert_eq!(s.messages_last_1d, 0);
        assert_eq!(s.messages_last_7d, 0);
        assert_eq!(s.messages_last_30d, 0);
        assert!(s.by_role.is_empty());
        assert!(s.oldest_ts_ms.is_none());
        assert!(s.newest_ts_ms.is_none());
    }

    #[test]
    fn stats_buckets_recency_correctly() {
        let db = db();
        let now: i64 = 100 * 86_400_000; // day 100 in ms
        // 1 row at now-2h, 1 at now-3d, 1 at now-15d, 1 at now-60d.
        db.record_message_at("s", "user", "a", now - 2 * 3_600_000).unwrap();
        db.record_message_at("s", "user", "b", now - 3 * 86_400_000).unwrap();
        db.record_message_at("s", "user", "c", now - 15 * 86_400_000).unwrap();
        db.record_message_at("t", "user", "d", now - 60 * 86_400_000).unwrap();
        let s = db.stats(now).unwrap();
        assert_eq!(s.total_messages, 4);
        assert_eq!(s.total_sessions, 2);
        assert_eq!(s.messages_last_1d, 1, "only the 2h-old row");
        assert_eq!(s.messages_last_7d, 2, "2h + 3d");
        assert_eq!(s.messages_last_30d, 3, "2h + 3d + 15d");
        assert_eq!(s.oldest_ts_ms, Some(now - 60 * 86_400_000));
        assert_eq!(s.newest_ts_ms, Some(now - 2 * 3_600_000));
    }

    #[test]
    fn stats_by_role_is_count_desc() {
        let db = db();
        db.record_message("s", "user", "1").unwrap();
        db.record_message("s", "user", "2").unwrap();
        db.record_message("s", "user", "3").unwrap();
        db.record_message("s", "assistant", "ok").unwrap();
        db.record_message("s", "system", "init").unwrap();
        let s = db.stats(0).unwrap();
        assert_eq!(s.by_role[0], ("user".into(), 3));
        // Tied roles can land in either order but both must be present.
        let names: Vec<&str> = s.by_role.iter().map(|(r, _)| r.as_str()).collect();
        assert!(names.contains(&"assistant"));
        assert!(names.contains(&"system"));
    }

    #[test]
    fn stats_titled_sessions_counts_session_titles_rows() {
        let db = db();
        db.record_message("a", "user", "x").unwrap();
        db.record_message("b", "user", "y").unwrap();
        db.set_title("a", "Alpha").unwrap();
        let s = db.stats(0).unwrap();
        assert_eq!(s.total_sessions, 2);
        assert_eq!(s.titled_sessions, 1);
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
    fn title_for_returns_none_when_unset() {
        let db = db();
        db.record_message("s", "user", "x").unwrap();
        assert!(db.title_for("s").unwrap().is_none());
    }

    #[test]
    fn set_title_persists_and_reads_back() {
        let db = db();
        db.set_title("s1", "Hello session").unwrap();
        assert_eq!(db.title_for("s1").unwrap().as_deref(), Some("Hello session"));
    }

    #[test]
    fn set_title_overwrites_existing() {
        let db = db();
        db.set_title("s1", "first").unwrap();
        db.set_title("s1", "second").unwrap();
        assert_eq!(db.title_for("s1").unwrap().as_deref(), Some("second"));
    }

    #[test]
    fn sessions_returns_title_when_present() {
        let db = db();
        db.record_message("s1", "user", "hi").unwrap();
        db.record_message("s2", "user", "hi").unwrap();
        db.set_title("s2", "labelled").unwrap();
        let summaries = db.sessions(10).unwrap();
        let m: std::collections::HashMap<_, _> = summaries
            .iter()
            .map(|s| (s.session_id.clone(), s.title.clone()))
            .collect();
        assert_eq!(m.get("s1").cloned().flatten(), None);
        assert_eq!(m.get("s2").cloned().flatten().as_deref(), Some("labelled"));
    }

    #[test]
    fn title_for_unknown_session_is_none() {
        let db = db();
        assert!(db.title_for("never-recorded").unwrap().is_none());
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
