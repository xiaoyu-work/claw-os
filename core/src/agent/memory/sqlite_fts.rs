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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};

use super::compaction::COMPACTION_SCHEMA;
use super::recovery::{self, MemoryLifecycleLock};

pub(crate) const INJECTED_ROLE: &str = "injected";

/// How long an opener waits for a lock held by a concurrent opener.
///
/// Armed on the connection before any statement runs, so the WAL switch
/// and the provenance migration both wait instead of failing.
const OPEN_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("memory db poisoned: {0}")]
    Poisoned(String),

    #[error("memory integrity failure: {0}")]
    Integrity(String),

    #[error("memory repair failed: {0}")]
    Repair(String),
}

impl MemoryError {
    pub fn is_integrity_failure(&self) -> bool {
        match self {
            Self::Integrity(_) => true,
            Self::Sqlite(error) => {
                if let rusqlite::Error::SqliteFailure(code, _) = error {
                    if matches!(
                        code.code,
                        rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
                    ) {
                        return true;
                    }
                }
                let message = error.to_string().to_ascii_lowercase();
                message.contains("database disk image is malformed")
                    || message.contains("not a database")
                    || message.contains("database corruption")
            }
            Self::Io(_) | Self::Poisoned(_) | Self::Repair(_) => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub ts_ms: i64,
    /// Provenance recorded when the row was written.
    ///
    /// `None` for every row written before provenance columns existed.
    /// A legacy row therefore resolves to
    /// [`TrustClass::LegacyUnknown`](crate::agent::trust::TrustClass::LegacyUnknown)
    /// via [`MessageRow::trust_class`], never to a trusted class.
    pub trust_class: Option<String>,
    pub trust_source: Option<String>,
    pub trust_lineage: Option<String>,
}

impl MessageRow {
    /// The row's trust class.
    ///
    /// Stored tags go through
    /// [`TrustClass::from_stored_label`](crate::agent::trust::TrustClass::from_stored_label),
    /// so a row whose column was tampered with to read `system-policy`
    /// still resolves to `LegacyUnknown`. A missing column is a legacy
    /// row and resolves the same way.
    pub fn trust_class(&self) -> crate::agent::trust::TrustClass {
        match &self.trust_class {
            Some(tag) => crate::agent::trust::TrustClass::from_stored_label(tag),
            None => crate::agent::trust::TrustClass::LegacyUnknown,
        }
    }

    /// The row's source kind. An unrecognised or missing tag is
    /// [`SourceKind::LegacyStoredRow`](crate::agent::trust::SourceKind::LegacyStoredRow).
    pub fn trust_source(&self) -> crate::agent::trust::SourceKind {
        match &self.trust_source {
            Some(tag) => crate::agent::trust::SourceKind::from_tag(tag),
            None => crate::agent::trust::SourceKind::LegacyStoredRow,
        }
    }

    /// Ordered source lineage, oldest contributor first.
    pub fn trust_lineage(&self) -> Vec<crate::agent::trust::SourceKind> {
        self.trust_lineage
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .filter(|tag| !tag.is_empty())
            .map(crate::agent::trust::SourceKind::from_tag)
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ToolInvocationRow {
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub input: String,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub success: Option<bool>,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
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

pub(super) const CONNECTION_PRAGMAS: &str = r#"
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
"#;

pub(super) const BASE_SCHEMA: &str = r#"
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

CREATE TABLE IF NOT EXISTS tool_invocations (
    session_id      TEXT NOT NULL,
    tool_call_id    TEXT NOT NULL,
    tool_name       TEXT NOT NULL,
    input           TEXT NOT NULL,
    started_at_ms   INTEGER NOT NULL,
    completed_at_ms INTEGER,
    success         INTEGER,
    latency_ms      INTEGER,
    error           TEXT,
    PRIMARY KEY (session_id, tool_call_id)
);

CREATE INDEX IF NOT EXISTS tool_invocations_session_started
    ON tool_invocations(session_id, started_at_ms);

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

pub(super) const FTS_SCHEMA: &str = r#"
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
    pub(super) conn: Arc<Mutex<Connection>>,
    pub(super) path: Option<PathBuf>,
    pub(super) compaction_locks: Arc<Mutex<HashSet<String>>>,
    _lifecycle_lock: Option<Arc<MemoryLifecycleLock>>,
}

impl MemoryDb {
    /// Open (or create) a memory DB at `path`, applying schema migrations.
    /// Parent directories are created as needed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            crate::storage::ensure_private_dir(parent)?;
        }
        let lifecycle_lock = recovery::acquire_shared_lifecycle_lock(path, true)?;
        recovery::ensure_runtime_open_allowed(path)?;
        let initialize = !path.exists()
            || std::fs::metadata(path)
                .map(|metadata| metadata.len() == 0)
                .unwrap_or(false);
        recovery::ensure_private_database_file(path)?;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        // The schema batch begins by switching the journal to WAL,
        // which needs an exclusive lock, and the provenance migration
        // needs one too. The broker, the worker and a CLI can all open
        // this database at once, so the busy timeout has to be armed
        // *before* either — a `PRAGMA busy_timeout` inside the batch is
        // too late to protect the statements ahead of it.
        let no_schema = recovery::database_has_no_user_schema(&conn)?;
        let legacy_schema = !no_schema
            && !conn
                .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'trust_class'")?
                .exists([])?;
        if initialize || no_schema || legacy_schema {
            initialize_connection(&conn)?;
        } else {
            conn.execute_batch(CONNECTION_PRAGMAS)?;
            conn.execute_batch(COMPACTION_SCHEMA).map_err(|error| {
                MemoryError::Integrity(format!(
                    "durable compaction schema migration failed: {error}"
                ))
            })?;
            migrate_provenance_columns(&conn)?;
        }
        let issues = recovery::runtime_schema_issues(&conn)?;
        if !issues.is_empty() {
            return Err(MemoryError::Integrity(format!(
                "{}; run `cos agent sessions repair --dry-run`",
                issues.join("; ")
            )));
        }
        crate::storage::set_private_file(path)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: Some(path.to_path_buf()),
            compaction_locks: Arc::new(Mutex::new(HashSet::new())),
            _lifecycle_lock: lifecycle_lock.map(Arc::new),
        })
    }

