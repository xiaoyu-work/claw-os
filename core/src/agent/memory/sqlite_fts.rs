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

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};

pub(crate) const INJECTED_ROLE: &str = "injected";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSystemPrompt {
    pub prompt: String,
    pub newly_frozen: bool,
    pub version: u32,
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

/// Per-session subset of [`MemoryStats`]. Drops the `total_sessions` /
/// `titled_sessions` fields (always 1 / 0 or 1 in this case) and adds
/// `session_id` + `title` so the result is self-describing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionStats {
    pub session_id: String,
    pub title: Option<String>,
    pub total_messages: usize,
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
-- Auto-checkpoint the WAL every 1000 frames so a long-running
-- agent doesn't accumulate an unbounded WAL on disk between
-- explicit checkpoints. 1000 frames ≈ 4 MiB at the default 4 KiB
-- page size, which is small enough not to stall writers and large
-- enough to avoid checkpointing every transaction.
PRAGMA wal_autocheckpoint = 1000;

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

CREATE TABLE IF NOT EXISTS system_prompts (
    hash        TEXT PRIMARY KEY,
    prompt      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS session_system_prompts (
    session_id     TEXT PRIMARY KEY,
    prompt_hash    TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    ts_ms          INTEGER NOT NULL,
    FOREIGN KEY(prompt_hash) REFERENCES system_prompts(hash) ON DELETE RESTRICT
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
        // Cap stored message bodies. A run-away tool that streams a
        // multi-MB blob into the conversation log would otherwise
        // bloat the FTS index for every full-text search forever.
        // Truncate at a character boundary so multi-byte UTF-8 is
        // preserved.
        const MAX_CONTENT_CHARS: usize = 64 * 1024;
        let stored: std::borrow::Cow<'_, str> = if content.chars().count() > MAX_CONTENT_CHARS {
            let truncated: String = content.chars().take(MAX_CONTENT_CHARS).collect();
            std::borrow::Cow::Owned(truncated + "\n…[truncated]")
        } else {
            std::borrow::Cow::Borrowed(content)
        };
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO messages (session_id, role, content, ts_ms) VALUES (?, ?, ?, ?)",
            params![session_id, role, &*stored, ts_ms],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Record an auto-injected system-prompt segment as its own row
    /// under `role = "injected"`. Enforces the "model-visible means
    /// logged" invariant (issue #2, point 1): every piece of variable
    /// content that reaches a model request — memory notes, due nudges,
    /// extra-file overrides — gets a durable row keyed by the
    /// per-turn session id, so a later transcript review can tell
    /// exactly which version of `MEMORY.md` / `USER.md` / nudges the
    /// model actually saw.
    ///
    /// `source` is a short stable tag (e.g. `memory_notes`,
    /// `due_nudges`, `prompt_extra`) and is prepended to `content`
    /// so the row is self-describing without schema changes.
    ///
    /// Best-effort, like [`record_message`]: a failure here must not
    /// bring down the agent loop — the caller logs and continues.
    pub fn record_injected(
        &self,
        session_id: &str,
        source: &str,
        content: &str,
    ) -> Result<i64, MemoryError> {
        let body = format!("[{source}]\n{content}");
        self.record_message_at(session_id, INJECTED_ROLE, &body, current_ts_ms())
    }

    /// Return the canonical system prompt frozen for `session_id`.
    ///
    /// The complete prompt is stored in a content-addressed table and sessions
    /// hold only its hash, so sessions with identical prompts share one blob.
    pub fn system_prompt_for(
        &self,
        session_id: &str,
        minimum_version: u32,
    ) -> Result<Option<String>, MemoryError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT p.prompt
             FROM session_system_prompts AS s
             JOIN system_prompts AS p ON p.hash = s.prompt_hash
             WHERE s.session_id = ? AND s.prompt_version >= ?",
            params![session_id, minimum_version],
            |row| row.get(0),
        )
        .optional()
        .map_err(MemoryError::from)
    }

    /// Freeze the first canonical system prompt for a session.
    ///
    /// Concurrent callers use first-writer-wins semantics. The losing caller
    /// receives the already-frozen prompt rather than its candidate, ensuring
    /// every process sends byte-identical system instructions for the session.
    pub fn freeze_system_prompt(
        &self,
        session_id: &str,
        prompt: &str,
        prompt_version: u32,
    ) -> Result<SessionSystemPrompt, MemoryError> {
        if session_id.trim().is_empty() {
            return Err(MemoryError::Poisoned(
                "cannot freeze a system prompt without a session id".to_string(),
            ));
        }

        let hash = system_prompt_hash(prompt);
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO system_prompts(hash, prompt) VALUES (?, ?)",
            params![hash, prompt],
        )?;
        let stored_for_hash: String = tx.query_row(
            "SELECT prompt FROM system_prompts WHERE hash = ?",
            params![hash],
            |row| row.get(0),
        )?;
        if stored_for_hash != prompt {
            return Err(MemoryError::Poisoned(
                "system prompt hash collision detected".to_string(),
            ));
        }

        let newly_frozen = tx.execute(
            "INSERT INTO session_system_prompts(
                 session_id, prompt_hash, prompt_version, ts_ms
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT(session_id) DO UPDATE SET
                 prompt_hash = excluded.prompt_hash,
                 prompt_version = excluded.prompt_version,
                 ts_ms = excluded.ts_ms
             WHERE session_system_prompts.prompt_version < excluded.prompt_version",
            params![session_id, hash, prompt_version, current_ts_ms()],
        )? == 1;
        let (frozen, frozen_version): (String, u32) = tx.query_row(
            "SELECT p.prompt, s.prompt_version
             FROM session_system_prompts AS s
             JOIN system_prompts AS p ON p.hash = s.prompt_hash
             WHERE s.session_id = ?",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        tx.execute(
            "DELETE FROM system_prompts
             WHERE NOT EXISTS (
                 SELECT 1 FROM session_system_prompts AS s
                 WHERE s.prompt_hash = system_prompts.hash
             )",
            [],
        )?;
        tx.commit()?;

        Ok(SessionSystemPrompt {
            prompt: frozen,
            newly_frozen,
            version: frozen_version,
        })
    }

    /// Most recent `limit` messages for `session_id`, oldest first.
    pub fn recent(&self, session_id: &str, limit: usize) -> Result<Vec<MessageRow>, MemoryError> {
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

    /// Most recent `limit` conversation rows for `session_id`, oldest first.
    ///
    /// Audit-only injected prompt rows are filtered before the limit is
    /// applied, so they neither reach continuation requests nor displace
    /// actual conversation history from the replay budget.
    pub fn recent_replayable(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MessageRow>, MemoryError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, ts_ms
             FROM (
                 SELECT id, session_id, role, content, ts_ms
                 FROM messages
                 WHERE session_id = ? AND role <> ?
                 ORDER BY ts_ms DESC, id DESC
                 LIMIT ?
             )
             ORDER BY ts_ms ASC, id ASC",
        )?;
        let rows = stmt
            .query_map(
                params![session_id, INJECTED_ROLE, limit as i64],
                row_to_message,
            )?
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

    pub fn has_session(&self, session_id: &str) -> Result<bool, MemoryError> {
        let conn = self.lock_conn()?;
        let present = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id = ?)",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(present != 0)
    }

    /// Delete every message in `session_id`. FTS index is kept in sync via
    /// the `messages_ad` trigger. Returns rows deleted.
    #[allow(dead_code)]
    pub fn clear_session(&self, session_id: &str) -> Result<usize, MemoryError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let n = tx.execute(
            "DELETE FROM messages WHERE session_id = ?",
            params![session_id],
        )?;
        tx.execute(
            "DELETE FROM session_system_prompts WHERE session_id = ?",
            params![session_id],
        )?;
        tx.execute(
            "DELETE FROM system_prompts
             WHERE NOT EXISTS (
                 SELECT 1 FROM session_system_prompts AS s
                 WHERE s.prompt_hash = system_prompts.hash
             )",
            [],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// Delete every message older than `cutoff_ts_ms` (strictly less
    /// than). Returns `(messages_deleted, sessions_fully_emptied)` so
    /// callers can report a meaningful summary. The FTS5 mirror is
    /// kept in sync via the `messages_ad` trigger; orphaned rows in
    /// `session_titles` (titles for sessions whose every message was
    /// purged) are also removed.
    pub fn purge_older_than_ms(&self, cutoff_ts_ms: i64) -> Result<PurgeStats, MemoryError> {
        let mut conn = self.lock_conn()?;
        // Wrap the COUNT → DELETE → DELETE-orphans sequence in a
        // single immediate transaction so a concurrent writer can't
        // race the title-cleanup step into deleting an entry whose
        // backing messages were just inserted.
        let tx = conn.transaction()?;
        let sessions_before: usize = tx
            .query_row("SELECT COUNT(DISTINCT session_id) FROM messages", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as usize)
            .unwrap_or(0);
        let messages_deleted = tx.execute(
            "DELETE FROM messages WHERE ts_ms < ?",
            params![cutoff_ts_ms],
        )?;
        let sessions_after: usize = tx
            .query_row("SELECT COUNT(DISTINCT session_id) FROM messages", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as usize)
            .unwrap_or(0);
        let sessions_emptied = sessions_before.saturating_sub(sessions_after);
        // Drop titles for sessions that no longer have messages.
        let titles_deleted = tx.execute(
            "DELETE FROM session_titles
             WHERE session_id NOT IN (SELECT DISTINCT session_id FROM messages)",
            [],
        )?;
        tx.execute(
            "DELETE FROM session_system_prompts
             WHERE session_id NOT IN (SELECT DISTINCT session_id FROM messages)",
            [],
        )?;
        tx.execute(
            "DELETE FROM system_prompts
             WHERE NOT EXISTS (
                 SELECT 1 FROM session_system_prompts AS s
                 WHERE s.prompt_hash = system_prompts.hash
             )",
            [],
        )?;
        tx.commit()?;
        Ok(PurgeStats {
            messages_deleted,
            sessions_emptied,
            titles_deleted,
        })
    }

    /// Read-only counterpart of [`purge_older_than_ms`]. Returns
    /// what *would* be deleted without mutating any rows, so callers
    /// can implement `--dry-run`.
    pub fn count_older_than_ms(&self, cutoff_ts_ms: i64) -> Result<PurgeStats, MemoryError> {
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
            .query_row("SELECT COUNT(DISTINCT session_id) FROM messages", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as usize)
            .unwrap_or(0);
        let titled_sessions: usize = conn
            .query_row("SELECT COUNT(*) FROM session_titles", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as usize)
            .unwrap_or(0);
        let count_since = |cutoff: i64| -> usize {
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
        let mut stmt =
            conn.prepare("SELECT role, COUNT(*) AS n FROM messages GROUP BY role ORDER BY n DESC")?;
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
            conn
                .query_row("SELECT MIN(ts_ms), MAX(ts_ms) FROM messages", [], |r| {
                    let lo: Option<i64> = r.get(0)?;
                    let hi: Option<i64> = r.get(1)?;
                    Ok((lo, hi))
                })
                .unwrap_or((None, None))
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

    /// Per-session twin of [`MemoryDb::stats`]. Returns zeroed buckets +
    /// empty `by_role` + None timestamps when the session has no rows
    /// (i.e., either never existed or was fully purged); the title is
    /// still surfaced from `session_titles` if it survived. Callers can
    /// detect "no such session" via `total_messages == 0 && title is None`.
    pub fn stats_for_session(
        &self,
        session_id: &str,
        now_ms: i64,
    ) -> Result<SessionStats, MemoryError> {
        const DAY_MS: i64 = 86_400_000;
        let conn = self.lock_conn()?;
        let total_messages: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?",
                params![session_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0);
        let title: Option<String> = conn
            .query_row(
                "SELECT title FROM session_titles WHERE session_id = ?",
                params![session_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        let count_since = |cutoff: i64| -> usize {
            conn.query_row(
                "SELECT COUNT(*) FROM messages
                  WHERE session_id = ? AND ts_ms >= ?",
                params![session_id, cutoff],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .unwrap_or(0)
        };
        let messages_last_1d = count_since(now_ms.saturating_sub(DAY_MS));
        let messages_last_7d = count_since(now_ms.saturating_sub(7 * DAY_MS));
        let messages_last_30d = count_since(now_ms.saturating_sub(30 * DAY_MS));
        let mut stmt = conn.prepare(
            "SELECT role, COUNT(*) AS n
             FROM messages
             WHERE session_id = ?
             GROUP BY role
             ORDER BY n DESC",
        )?;
        let by_role: Vec<(String, usize)> = stmt
            .query_map(params![session_id], |row| {
                let role: String = row.get(0)?;
                let n: i64 = row.get(1)?;
                Ok((role, n as usize))
            })?
            .filter_map(Result::ok)
            .collect();
        let (oldest_ts_ms, newest_ts_ms) = if total_messages == 0 {
            (None, None)
        } else {
            conn.query_row(
                "SELECT MIN(ts_ms), MAX(ts_ms)
                 FROM messages
                 WHERE session_id = ?",
                params![session_id],
                |r| {
                    let lo: Option<i64> = r.get(0)?;
                    let hi: Option<i64> = r.get(1)?;
                    Ok((lo, hi))
                },
            )
            .unwrap_or((None, None))
        };
        Ok(SessionStats {
            session_id: session_id.to_string(),
            title,
            total_messages,
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

    /// Like [`Self::sessions`] but ordered by message count desc, with
    /// `last_ts_ms` as a stable tiebreaker (more-recent first). Useful
    /// for "which conversations are bloating my memory.db" — the
    /// natural pre-purge / pre-clear question.
    pub fn sessions_top(&self, limit: usize) -> Result<Vec<SessionSummary>, MemoryError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.session_id,
                    MAX(m.ts_ms) AS last_ts,
                    COUNT(*)     AS n,
                    t.title      AS title
             FROM messages AS m
             LEFT JOIN session_titles AS t ON t.session_id = m.session_id
             GROUP BY m.session_id
             ORDER BY n DESC, last_ts DESC
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

    pub(crate) fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, MemoryError> {
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

pub(crate) fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRow> {
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

fn system_prompt_hash(prompt: &str) -> String {
    hex::encode(Sha256::digest(prompt.as_bytes()))
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
            crate::agent::llm::ContentBlock::Reasoning { .. } => {
                // Opaque provider state is retained in conversation history,
                // but must not pollute semantic memory or FTS results.
            }
            crate::agent::llm::ContentBlock::ToolState { .. } => {}
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory/sqlite_fts.rs"
    ));
}
