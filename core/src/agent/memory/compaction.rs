//! Durable session compaction lifecycle and continuation projection.
//!
//! Raw `messages` rows remain authoritative and searchable. A completed
//! compaction is a content-addressed projection over an exact inclusive row-id
//! range, with a digest that lets continuation loading reject stale or damaged
//! summaries. Per-session advisory locks span the provider call, so a second
//! process cannot summarize the same head concurrently; a `started` row found
//! after that lock is reacquired is an interrupted attempt and is closed as
//! `failed` before retry.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::sqlite_fts::{MemoryDb, MemoryError, MessageRow, INJECTED_ROLE};

pub(super) const COMPACTION_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS compaction_summaries (
    hash        TEXT PRIMARY KEY,
    summary     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS session_compactions (
    id                         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id                 TEXT NOT NULL,
    generation                 INTEGER NOT NULL,
    state                      TEXT NOT NULL
                               CHECK(state IN ('started', 'completed', 'failed')),
    started_ts_ms              INTEGER NOT NULL,
    finished_ts_ms             INTEGER,
    source_start_id            INTEGER NOT NULL,
    source_end_id              INTEGER NOT NULL,
    source_count               INTEGER NOT NULL,
    source_ids_json            TEXT NOT NULL,
    source_digest              TEXT NOT NULL,
    algorithm                  TEXT NOT NULL,
    algorithm_version          INTEGER NOT NULL,
    protected_tail_start_id    INTEGER,
    protected_user_message_id  INTEGER,
    summary_hash               TEXT,
    prompt_hash                TEXT,
    prompt_version             INTEGER,
    provider                   TEXT NOT NULL,
    model                      TEXT NOT NULL,
    previous_compaction_id     INTEGER,
    recovery_metadata          TEXT NOT NULL,
    failure_kind               TEXT,
    CHECK(generation > 0),
    CHECK(source_start_id > 0),
    CHECK(source_end_id >= source_start_id),
    CHECK(source_count > 0),
    CHECK(algorithm_version > 0),
    CHECK(
        (prompt_hash IS NULL AND prompt_version IS NULL)
        OR
        (prompt_hash IS NOT NULL AND prompt_version > 0)
    ),
    CHECK(
        (state = 'started' AND finished_ts_ms IS NULL
                           AND summary_hash IS NULL
                           AND failure_kind IS NULL)
        OR
        (state = 'completed' AND finished_ts_ms IS NOT NULL
                             AND summary_hash IS NOT NULL
                             AND failure_kind IS NULL)
        OR
        (state = 'failed' AND finished_ts_ms IS NOT NULL
                          AND summary_hash IS NULL
                          AND failure_kind IS NOT NULL)
    ),
    UNIQUE(session_id, generation),
    FOREIGN KEY(summary_hash) REFERENCES compaction_summaries(hash) ON DELETE RESTRICT,
    FOREIGN KEY(prompt_hash) REFERENCES system_prompts(hash) ON DELETE RESTRICT,
    FOREIGN KEY(previous_compaction_id) REFERENCES session_compactions(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS session_compactions_latest
    ON session_compactions(session_id, generation DESC);

CREATE UNIQUE INDEX IF NOT EXISTS session_compactions_one_started
    ON session_compactions(session_id)
    WHERE state = 'started';
"#;

pub const SOURCE_DIGEST_ALGORITHM: &str = "sha256-message-rows-v1";
pub const RECOVERY_METADATA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionState {
    Started,
    Completed,
    Failed,
    Invalid,
}

impl CompactionState {
    fn parse(value: &str) -> Self {
        match value {
            "started" => Self::Started,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Invalid,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionRecoveryMetadata {
    pub version: u32,
    pub source_digest_algorithm: String,
    pub source_start_id: i64,
    pub source_end_id: i64,
    pub source_count: usize,
    pub source_ids: Vec<i64>,
    pub protected_tail_start_id: Option<i64>,
    pub protected_user_message_id: Option<i64>,
    #[serde(default)]
    pub protected_tail_identity_digest: String,
    #[serde(default)]
    pub protected_user_identity_digest: String,
    pub previous_compaction_id: Option<i64>,
    pub pruned_tool_results: usize,
    pub raw_rows_searchable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompactionRecord {
    pub id: i64,
    pub session_id: String,
    pub generation: u64,
    pub state: CompactionState,
    pub started_ts_ms: i64,
    pub finished_ts_ms: Option<i64>,
    pub source_start_id: i64,
    pub source_end_id: i64,
    pub source_count: usize,
    pub source_ids: Vec<i64>,
    pub source_digest: String,
    pub algorithm: String,
    pub algorithm_version: u32,
    pub protected_tail_start_id: Option<i64>,
    pub protected_user_message_id: Option<i64>,
    pub summary_hash: Option<String>,
    pub prompt_hash: Option<String>,
    pub prompt_version: Option<u32>,
    pub provider: String,
    pub model: String,
    pub previous_compaction_id: Option<i64>,
    pub recovery_metadata: CompactionRecoveryMetadata,
    pub recovery_metadata_valid: bool,
    pub failure_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSummary {
    pub record: CompactionRecord,
    /// Exact content-addressed, model-visible summary message.
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct ContinuationProjection {
    pub summary: Option<CompactionSummary>,
    pub tail: Vec<MessageRow>,
    pub recovered_interrupted: usize,
    pub rejected_invalid: usize,
}

#[derive(Debug, Clone)]
pub struct NewCompaction {
    pub source_start_id: i64,
    pub source_end_id: i64,
    pub source_count: usize,
    pub protected_tail_start_id: Option<i64>,
    pub protected_user_message_id: Option<i64>,
    pub algorithm: String,
    pub algorithm_version: u32,
    pub provider: String,
    pub model: String,
    pub previous_compaction_id: Option<i64>,
    pub pruned_tool_results: usize,
}

#[derive(Debug)]
pub enum BeginCompaction {
    Started(CompactionAttempt),
    Busy,
    AlreadyCovered,
}

#[derive(Debug)]
pub struct CompactionAttempt {
    db: MemoryDb,
    guard: Option<CompactionLockGuard>,
    id: i64,
    generation: u64,
}

impl CompactionAttempt {
    pub fn id(&self) -> i64 {
        self.id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn complete(mut self, summary: &str) -> Result<CompactionSummary, MemoryError> {
        let summary = summary.trim();
        if summary.is_empty() {
            return Err(MemoryError::Integrity(
                "cannot complete compaction with an empty summary".to_string(),
            ));
        }
        let hash = content_hash(summary);
        let mut conn = self.db.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO compaction_summaries(hash, summary) VALUES (?, ?)",
            params![&hash, summary],
        )?;
        let stored: String = tx.query_row(
            "SELECT summary FROM compaction_summaries WHERE hash = ?",
            params![&hash],
            |row| row.get(0),
        )?;
        if stored != summary {
            return Err(MemoryError::Integrity(
                "compaction summary hash collision detected".to_string(),
            ));
        }
        let changed = tx.execute(
            "UPDATE session_compactions
             SET state = 'completed',
                 finished_ts_ms = ?,
                 summary_hash = ?,
                 failure_kind = NULL
             WHERE id = ? AND state = 'started'",
            params![current_ts_ms(), &hash, self.id],
        )?;
        if changed != 1 {
            return Err(MemoryError::Integrity(format!(
                "compaction {} no longer has a started lifecycle",
                self.id
            )));
        }
        tx.commit()?;
        drop(conn);
        let completed = self
            .db
            .compaction_by_id(self.id)?
            .ok_or_else(|| MemoryError::Integrity("completed compaction disappeared".into()))?;
        self.guard.take();
        Ok(CompactionSummary {
            record: completed,
            summary: stored,
        })
    }

    pub fn fail(mut self, failure_kind: &str) -> Result<(), MemoryError> {
        let failure_kind = bounded_identifier(failure_kind, "compaction_failed");
        let conn = self.db.lock_conn()?;
        let changed = conn.execute(
            "UPDATE session_compactions
             SET state = 'failed',
                 finished_ts_ms = ?,
                 failure_kind = ?
             WHERE id = ? AND state = 'started'",
            params![current_ts_ms(), failure_kind, self.id],
        )?;
        if changed != 1 {
            return Err(MemoryError::Integrity(format!(
                "compaction {} no longer has a started lifecycle",
                self.id
            )));
        }
        drop(conn);
        self.guard.take();
        Ok(())
    }
}

#[derive(Debug)]
struct CompactionLockGuard {
    local: Arc<Mutex<HashSet<String>>>,
    key: String,
    file: Option<File>,
}

impl Drop for CompactionLockGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(file) = self.file.as_ref() {
            use std::os::unix::io::AsRawFd;
            let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        }
        if let Ok(mut held) = self.local.lock() {
            held.remove(&self.key);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct CompactionInspection {
    pub completed: u64,
    pub failed: u64,
    pub interrupted: u64,
    pub invalid_records: u64,
    pub orphaned_sessions: u64,
    pub unreferenced_summaries: u64,
}

impl MemoryDb {
    pub fn begin_compaction(
        &self,
        session_id: &str,
        spec: NewCompaction,
    ) -> Result<BeginCompaction, MemoryError> {
        if session_id.trim().is_empty() {
            return Err(MemoryError::Integrity(
                "cannot compact without a session id".to_string(),
            ));
        }
        let Some(guard) = self.try_compaction_lock(session_id)? else {
            return Ok(BeginCompaction::Busy);
        };

        let mut conn = self.lock_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        close_interrupted_locked(&tx, session_id)?;

        let earliest_replayable: Option<i64> = tx.query_row(
            "SELECT MIN(id) FROM messages WHERE session_id = ? AND role <> ?",
            params![session_id, INJECTED_ROLE],
            |row| row.get(0),
        )?;
        let earliest_replayable = earliest_replayable.ok_or_else(|| {
            MemoryError::Integrity(
                "cannot compact a session without replayable message rows".to_string(),
            )
        })?;
        let (latest, _) = latest_valid_locked(&tx, session_id)?;
        match latest.as_ref() {
            None => {
                if spec.previous_compaction_id.is_some() {
                    return Err(MemoryError::Integrity(
                        "first compaction cannot reference a predecessor".to_string(),
                    ));
                }
                if spec.source_start_id != earliest_replayable {
                    return Err(MemoryError::Integrity(format!(
                        "first compaction must start at earliest replayable row {earliest_replayable}"
                    )));
                }
            }
            Some(previous) => {
                if spec.source_end_id <= previous.record.source_end_id {
                    return Ok(BeginCompaction::AlreadyCovered);
                }
                if spec.previous_compaction_id != Some(previous.record.id) {
                    return Err(MemoryError::Integrity(
                        "successor compaction must reference the latest valid predecessor"
                            .to_string(),
                    ));
                }
                if spec.source_start_id != previous.record.source_start_id {
                    return Err(MemoryError::Integrity(format!(
                        "successor compaction must retain predecessor source start {}",
                        previous.record.source_start_id
                    )));
                }
            }
        }

        let rows =
            source_rows_for_range(&tx, session_id, spec.source_start_id, spec.source_end_id)?;
        if rows.len() != spec.source_count
            || rows.first().map(|row| row.id) != Some(spec.source_start_id)
            || rows.last().map(|row| row.id) != Some(spec.source_end_id)
        {
            return Err(MemoryError::Integrity(
                "compaction source range does not match durable message rows".to_string(),
            ));
        }
        let protected = validate_protected_rows(
            &tx,
            session_id,
            spec.source_end_id,
            spec.protected_tail_start_id,
            spec.protected_user_message_id,
        )?;

        let source_digest = digest_rows(&rows);
        let source_ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
        let source_ids_json = serde_json::to_string(&source_ids)
            .map_err(|error| MemoryError::Integrity(error.to_string()))?;
        let generation: u64 = tx
            .query_row(
                "SELECT COALESCE(MAX(generation), 0) + 1
                 FROM session_compactions
                 WHERE session_id = ?",
                params![session_id],
                |row| row.get::<_, i64>(0),
            )?
            .max(1) as u64;
        let prompt: Option<(String, u32)> = tx
            .query_row(
                "SELECT prompt_hash, prompt_version
                 FROM session_system_prompts
                 WHERE session_id = ?",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let recovery = CompactionRecoveryMetadata {
            version: RECOVERY_METADATA_VERSION,
            source_digest_algorithm: SOURCE_DIGEST_ALGORITHM.to_string(),
            source_start_id: spec.source_start_id,
            source_end_id: spec.source_end_id,
            source_count: spec.source_count,
            source_ids: source_ids.clone(),
            protected_tail_start_id: spec.protected_tail_start_id,
            protected_user_message_id: spec.protected_user_message_id,
            protected_tail_identity_digest: message_identity_digest(&protected.tail),
            protected_user_identity_digest: message_identity_digest(&protected.user),
            previous_compaction_id: spec.previous_compaction_id,
            pruned_tool_results: spec.pruned_tool_results,
            raw_rows_searchable: true,
        };
        let recovery_json = serde_json::to_string(&recovery)
            .map_err(|error| MemoryError::Integrity(error.to_string()))?;
        let (prompt_hash, prompt_version) = prompt
            .map(|(hash, version)| (Some(hash), Some(version)))
            .unwrap_or((None, None));
        tx.execute(
            "INSERT INTO session_compactions(
                 session_id, generation, state, started_ts_ms, finished_ts_ms,
                 source_start_id, source_end_id, source_count, source_ids_json,
                 source_digest,
                 algorithm, algorithm_version, protected_tail_start_id,
                 protected_user_message_id, summary_hash, prompt_hash,
                 prompt_version, provider, model, previous_compaction_id,
                 recovery_metadata, failure_kind
             ) VALUES(
                 ?, ?, 'started', ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?,
                 ?, ?, ?, ?, ?, NULL
             )",
            params![
                session_id,
                generation as i64,
                current_ts_ms(),
                spec.source_start_id,
                spec.source_end_id,
                spec.source_count as i64,
                source_ids_json,
                source_digest,
                bounded_identifier(&spec.algorithm, "unknown"),
                spec.algorithm_version,
                spec.protected_tail_start_id,
                spec.protected_user_message_id,
                prompt_hash,
                prompt_version,
                bounded_identifier(&spec.provider, "unknown"),
                bounded_identifier(&spec.model, "unknown"),
                spec.previous_compaction_id,
                recovery_json,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(BeginCompaction::Started(CompactionAttempt {
            db: self.clone(),
            guard: Some(guard),
            id,
            generation,
        }))
    }

    pub fn continuation_projection(
        &self,
        session_id: &str,
        history_limit: usize,
        full_history_without_summary: bool,
    ) -> Result<ContinuationProjection, MemoryError> {
        let recovered_interrupted = self.recover_interrupted_compactions(session_id)?;
        let (summary, rejected_invalid) = self.latest_valid_compaction(session_id)?;
        let tail = if let Some(summary) = summary.as_ref() {
            self.replayable_after(session_id, summary.record.source_end_id)?
        } else if full_history_without_summary {
            self.replayable_after(session_id, 0)?
        } else {
            let limit = if history_limit == 0 {
                200
            } else {
                history_limit
            };
            self.recent_replayable(session_id, limit)?
        };
        Ok(ContinuationProjection {
            summary,
            tail,
            recovered_interrupted,
            rejected_invalid,
        })
    }

    pub fn latest_valid_compaction(
        &self,
        session_id: &str,
    ) -> Result<(Option<CompactionSummary>, usize), MemoryError> {
        let conn = self.lock_conn()?;
        latest_valid_locked(&conn, session_id)
    }

    pub fn compactions_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<CompactionRecord>, MemoryError> {
        let conn = self.lock_conn()?;
        let mut statement = conn.prepare(
            "SELECT id, session_id, generation, state, started_ts_ms,
                    finished_ts_ms, source_start_id, source_end_id, source_count,
                    source_ids_json, source_digest, algorithm, algorithm_version,
                    protected_tail_start_id, protected_user_message_id,
                    summary_hash, prompt_hash, prompt_version, provider, model,
                    previous_compaction_id, recovery_metadata, failure_kind
             FROM session_compactions
             WHERE session_id = ?
             ORDER BY generation",
        )?;
        let rows = statement
            .query_map(params![session_id], row_to_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn compaction_by_id(&self, id: i64) -> Result<Option<CompactionRecord>, MemoryError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT id, session_id, generation, state, started_ts_ms,
                    finished_ts_ms, source_start_id, source_end_id, source_count,
                    source_ids_json, source_digest, algorithm, algorithm_version,
                    protected_tail_start_id, protected_user_message_id,
                    summary_hash, prompt_hash, prompt_version, provider, model,
                    previous_compaction_id, recovery_metadata, failure_kind
             FROM session_compactions
             WHERE id = ?",
            params![id],
            row_to_record,
        )
        .optional()
        .map_err(MemoryError::from)
    }

    fn replayable_after(
        &self,
        session_id: &str,
        after_id: i64,
    ) -> Result<Vec<MessageRow>, MemoryError> {
        let conn = self.lock_conn()?;
        let mut statement = conn.prepare(
            "SELECT id, session_id, role, content, ts_ms
             FROM messages
             WHERE session_id = ? AND role <> ? AND id > ?
             ORDER BY id",
        )?;
        let rows = statement
            .query_map(
                params![session_id, INJECTED_ROLE, after_id],
                super::sqlite_fts::row_to_message,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn recover_interrupted_compactions(&self, session_id: &str) -> Result<usize, MemoryError> {
        let Some(_guard) = self.try_compaction_lock(session_id)? else {
            return Ok(0);
        };
        let conn = self.lock_conn()?;
        close_interrupted_locked(&conn, session_id)
    }

    fn try_compaction_lock(
        &self,
        session_id: &str,
    ) -> Result<Option<CompactionLockGuard>, MemoryError> {
        let key = session_id.to_string();
        {
            let mut held = self
                .compaction_locks
                .lock()
                .map_err(|error| MemoryError::Poisoned(error.to_string()))?;
            if !held.insert(key.clone()) {
                return Ok(None);
            }
        }

        let file = match self.path.as_deref() {
            Some(path) => match try_file_lock(path, session_id) {
                Ok(Some(file)) => Some(file),
                Ok(None) => {
                    if let Ok(mut held) = self.compaction_locks.lock() {
                        held.remove(&key);
                    }
                    return Ok(None);
                }
                Err(error) => {
                    if let Ok(mut held) = self.compaction_locks.lock() {
                        held.remove(&key);
                    }
                    return Err(error);
                }
            },
            None => None,
        };
        Ok(Some(CompactionLockGuard {
            local: self.compaction_locks.clone(),
            key,
            file,
        }))
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<CompactionRecord> {
    let state_raw: String = row.get(3)?;
    let source_ids_raw: String = row.get(9)?;
    let recovery_raw: String = row.get(21)?;
    let state = CompactionState::parse(&state_raw);
    let source_start_id = row.get(6)?;
    let source_end_id = row.get(7)?;
    let source_count = row.get::<_, i64>(8)?.max(0) as usize;
    let parsed_source_ids = serde_json::from_str::<Vec<i64>>(&source_ids_raw);
    let source_ids_valid = parsed_source_ids.is_ok();
    let source_ids = parsed_source_ids.unwrap_or_default();
    let protected_tail_start_id = row.get(13)?;
    let protected_user_message_id = row.get(14)?;
    let previous_compaction_id = row.get(20)?;
    let parsed_recovery = serde_json::from_str::<CompactionRecoveryMetadata>(&recovery_raw);
    let recovery_metadata_valid = parsed_recovery.is_ok();
    let recovery_metadata = parsed_recovery.unwrap_or(CompactionRecoveryMetadata {
        version: 0,
        source_digest_algorithm: String::new(),
        source_start_id,
        source_end_id,
        source_count,
        source_ids: source_ids.clone(),
        protected_tail_start_id,
        protected_user_message_id,
        protected_tail_identity_digest: String::new(),
        protected_user_identity_digest: String::new(),
        previous_compaction_id,
        pruned_tool_results: 0,
        raw_rows_searchable: false,
    });
    Ok(CompactionRecord {
        id: row.get(0)?,
        session_id: row.get(1)?,
        generation: row.get::<_, i64>(2)?.max(0) as u64,
        state,
        started_ts_ms: row.get(4)?,
        finished_ts_ms: row.get(5)?,
        source_start_id,
        source_end_id,
        source_count,
        source_ids,
        source_digest: row.get(10)?,
        algorithm: row.get(11)?,
        algorithm_version: row.get(12)?,
        protected_tail_start_id,
        protected_user_message_id,
        summary_hash: row.get(15)?,
        prompt_hash: row.get(16)?,
        prompt_version: row.get(17)?,
        provider: row.get(18)?,
        model: row.get(19)?,
        previous_compaction_id,
        recovery_metadata,
        recovery_metadata_valid: recovery_metadata_valid && source_ids_valid,
        failure_kind: row.get(22)?,
    })
}

fn close_interrupted_locked(conn: &Connection, session_id: &str) -> Result<usize, MemoryError> {
    Ok(conn.execute(
        "UPDATE session_compactions
         SET state = 'failed',
             finished_ts_ms = ?,
             failure_kind = 'interrupted_before_completion'
         WHERE session_id = ? AND state = 'started'",
        params![current_ts_ms(), session_id],
    )?)
}

fn latest_valid_locked(
    conn: &Connection,
    session_id: &str,
) -> Result<(Option<CompactionSummary>, usize), MemoryError> {
    let mut statement = conn.prepare(
        "SELECT c.id, c.session_id, c.generation, c.state, c.started_ts_ms,
                c.finished_ts_ms, c.source_start_id, c.source_end_id,
                c.source_count, c.source_ids_json, c.source_digest, c.algorithm,
                c.algorithm_version, c.protected_tail_start_id,
                c.protected_user_message_id, c.summary_hash, c.prompt_hash,
                c.prompt_version, c.provider, c.model,
                c.previous_compaction_id, c.recovery_metadata, c.failure_kind,
                s.summary
         FROM session_compactions AS c
         LEFT JOIN compaction_summaries AS s ON s.hash = c.summary_hash
         WHERE c.session_id = ? AND c.state = 'completed'
         ORDER BY c.generation DESC",
    )?;
    let candidates = statement
        .query_map(params![session_id], |row| {
            Ok((row_to_record(row)?, row.get::<_, Option<String>>(23)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut rejected = 0;
    for (record, summary) in candidates {
        let Some(summary) = summary else {
            rejected += 1;
            continue;
        };
        if validate_completed(conn, &record, &summary).is_ok() {
            return Ok((Some(CompactionSummary { record, summary }), rejected));
        }
        rejected += 1;
    }
    Ok((None, rejected))
}

fn validate_completed(
    conn: &Connection,
    record: &CompactionRecord,
    summary: &str,
) -> Result<(), MemoryError> {
    if record.state != CompactionState::Completed || !record.recovery_metadata_valid {
        return Err(MemoryError::Integrity(
            "compaction lifecycle or recovery metadata is invalid".to_string(),
        ));
    }
    let recovery = &record.recovery_metadata;
    if recovery.version != RECOVERY_METADATA_VERSION
        || recovery.source_digest_algorithm != SOURCE_DIGEST_ALGORITHM
        || recovery.source_start_id != record.source_start_id
        || recovery.source_end_id != record.source_end_id
        || recovery.source_count != record.source_count
        || recovery.source_ids != record.source_ids
        || recovery.protected_tail_start_id != record.protected_tail_start_id
        || recovery.protected_user_message_id != record.protected_user_message_id
        || recovery.previous_compaction_id != record.previous_compaction_id
        || !recovery.raw_rows_searchable
    {
        return Err(MemoryError::Integrity(
            "compaction recovery metadata does not match lifecycle columns".to_string(),
        ));
    }
    let summary_hash = record
        .summary_hash
        .as_deref()
        .ok_or_else(|| MemoryError::Integrity("completed compaction has no summary hash".into()))?;
    if content_hash(summary) != summary_hash {
        return Err(MemoryError::Integrity(
            "compaction summary failed SHA-256 verification".to_string(),
        ));
    }
    let rows = source_rows_for_range(
        conn,
        &record.session_id,
        record.source_start_id,
        record.source_end_id,
    )?;
    if rows.len() != record.source_count
        || rows.first().map(|row| row.id) != Some(record.source_start_id)
        || rows.last().map(|row| row.id) != Some(record.source_end_id)
        || rows.iter().map(|row| row.id).collect::<Vec<_>>() != record.source_ids
        || digest_rows(&rows) != record.source_digest
    {
        return Err(MemoryError::Integrity(
            "compaction source rows failed digest verification".to_string(),
        ));
    }
    let protected = validate_protected_rows(
        conn,
        &record.session_id,
        record.source_end_id,
        record.protected_tail_start_id,
        record.protected_user_message_id,
    )?;
    if recovery.protected_tail_identity_digest.is_empty()
        || recovery.protected_user_identity_digest.is_empty()
        || recovery.protected_tail_identity_digest != message_identity_digest(&protected.tail)
        || recovery.protected_user_identity_digest != message_identity_digest(&protected.user)
    {
        return Err(MemoryError::Integrity(
            "protected compaction rows changed after the summary was created".to_string(),
        ));
    }
    if let Some(prompt_hash) = record.prompt_hash.as_deref() {
        if record.prompt_version.is_none_or(|version| version == 0) {
            return Err(MemoryError::Integrity(
                "compaction prompt version is missing or invalid".to_string(),
            ));
        }
        let prompt: Option<String> = conn
            .query_row(
                "SELECT prompt FROM system_prompts WHERE hash = ?",
                params![prompt_hash],
                |row| row.get(0),
            )
            .optional()?;
        if prompt
            .as_deref()
            .is_none_or(|prompt| content_hash(prompt) != prompt_hash)
        {
            return Err(MemoryError::Integrity(
                "compaction prompt authority is missing or hash-invalid".to_string(),
            ));
        }
    } else if record.prompt_version.is_some() {
        return Err(MemoryError::Integrity(
            "compaction prompt version has no content hash".to_string(),
        ));
    }
    Ok(())
}

struct ProtectedRows {
    tail: MessageRow,
    user: MessageRow,
}

fn validate_protected_rows(
    conn: &Connection,
    session_id: &str,
    source_end_id: i64,
    protected_tail_start_id: Option<i64>,
    protected_user_message_id: Option<i64>,
) -> Result<ProtectedRows, MemoryError> {
    let protected_tail_start_id = protected_tail_start_id.ok_or_else(|| {
        MemoryError::Integrity("compaction requires a protected tail boundary".to_string())
    })?;
    let protected_user_message_id = protected_user_message_id.ok_or_else(|| {
        MemoryError::Integrity("compaction requires a protected real-user anchor".to_string())
    })?;
    if protected_tail_start_id <= source_end_id {
        return Err(MemoryError::Integrity(
            "protected tail boundary is not later than the source range".to_string(),
        ));
    }
    if protected_user_message_id < protected_tail_start_id {
        return Err(MemoryError::Integrity(
            "protected user anchor precedes the protected tail boundary".to_string(),
        ));
    }
    let tail = message_row_by_id(conn, session_id, protected_tail_start_id)?
        .ok_or_else(|| MemoryError::Integrity("protected tail boundary is missing".to_string()))?;
    let user = message_row_by_id(conn, session_id, protected_user_message_id)?
        .ok_or_else(|| MemoryError::Integrity("protected user anchor is missing".to_string()))?;
    let expected_tail = next_replayable_after(conn, session_id, source_end_id)?
        .ok_or_else(|| MemoryError::Integrity("compaction source has no protected tail".into()))?;
    if expected_tail.id != protected_tail_start_id {
        return Err(MemoryError::Integrity(format!(
            "protected tail boundary must be the first replayable row after source {}",
            source_end_id
        )));
    }
    if !is_real_user_row(&user) {
        return Err(MemoryError::Integrity(
            "protected user anchor is not a real user message".to_string(),
        ));
    }
    let source_end = message_row_by_id(conn, session_id, source_end_id)?
        .ok_or_else(|| MemoryError::Integrity("compaction source end is missing".to_string()))?;
    if row_has_tool_use(&source_end) || row_has_tool_result(&tail) {
        return Err(MemoryError::Integrity(
            "compaction boundary splits or strands a tool call/result pair".to_string(),
        ));
    }
    Ok(ProtectedRows { tail, user })
}

fn next_replayable_after(
    conn: &Connection,
    session_id: &str,
    id: i64,
) -> Result<Option<MessageRow>, MemoryError> {
    conn.query_row(
        "SELECT id, session_id, role, content, ts_ms
         FROM messages
         WHERE session_id = ? AND role <> ? AND id > ?
         ORDER BY id
         LIMIT 1",
        params![session_id, INJECTED_ROLE, id],
        super::sqlite_fts::row_to_message,
    )
    .optional()
    .map_err(MemoryError::from)
}

fn message_row_by_id(
    conn: &Connection,
    session_id: &str,
    id: i64,
) -> Result<Option<MessageRow>, MemoryError> {
    conn.query_row(
        "SELECT id, session_id, role, content, ts_ms
         FROM messages
         WHERE id = ? AND session_id = ? AND role <> ?",
        params![id, session_id, INJECTED_ROLE],
        super::sqlite_fts::row_to_message,
    )
    .optional()
    .map_err(MemoryError::from)
}

fn is_real_user_row(row: &MessageRow) -> bool {
    row.role == "user" && !row_has_tool_result(row) && !row.content.trim().is_empty()
}

fn row_has_tool_use(row: &MessageRow) -> bool {
    row.role == "assistant"
        && row
            .content
            .lines()
            .any(|line| line.trim_start().starts_with("[tool_use:"))
}

fn row_has_tool_result(row: &MessageRow) -> bool {
    matches!(row.role.as_str(), "user" | "tool")
        && row.content.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("[tool_result]") || line.starts_with("[tool_result:error]")
        })
}

fn message_identity_digest(row: &MessageRow) -> String {
    digest_rows(std::slice::from_ref(row))
}

fn source_rows_for_range(
    conn: &Connection,
    session_id: &str,
    start_id: i64,
    end_id: i64,
) -> Result<Vec<MessageRow>, MemoryError> {
    if start_id <= 0 || end_id < start_id {
        return Err(MemoryError::Integrity(
            "invalid compaction source range".to_string(),
        ));
    }
    let mut statement = conn.prepare(
        "SELECT id, session_id, role, content, ts_ms
         FROM messages
         WHERE session_id = ? AND role <> ? AND id BETWEEN ? AND ?
         ORDER BY id",
    )?;
    let rows = statement
        .query_map(
            params![session_id, INJECTED_ROLE, start_id, end_id],
            super::sqlite_fts::row_to_message,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn digest_rows(rows: &[MessageRow]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SOURCE_DIGEST_ALGORITHM.as_bytes());
    for row in rows {
        hasher.update(row.id.to_be_bytes());
        hash_field(&mut hasher, row.session_id.as_bytes());
        hash_field(&mut hasher, row.role.as_bytes());
        hash_field(&mut hasher, row.content.as_bytes());
        hasher.update(row.ts_ms.to_be_bytes());
    }
    hex::encode(hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn content_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn bounded_identifier(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    let value = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    value.chars().take(256).collect()
}

fn current_ts_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn compaction_lock_path(database: &Path, session_id: &str) -> Result<PathBuf, MemoryError> {
    let parent = database.parent().ok_or_else(|| {
        MemoryError::Repair(format!(
            "memory database has no parent: {}",
            database.display()
        ))
    })?;
    let name = database
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| MemoryError::Repair("memory database has no UTF-8 filename".to_string()))?;
    let digest = content_hash(session_id);
    Ok(parent.join(format!("{name}.compaction-{}.lock", &digest[..32])))
}

fn try_file_lock(database: &Path, session_id: &str) -> Result<Option<File>, MemoryError> {
    let path = compaction_lock_path(database, session_id)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(MemoryError::Repair(format!(
                "compaction lock is not a regular file: {}",
                path.display()
            )));
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options.open(&path)?;
    crate::storage::set_private_file(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(MemoryError::Io(error));
        }
    }
    Ok(Some(file))
}

pub(super) fn inspect_projection(conn: &Connection) -> Result<CompactionInspection, MemoryError> {
    let mut inspection = CompactionInspection::default();
    let mut statement = conn.prepare(
        "SELECT id, session_id, generation, state, started_ts_ms,
                finished_ts_ms, source_start_id, source_end_id, source_count,
                source_ids_json, source_digest, algorithm, algorithm_version,
                protected_tail_start_id, protected_user_message_id,
                summary_hash, prompt_hash, prompt_version, provider, model,
                previous_compaction_id, recovery_metadata, failure_kind
         FROM session_compactions
         ORDER BY id",
    )?;
    let records = statement
        .query_map([], row_to_record)?
        .collect::<Result<Vec<_>, _>>()?;
    for record in records {
        if !record.recovery_metadata_valid || record.state == CompactionState::Invalid {
            inspection.invalid_records += 1;
            continue;
        }
        match record.state {
            CompactionState::Started => inspection.interrupted += 1,
            CompactionState::Failed => inspection.failed += 1,
            CompactionState::Completed => {
                inspection.completed += 1;
                let summary = record.summary_hash.as_deref().and_then(|hash| {
                    conn.query_row(
                        "SELECT summary FROM compaction_summaries WHERE hash = ?",
                        params![hash],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .ok()
                    .flatten()
                });
                if summary
                    .as_deref()
                    .is_none_or(|summary| validate_completed(conn, &record, summary).is_err())
                {
                    inspection.invalid_records += 1;
                }
            }
            CompactionState::Invalid => unreachable!(),
        }
        if conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id = ?)",
            params![&record.session_id],
            |row| row.get::<_, i64>(0),
        )? == 0
        {
            inspection.orphaned_sessions += 1;
        }
    }
    inspection.unreferenced_summaries = conn.query_row(
        "SELECT COUNT(*)
         FROM compaction_summaries AS s
         WHERE NOT EXISTS(
             SELECT 1 FROM session_compactions AS c WHERE c.summary_hash = s.hash
         )",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64;
    Ok(inspection)
}

pub(super) fn repair_projection(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE session_compactions
         SET state = 'failed',
             finished_ts_ms = ?,
             failure_kind = 'interrupted_during_repair'
         WHERE state = 'started'",
        params![current_ts_ms()],
    )?;
    let mut invalid = Vec::new();
    {
        let mut statement = conn.prepare(
            "SELECT id, session_id, generation, state, started_ts_ms,
                    finished_ts_ms, source_start_id, source_end_id, source_count,
                    source_ids_json, source_digest, algorithm, algorithm_version,
                    protected_tail_start_id, protected_user_message_id,
                    summary_hash, prompt_hash, prompt_version, provider, model,
                    previous_compaction_id, recovery_metadata, failure_kind
             FROM session_compactions",
        )?;
        let records = statement
            .query_map([], row_to_record)?
            .collect::<Result<Vec<_>, _>>()?;
        for record in records {
            if !record.recovery_metadata_valid || record.state == CompactionState::Invalid {
                invalid.push(record.id);
                continue;
            }
            if record.state != CompactionState::Completed {
                continue;
            }
            let summary = record.summary_hash.as_deref().and_then(|hash| {
                conn.query_row(
                    "SELECT summary FROM compaction_summaries WHERE hash = ?",
                    params![hash],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten()
            });
            if summary
                .as_deref()
                .is_none_or(|summary| validate_completed(conn, &record, summary).is_err())
            {
                invalid.push(record.id);
            }
        }
    }
    for id in invalid {
        conn.execute("DELETE FROM session_compactions WHERE id = ?", params![id])?;
    }
    conn.execute(
        "DELETE FROM session_compactions
         WHERE NOT EXISTS(
             SELECT 1 FROM messages WHERE messages.session_id = session_compactions.session_id
         )",
        [],
    )?;
    conn.execute(
        "DELETE FROM compaction_summaries
         WHERE NOT EXISTS(
             SELECT 1 FROM session_compactions
             WHERE session_compactions.summary_hash = compaction_summaries.hash
         )",
        [],
    )?;
    Ok(())
}

pub(super) fn recover_projection(
    source: &Connection,
    target: &mut Connection,
    sessions: &HashSet<String>,
) -> Result<(u64, u64), MemoryError> {
    if !table_has_columns(source, "compaction_summaries", &["hash", "summary"])?
        || !table_has_columns(
            source,
            "session_compactions",
            &[
                "id",
                "session_id",
                "generation",
                "state",
                "started_ts_ms",
                "finished_ts_ms",
                "source_start_id",
                "source_end_id",
                "source_count",
                "source_ids_json",
                "source_digest",
                "algorithm",
                "algorithm_version",
                "protected_tail_start_id",
                "protected_user_message_id",
                "summary_hash",
                "prompt_hash",
                "prompt_version",
                "provider",
                "model",
                "previous_compaction_id",
                "recovery_metadata",
                "failure_kind",
            ],
        )?
    {
        return Ok((0, 0));
    }

    let tx = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut recovered = 0;
    let mut skipped = 0;
    let mut recovered_ids = HashSet::new();
    let mut statement = source.prepare(
        "SELECT id, session_id, generation, state, started_ts_ms,
                finished_ts_ms, source_start_id, source_end_id, source_count,
                source_ids_json, source_digest, algorithm, algorithm_version,
                protected_tail_start_id, protected_user_message_id,
                summary_hash, prompt_hash, prompt_version, provider, model,
                previous_compaction_id, recovery_metadata, failure_kind
         FROM session_compactions
         WHERE state = 'completed'
         ORDER BY id",
    )?;
    let records = statement
        .query_map([], row_to_record)?
        .collect::<Result<Vec<_>, _>>()?;
    for mut record in records {
        if !sessions.contains(&record.session_id) {
            skipped += 1;
            continue;
        }
        let Some(summary_hash) = record.summary_hash.clone() else {
            skipped += 1;
            continue;
        };
        let summary: Option<String> = source
            .query_row(
                "SELECT summary FROM compaction_summaries WHERE hash = ?",
                params![&summary_hash],
                |row| row.get(0),
            )
            .optional()?;
        let Some(summary) = summary.filter(|value| content_hash(value) == summary_hash) else {
            skipped += 1;
            continue;
        };
        if validate_completed(source, &record, &summary).is_err() {
            skipped += 1;
            continue;
        }
        let rows = source_rows_for_range(
            &tx,
            &record.session_id,
            record.source_start_id,
            record.source_end_id,
        )?;
        if rows.len() != record.source_count || digest_rows(&rows) != record.source_digest {
            skipped += 1;
            continue;
        }
        let protected = match validate_protected_rows(
            &tx,
            &record.session_id,
            record.source_end_id,
            record.protected_tail_start_id,
            record.protected_user_message_id,
        ) {
            Ok(protected) => protected,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if record.recovery_metadata.protected_tail_identity_digest
            != message_identity_digest(&protected.tail)
            || record.recovery_metadata.protected_user_identity_digest
                != message_identity_digest(&protected.user)
        {
            skipped += 1;
            continue;
        }
        if let Some(prompt_hash) = record.prompt_hash.clone() {
            let prompt: Option<String> = source
                .query_row(
                    "SELECT prompt FROM system_prompts WHERE hash = ?",
                    params![&prompt_hash],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(prompt) = prompt.filter(|value| content_hash(value) == prompt_hash) else {
                skipped += 1;
                continue;
            };
            tx.execute(
                "INSERT OR IGNORE INTO system_prompts(hash, prompt) VALUES (?, ?)",
                params![prompt_hash, prompt],
            )?;
        }
        if record
            .previous_compaction_id
            .is_some_and(|id| !recovered_ids.contains(&id))
        {
            record.previous_compaction_id = None;
            record.recovery_metadata.previous_compaction_id = None;
        }
        tx.execute(
            "INSERT OR IGNORE INTO compaction_summaries(hash, summary) VALUES (?, ?)",
            params![&summary_hash, summary],
        )?;
        let record_id = record.id;
        let source_ids_json = serde_json::to_string(&record.source_ids)
            .map_err(|error| MemoryError::Integrity(error.to_string()))?;
        let recovery_metadata = serde_json::to_string(&record.recovery_metadata)
            .map_err(|error| MemoryError::Integrity(error.to_string()))?;
        tx.execute(
            "INSERT INTO session_compactions(
                 id, session_id, generation, state, started_ts_ms, finished_ts_ms,
                 source_start_id, source_end_id, source_count, source_ids_json,
                 source_digest,
                 algorithm, algorithm_version, protected_tail_start_id,
                 protected_user_message_id, summary_hash, prompt_hash,
                 prompt_version, provider, model, previous_compaction_id,
                 recovery_metadata, failure_kind
             ) VALUES(
                 ?, ?, ?, 'completed', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 ?, ?, ?, ?, NULL
             )",
            params![
                record_id,
                record.session_id,
                record.generation as i64,
                record.started_ts_ms,
                record.finished_ts_ms.or(Some(record.started_ts_ms)),
                record.source_start_id,
                record.source_end_id,
                record.source_count as i64,
                source_ids_json,
                record.source_digest,
                record.algorithm,
                record.algorithm_version,
                record.protected_tail_start_id,
                record.protected_user_message_id,
                summary_hash,
                record.prompt_hash,
                record.prompt_version,
                record.provider,
                record.model,
                record.previous_compaction_id,
                recovery_metadata,
            ],
        )?;
        recovered_ids.insert(record_id);
        recovered += 1;
    }
    tx.commit()?;
    Ok((recovered, skipped))
}

pub(super) fn tables_compatible(conn: &Connection) -> Result<bool, MemoryError> {
    Ok(
        table_has_columns(conn, "compaction_summaries", &["hash", "summary"])?
            && table_has_columns(
                conn,
                "session_compactions",
                &[
                    "id",
                    "session_id",
                    "generation",
                    "state",
                    "started_ts_ms",
                    "finished_ts_ms",
                    "source_start_id",
                    "source_end_id",
                    "source_count",
                    "source_ids_json",
                    "source_digest",
                    "algorithm",
                    "algorithm_version",
                    "protected_tail_start_id",
                    "protected_user_message_id",
                    "summary_hash",
                    "prompt_hash",
                    "prompt_version",
                    "provider",
                    "model",
                    "previous_compaction_id",
                    "recovery_metadata",
                    "failure_kind",
                ],
            )?,
    )
}

fn table_has_columns(
    conn: &Connection,
    table: &str,
    expected: &[&str],
) -> Result<bool, MemoryError> {
    let object: Option<String> = conn
        .query_row(
            "SELECT type FROM sqlite_schema WHERE name = ?",
            params![table],
            |row| row.get(0),
        )
        .optional()?;
    if object.as_deref() != Some("table") {
        return Ok(false);
    }
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(expected.iter().all(|column| columns.contains(*column)))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory/compaction.rs"
    ));
}