    /// Open an existing memory DB without creating files or applying
    /// migrations. Read-only UI routes use this so inspecting history
    /// cannot create or migrate agent state.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(OPEN_BUSY_TIMEOUT)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: None,
            compaction_locks: Arc::new(Mutex::new(HashSet::new())),
            _lifecycle_lock: None,
        })
    }

    /// In-memory DB — used for tests and ephemeral sessions.
    #[allow(dead_code)]
    pub fn open_in_memory() -> Result<Self, MemoryError> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(OPEN_BUSY_TIMEOUT)?;
        initialize_connection(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: None,
            compaction_locks: Arc::new(Mutex::new(HashSet::new())),
            _lifecycle_lock: None,
        })
    }

    /// Open the default system memory DB at `agent_memory_db_path()`.
    pub fn open_default() -> Result<Self, MemoryError> {
        Self::open(default_path())
    }

    /// Wrap a caller-provided connection, applying the provenance
    /// migration. Used by migration tests to model an upgrade in place.
    #[cfg(test)]
    pub(crate) fn from_connection_for_test(conn: Connection) -> Self {
        migrate_provenance_columns(&conn).expect("migrate");
        Self {
            conn: Arc::new(Mutex::new(conn)),
            path: None,
            compaction_locks: Arc::new(Mutex::new(HashSet::new())),
            _lifecycle_lock: None,
        }
    }

    /// Re-run the provenance migration, to prove it is idempotent.
    #[cfg(test)]
    pub(crate) fn run_provenance_migration_for_test(&self) -> Result<(), MemoryError> {
        let conn = self.lock_conn()?;
        migrate_provenance_columns(&conn)
    }

    /// Overwrite a stored trust class, to prove reads re-clamp it.
    #[cfg(test)]
    pub(crate) fn set_trust_class_for_test(
        &self,
        session_id: &str,
        value: &str,
    ) -> Result<(), MemoryError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE messages SET trust_class = ? WHERE session_id = ?",
            params![value, session_id],
        )?;
        Ok(())
    }

    /// Overwrite a stored source tag, to prove reads re-clamp it.
    #[cfg(test)]
    pub(crate) fn set_trust_source_for_test(
        &self,
        session_id: &str,
        value: &str,
    ) -> Result<(), MemoryError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE messages SET trust_source = ? WHERE session_id = ?",
            params![value, session_id],
        )?;
        Ok(())
    }

    /// Insert a single message. Returns the row id.
    ///
    /// The row carries no provenance, so replay resolves it as
    /// [`TrustClass::LegacyUnknown`](crate::agent::trust::TrustClass::LegacyUnknown).
    /// Prefer [`record_labeled_message`](Self::record_labeled_message)
    /// wherever the writer knows which source produced the bytes.
    pub fn record_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<i64, MemoryError> {
        self.record_message_at(session_id, role, content, current_ts_ms())
    }

    /// Insert a message together with its immutable provenance.
    ///
    /// The class is written from the segment, not from the caller, and
    /// is re-clamped on read, so a tampered column cannot upgrade a row.
    pub fn record_labeled_message(
        &self,
        session_id: &str,
        role: &str,
        segment: &crate::agent::trust::LabeledSegment,
        content: &str,
    ) -> Result<i64, MemoryError> {
        let lineage = segment
            .lineage()
            .iter()
            .map(|kind| kind.tag())
            .collect::<Vec<_>>()
            .join(",");
        self.record_message_labeled_at(
            session_id,
            role,
            content,
            current_ts_ms(),
            Some(segment.class().wire_tag()),
            Some(segment.kind().tag()),
            Some(&lineage),
        )
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
        self.record_message_labeled_at(session_id, role, content, ts_ms, None, None, None)
    }

    fn record_message_labeled_at(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        ts_ms: i64,
        trust_class: Option<&str>,
        trust_source: Option<&str>,
        trust_lineage: Option<&str>,
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
            "INSERT INTO messages
                 (session_id, role, content, ts_ms, trust_class, trust_source, trust_lineage)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                session_id,
                role,
                &*stored,
                ts_ms,
                trust_class,
                trust_source,
                trust_lineage
            ],
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
        let kind = crate::agent::trust::SourceKind::from_tag(source);
        self.record_message_labeled_at(
            session_id,
            INJECTED_ROLE,
            &body,
            current_ts_ms(),
            Some(kind.class().wire_tag()),
            Some(kind.tag()),
            Some(kind.tag()),
        )
    }

    pub fn record_tool_start(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        input: &str,
    ) -> Result<(), MemoryError> {
        const MAX_INPUT_CHARS: usize = 16 * 1024;
        let input = truncate_chars(input, MAX_INPUT_CHARS);
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO tool_invocations (
                 session_id, tool_call_id, tool_name, input, started_at_ms,
                 completed_at_ms, success, latency_ms, error
             ) VALUES (?, ?, ?, ?, ?, NULL, NULL, NULL, NULL)
             ON CONFLICT(session_id, tool_call_id) DO UPDATE SET
                 tool_name = excluded.tool_name,
                 input = excluded.input,
                 started_at_ms = excluded.started_at_ms,
                 completed_at_ms = NULL,
                 success = NULL,
                 latency_ms = NULL,
                 error = NULL",
            params![session_id, tool_call_id, tool_name, input, current_ts_ms()],
        )?;
        Ok(())
    }

    pub fn record_tool_result(
        &self,
        session_id: &str,
        tool_call_id: &str,
        success: bool,
        latency_ms: u64,
        error: Option<&str>,
    ) -> Result<(), MemoryError> {
        const MAX_ERROR_CHARS: usize = 16 * 1024;
        let error = error.map(|value| truncate_chars(value, MAX_ERROR_CHARS));
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE tool_invocations
             SET completed_at_ms = ?, success = ?, latency_ms = ?, error = ?
             WHERE session_id = ? AND tool_call_id = ?",
            params![
                current_ts_ms(),
                success,
                latency_ms.min(i64::MAX as u64) as i64,
                error,
                session_id,
                tool_call_id
            ],
        )?;
        Ok(())
    }

    pub fn recent_tool_invocations(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<ToolInvocationRow>, MemoryError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, tool_call_id, tool_name, input, started_at_ms,
                    completed_at_ms, success, latency_ms, error
             FROM tool_invocations
             WHERE session_id = ?
             ORDER BY started_at_ms DESC
             LIMIT ?",
        )?;
        let mut rows = stmt
            .query_map(params![session_id, limit as i64], |row| {
                Ok(ToolInvocationRow {
                    session_id: row.get(0)?,
                    tool_call_id: row.get(1)?,
                    tool_name: row.get(2)?,
                    input: row.get(3)?,
                    started_at_ms: row.get(4)?,
                    completed_at_ms: row.get(5)?,
                    success: row.get(6)?,
                    latency_ms: row
                        .get::<_, Option<i64>>(7)?
                        .map(|value| value.max(0) as u64),
                    error: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
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
        let stored = conn
            .query_row(
                "SELECT s.prompt_hash, s.prompt_version, p.prompt
                 FROM session_system_prompts AS s
                 LEFT JOIN system_prompts AS p ON p.hash = s.prompt_hash
                 WHERE s.session_id = ?",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((hash, version, prompt)) = stored else {
            return Ok(None);
        };
        let prompt = prompt.ok_or_else(|| {
            MemoryError::Integrity(format!(
                "session {session_id} references missing system prompt {hash}"
            ))
        })?;
        verify_system_prompt_hash(&hash, &prompt)?;
        if version < minimum_version {
            return Ok(None);
        }
        Ok(Some(prompt))
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
        let existing = tx
            .query_row(
                "SELECT s.prompt_hash, p.prompt
                 FROM session_system_prompts AS s
                 LEFT JOIN system_prompts AS p ON p.hash = s.prompt_hash
                 WHERE s.session_id = ?",
                params![session_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        if let Some((existing_hash, existing_prompt)) = existing {
            let existing_prompt = existing_prompt.ok_or_else(|| {
                MemoryError::Integrity(format!(
                    "session {session_id} references missing system prompt {existing_hash}"
                ))
            })?;
            verify_system_prompt_hash(&existing_hash, &existing_prompt)?;
        }
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
            return Err(MemoryError::Integrity(
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
        let (frozen_hash, frozen, frozen_version): (String, String, u32) = tx.query_row(
            "SELECT s.prompt_hash, p.prompt, s.prompt_version
             FROM session_system_prompts AS s
             JOIN system_prompts AS p ON p.hash = s.prompt_hash
             WHERE s.session_id = ?",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        verify_system_prompt_hash(&frozen_hash, &frozen)?;
        tx.execute(
            "DELETE FROM system_prompts
             WHERE NOT EXISTS (
                 SELECT 1 FROM session_system_prompts AS s
                 WHERE s.prompt_hash = system_prompts.hash
             )
               AND NOT EXISTS (
                 SELECT 1 FROM session_compactions AS c
                 WHERE c.prompt_hash = system_prompts.hash
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
            "SELECT id, session_id, role, content, ts_ms, trust_class, trust_source, trust_lineage
             FROM (
                 SELECT id, session_id, role, content, ts_ms, trust_class, trust_source, trust_lineage
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
            "SELECT id, session_id, role, content, ts_ms, trust_class, trust_source, trust_lineage
             FROM (
                 SELECT id, session_id, role, content, ts_ms, trust_class, trust_source, trust_lineage
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
            "SELECT m.id, m.session_id, m.role, m.content, m.ts_ms, m.trust_class, m.trust_source, m.trust_lineage, bm25(messages_fts) AS rank
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
                    rank: row.get::<_, f64>(8)?,
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
            "SELECT m.id, m.session_id, m.role, m.content, m.ts_ms, m.trust_class, m.trust_source, m.trust_lineage, bm25(messages_fts) AS rank
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
                    rank: row.get::<_, f64>(8)?,
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
        tx.execute(
            "DELETE FROM session_compactions WHERE session_id = ?",
            params![session_id],
        )?;
        tx.execute(
            "DELETE FROM compaction_summaries
             WHERE NOT EXISTS (
                 SELECT 1 FROM session_compactions AS c
                 WHERE c.summary_hash = compaction_summaries.hash
             )",
            [],
        )?;
        let n = tx.execute(
            "DELETE FROM messages WHERE session_id = ?",
            params![session_id],
        )?;
        tx.execute(
            "DELETE FROM tool_invocations WHERE session_id = ?",
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
             )
               AND NOT EXISTS (
                 SELECT 1 FROM session_compactions AS c
                 WHERE c.prompt_hash = system_prompts.hash
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
        tx.execute(
            "DELETE FROM session_compactions
             WHERE session_id IN (
                 SELECT DISTINCT session_id FROM messages WHERE ts_ms < ?
             )",
            params![cutoff_ts_ms],
        )?;
        tx.execute(
            "DELETE FROM compaction_summaries
             WHERE NOT EXISTS (
                 SELECT 1 FROM session_compactions AS c
                 WHERE c.summary_hash = compaction_summaries.hash
             )",
            [],
        )?;
        let messages_deleted = tx.execute(
            "DELETE FROM messages WHERE ts_ms < ?",
            params![cutoff_ts_ms],
        )?;
        tx.execute(
            "DELETE FROM tool_invocations WHERE started_at_ms < ?",
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
             )
               AND NOT EXISTS (
                 SELECT 1 FROM session_compactions AS c
                 WHERE c.prompt_hash = system_prompts.hash
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
            conn.query_row("SELECT MIN(ts_ms), MAX(ts_ms) FROM messages", [], |r| {
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
        trust_class: row.get(5).unwrap_or(None),
        trust_source: row.get(6).unwrap_or(None),
        trust_lineage: row.get(7).unwrap_or(None),
    })
}

/// Add the provenance columns to a database created before they
/// existed.
///
/// Adding a nullable column is the whole migration: existing rows keep
/// `NULL`, and `NULL` reads back as
/// [`TrustClass::LegacyUnknown`](crate::agent::trust::TrustClass::LegacyUnknown).
/// A legacy transcript therefore degrades to "provenance unknown",
/// never to "trusted", which is the fail-safe direction.
///
/// Several processes open this database concurrently — the broker, the
/// worker and a CLI can all migrate at once — so the check-then-alter
/// is inherently racy. Rather than lock, the `ALTER` is attempted and a
/// "duplicate column" failure is treated as success: another process
/// already did the work, and the end state is identical.
pub(super) fn initialize_connection(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute_batch(CONNECTION_PRAGMAS)?;
    conn.execute_batch(BASE_SCHEMA)?;
    conn.execute_batch(COMPACTION_SCHEMA)?;
    conn.execute_batch(FTS_SCHEMA)?;
    migrate_provenance_columns(conn)?;
    Ok(())
}

fn migrate_provenance_columns(conn: &Connection) -> Result<(), MemoryError> {
    for column in ["trust_class", "trust_source", "trust_lineage"] {
        let exists: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = ?")?
            .exists(params![column])?;
        if exists {
            continue;
        }
        add_column_with_retry(conn, column)?;
    }
    Ok(())
}

/// `ALTER TABLE` needs an exclusive lock, and the broker, the worker
/// and a CLI can all be opening this database at once. Retry a bounded
/// number of times on a busy/locked database, and treat "duplicate
/// column" as success because it means a racing process finished the
/// same work first.
fn add_column_with_retry(conn: &Connection, column: &str) -> Result<(), MemoryError> {
    const ATTEMPTS: u32 = 5;
    let statement = format!("ALTER TABLE messages ADD COLUMN {column} TEXT");
    for attempt in 0..ATTEMPTS {
        match conn.execute_batch(&statement) {
            Ok(()) => return Ok(()),
            Err(error) if is_duplicate_column(&error) => return Ok(()),
            Err(error) if is_busy(&error) && attempt + 1 < ATTEMPTS => {
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt as u64 + 1)));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Whether `error` is SQLite reporting that the column another process
/// added a moment ago already exists.
fn is_duplicate_column(error: &rusqlite::Error) -> bool {
    error.to_string().contains("duplicate column name")
}

/// Whether `error` is transient lock contention rather than a real
/// schema problem.
fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy) | Some(rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn current_ts_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    truncated.push_str("\n…[truncated]");
    truncated
}

pub(super) fn system_prompt_hash(prompt: &str) -> String {
    hex::encode(Sha256::digest(prompt.as_bytes()))
}

fn verify_system_prompt_hash(hash: &str, prompt: &str) -> Result<(), MemoryError> {
    let actual = system_prompt_hash(prompt);
    if actual == hash {
        Ok(())
    } else {
        Err(MemoryError::Integrity(format!(
            "system prompt blob {hash} failed SHA-256 verification (computed {actual})"
        )))
    }
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
                let (name, input) =
                    crate::agent::tools::progressive::resolve_visible_identity(name, input)
                        .unwrap_or_else(|| (name.clone(), input.clone()));
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str("[tool_use:");
                out.push_str(&name);
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
