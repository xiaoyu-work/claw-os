//! Health, repair, and quarantine lifecycle for `memory.db`.
//!
//! Diagnosis copies the database/WAL/SHM family under the lifecycle lock and
//! opens only that private snapshot, treating FTS as a projection over
//! `messages`. Mutating repair is bracketed in a private append-only log and
//! takes an exclusive sibling-file lock; normal
//! [`MemoryDb`](super::sqlite_fts::MemoryDb) handles retain a shared lock for
//! their lifetime, so a database cannot be replaced beneath an active writer.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::compaction::{self, COMPACTION_SCHEMA};
use super::sqlite_fts::{
    initialize_connection, system_prompt_hash, MemoryError, BASE_SCHEMA, CONNECTION_PRAGMAS,
    FTS_SCHEMA,
};

const REPAIR_LOG_VERSION: u32 = 1;
const MAX_REPAIR_LOG_BYTES: u64 = 8 * 1024 * 1024;
const WAL_FORMAT_VERSION: u32 = 3_007_000;
const SQLITE_MAX_PAGE_NUMBER: u32 = 0xffff_fffe;
const REPLACEMENT_MARKER_TABLE: &str = "memory_repair_install";
const EXPECTED_TABLES: &[(&str, &[&str])] = &[
    (
        "messages",
        &["id", "session_id", "role", "content", "ts_ms"],
    ),
    ("session_titles", &["session_id", "title", "ts_ms"]),
    ("system_prompts", &["hash", "prompt"]),
    (
        "session_system_prompts",
        &["session_id", "prompt_hash", "prompt_version", "ts_ms"],
    ),
];
const EXPECTED_INDEXES: &[&str] = &["messages_session_ts", "messages_ts"];
const EXPECTED_COMPACTION_TABLES: &[(&str, &[&str])] = &[
    ("compaction_summaries", &["hash", "summary"]),
    (
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
    ),
];
const EXPECTED_COMPACTION_INDEXES: &[&str] = &[
    "session_compactions_latest",
    "session_compactions_one_started",
];
const EXPECTED_TRIGGERS: &[&str] = &["messages_ai", "messages_ad", "messages_au"];

#[cfg(test)]
thread_local! {
    static STANDALONE_RECOVERY_FAILPOINT: std::cell::Cell<Option<StandaloneRecoveryStage>> =
        const { std::cell::Cell::new(None) };
}

#[derive(Debug)]
pub(super) struct MemoryLifecycleLock {
    file: File,
}

impl Drop for MemoryLifecycleLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MemoryHealthCheck {
    pub status: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, u64>,
}

impl MemoryHealthCheck {
    fn ok(summary: impl Into<String>) -> Self {
        Self {
            status: "ok".to_string(),
            summary: summary.into(),
            issues: Vec::new(),
            metrics: BTreeMap::new(),
        }
    }

    fn warn(summary: impl Into<String>, issues: Vec<String>) -> Self {
        Self {
            status: "warn".to_string(),
            summary: summary.into(),
            issues,
            metrics: BTreeMap::new(),
        }
    }

    fn fail(summary: impl Into<String>, issues: Vec<String>) -> Self {
        Self {
            status: "fail".to_string(),
            summary: summary.into(),
            issues,
            metrics: BTreeMap::new(),
        }
    }

    fn with_metric(mut self, name: &str, value: u64) -> Self {
        self.metrics.insert(name.to_string(), value);
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct MemoryHealthStats {
    pub total_messages: u64,
    pub total_sessions: u64,
    pub titled_sessions: u64,
    pub messages_last_1d: u64,
    pub messages_last_7d: u64,
    pub messages_last_30d: u64,
    pub oldest_ts_ms: Option<i64>,
    pub newest_ts_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MemoryHealthReport {
    pub status: String,
    pub path: String,
    pub initialized: bool,
    pub sqlite: MemoryHealthCheck,
    pub wal: MemoryHealthCheck,
    pub schema: MemoryHealthCheck,
    pub fts: MemoryHealthCheck,
    pub prompt_references: MemoryHealthCheck,
    pub prompt_hashes: MemoryHealthCheck,
    pub compactions: MemoryHealthCheck,
    pub titles: MemoryHealthCheck,
    pub repair_lifecycle: MemoryHealthCheck,
    pub stats: Option<MemoryHealthStats>,
    pub repairable_in_place: bool,
    pub requires_quarantine: bool,
    pub planned_repairs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepairOptions {
    pub dry_run: bool,
    pub rebuild_fts: bool,
    pub allow_quarantine: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoveredRecords {
    pub messages: u64,
    pub titles: u64,
    pub prompt_references: u64,
    pub skipped_prompt_references: u64,
    #[serde(default)]
    pub compactions: u64,
    #[serde(default)]
    pub skipped_compactions: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MemoryRepairReport {
    pub status: String,
    pub dry_run: bool,
    pub changed: bool,
    pub resumed_interrupted_repair: bool,
    pub actions: Vec<String>,
    pub before: MemoryHealthReport,
    pub after: Option<MemoryHealthReport>,
    pub quarantine_path: Option<String>,
    pub quarantined_files: Vec<String>,
    pub recovered: RecoveredRecords,
    pub recovery_warning: Option<String>,
    pub repair_log_path: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RepairMode {
    InPlace,
    Quarantine,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RepairPhase {
    Started,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RepairEvent {
    version: u32,
    attempt_id: String,
    ts_ms: i64,
    phase: RepairPhase,
    mode: RepairMode,
    #[serde(default)]
    planned_actions: Vec<String>,
    #[serde(default)]
    quarantine_path: Option<String>,
    #[serde(default, alias = "salvage_source")]
    checkpoint_source: bool,
    #[serde(default)]
    recovered: RecoveredRecords,
    #[serde(default)]
    recovery_warning: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Default)]
struct RepairLogSnapshot {
    incomplete: Vec<RepairEvent>,
    last_applied: Option<RepairEvent>,
    malformed_lines: usize,
}

#[derive(Debug, Default)]
struct SchemaInspection {
    missing: Vec<String>,
    incompatible: Vec<String>,
    authoritative_incompatible: bool,
    missing_triggers: Vec<String>,
    altered_triggers: Vec<String>,
    messages_compatible: bool,
    prompts_compatible: bool,
    compactions_compatible: bool,
    titles_compatible: bool,
    fts_compatible: bool,
}

#[derive(Debug, Default)]
struct QuarantineResult {
    base: Option<PathBuf>,
    files: Vec<PathBuf>,
    recovered: RecoveredRecords,
    recovery_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ReplacementMarker {
    // Kept in the installed database so a retry after rename can prove that
    // the live file was produced by this exact repair attempt.
    attempt_id: String,
    quarantine_path: String,
    source_main_sha256: Option<String>,
    complete: bool,
    salvage_succeeded: bool,
    recovered: RecoveredRecords,
    recovery_warning: Option<String>,
}

struct DiagnosticSnapshot {
    directory: PathBuf,
    database: PathBuf,
}

impl Drop for DiagnosticSnapshot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

struct StandaloneMainCopy {
    path: PathBuf,
}

impl Drop for StandaloneMainCopy {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(wal_path(&self.path));
        let _ = fs::remove_file(shm_path(&self.path));
    }
}

#[derive(Debug, Default)]
struct StandaloneRecoveryResult {
    recovered: RecoveredRecords,
    salvage_succeeded: bool,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct WalValidation {
    bytes: u64,
    frames: u64,
    physical_frames: u64,
    commits: u64,
    page_size: u64,
    stale_tail_bytes: u64,
    wal_index_issue: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct WalIndexHint {
    mx_frame: u64,
    frame_checksum: [u32; 2],
}

pub fn diagnose_default() -> Result<MemoryHealthReport, MemoryError> {
    diagnose(&crate::paths::agent_memory_db_path())
}

pub fn diagnose(path: &Path) -> Result<MemoryHealthReport, MemoryError> {
    if !database_family_exists(path) && !repair_log_path(path).exists() {
        return diagnose_locked(path, true);
    }
    let _lock = acquire_lifecycle_lock(path, true, true)?;
    diagnose_locked(path, true)
}

pub fn repair_default(options: RepairOptions) -> Result<MemoryRepairReport, MemoryError> {
    repair(&crate::paths::agent_memory_db_path(), options)
}

pub fn repair(path: &Path, options: RepairOptions) -> Result<MemoryRepairReport, MemoryError> {
    if options.dry_run {
        let before = diagnose(path)?;
        let mut actions = before.planned_repairs.clone();
        add_forced_fts_action(&mut actions, options.rebuild_fts);
        return Ok(MemoryRepairReport {
            status: if before.requires_quarantine {
                "requires_quarantine".to_string()
            } else if actions.is_empty() {
                "healthy".to_string()
            } else {
                "planned".to_string()
            },
            dry_run: true,
            changed: false,
            resumed_interrupted_repair: before
                .repair_lifecycle
                .metrics
                .get("interrupted_attempts")
                .copied()
                .unwrap_or(0)
                > 0,
            actions,
            before,
            after: None,
            quarantine_path: None,
            quarantined_files: Vec::new(),
            recovered: RecoveredRecords::default(),
            recovery_warning: None,
            repair_log_path: repair_log_path(path).display().to_string(),
        });
    }

    let parent = path.parent().ok_or_else(|| {
        MemoryError::Repair(format!("memory database has no parent: {}", path.display()))
    })?;
    crate::storage::ensure_private_dir(parent)?;
    reject_non_regular_file(path)?;
    reject_non_regular_file(&wal_path(path))?;
    reject_non_regular_file(&shm_path(path))?;

    let _lock = acquire_exclusive_lifecycle_lock(path)?;
    let log_snapshot = read_repair_log(path)?;
    if log_snapshot.malformed_lines > 0 {
        return Err(MemoryError::Integrity(format!(
            "repair log {} contains {} malformed line(s); preserve it for inspection before retrying",
            repair_log_path(path).display(),
            log_snapshot.malformed_lines
        )));
    }
    if log_snapshot.incomplete.len() > 1 {
        return Err(MemoryError::Integrity(format!(
            "repair log {} contains multiple interrupted attempts; preserve it for inspection before retrying",
            repair_log_path(path).display()
        )));
    }

    let before = diagnose_locked(path, true)?;
    let mut actions = before.planned_repairs.clone();
    add_forced_fts_action(&mut actions, options.rebuild_fts);

    let interrupted = log_snapshot.incomplete.last().cloned();
    let failed_quarantine = log_snapshot
        .last_applied
        .as_ref()
        .filter(|event| event.phase == RepairPhase::Failed && event.mode == RepairMode::Quarantine)
        .cloned();
    let resumed_interrupted_repair = interrupted.is_some();
    if resumed_interrupted_repair
        && !actions
            .iter()
            .any(|action| action == "resume_interrupted_repair")
    {
        actions.insert(0, "resume_interrupted_repair".to_string());
    }

    let required_mode = if before.requires_quarantine {
        RepairMode::Quarantine
    } else {
        RepairMode::InPlace
    };
    let mode = interrupted
        .as_ref()
        .map(|event| event.mode)
        .or_else(|| failed_quarantine.as_ref().map(|_| RepairMode::Quarantine))
        .unwrap_or(required_mode);
    if mode == RepairMode::Quarantine
        && !actions
            .iter()
            .any(|action| action == "quarantine_and_initialize_replacement")
    {
        actions.push("quarantine_and_initialize_replacement".to_string());
    }
    if required_mode == RepairMode::Quarantine && mode != RepairMode::Quarantine {
        if let Some(event) = interrupted.as_ref() {
            append_repair_event(
                path,
                &RepairEvent {
                    phase: RepairPhase::Failed,
                    error: Some(
                        "health changed while repair was interrupted; quarantine is now required"
                            .to_string(),
                    ),
                    ..event.clone()
                },
            )?;
        }
        return Err(MemoryError::Repair(
            "memory damage now requires quarantine; re-run with `--quarantine --yes`".to_string(),
        ));
    }
    if mode == RepairMode::Quarantine && !options.allow_quarantine {
        return Err(MemoryError::Repair(
            "repair requires preserving the damaged database in quarantine; inspect with \
             `cos agent sessions repair --dry-run`, then re-run with `--quarantine --yes`"
                .to_string(),
        ));
    }

    let checkpoint_source =
        before.sqlite.status == "ok" && before.wal.status != "fail" && path.exists();
    let attempt = if let Some(mut event) = interrupted {
        event.ts_ms = current_ts_ms();
        event.phase = RepairPhase::Started;
        event.planned_actions = actions.clone();
        event.checkpoint_source = checkpoint_source;
        event.recovered = RecoveredRecords::default();
        event.recovery_warning = None;
        event.error = None;
        append_repair_event(path, &event)?;
        event
    } else {
        let attempt_id = failed_quarantine
            .as_ref()
            .map(|event| event.attempt_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let quarantine = if mode == RepairMode::Quarantine {
            Some(
                failed_quarantine
                    .as_ref()
                    .and_then(|event| event.quarantine_path.as_deref())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| quarantine_base(path, &attempt_id)),
            )
        } else {
            None
        };
        let event = RepairEvent {
            version: REPAIR_LOG_VERSION,
            attempt_id,
            ts_ms: current_ts_ms(),
            phase: RepairPhase::Started,
            mode,
            planned_actions: actions.clone(),
            quarantine_path: quarantine.map(|value| value.display().to_string()),
            checkpoint_source,
            recovered: RecoveredRecords::default(),
            recovery_warning: None,
            error: None,
        };
        append_repair_event(path, &event)?;
        event
    };

    let mut active_attempt = attempt;
    let mut active_mode = mode;
    let mut operation = run_repair_operation(path, &actions, &active_attempt);
    if operation.is_err() && active_mode == RepairMode::InPlace && options.allow_quarantine {
        let in_place_error = operation.expect_err("checked above");
        append_repair_event(
            path,
            &RepairEvent {
                phase: RepairPhase::Failed,
                ts_ms: current_ts_ms(),
                error: Some(in_place_error.to_string()),
                ..active_attempt.clone()
            },
        )?;
        if !actions
            .iter()
            .any(|action| action == "fallback_to_quarantine")
        {
            actions.push("fallback_to_quarantine".to_string());
        }
        active_mode = RepairMode::Quarantine;
        let quarantine = quarantine_base(path, &uuid::Uuid::new_v4().simple().to_string());
        active_attempt = RepairEvent {
            version: REPAIR_LOG_VERSION,
            attempt_id: uuid::Uuid::new_v4().simple().to_string(),
            ts_ms: current_ts_ms(),
            phase: RepairPhase::Started,
            mode: active_mode,
            planned_actions: actions.clone(),
            quarantine_path: Some(quarantine.display().to_string()),
            checkpoint_source,
            recovered: RecoveredRecords::default(),
            recovery_warning: None,
            error: None,
        };
        append_repair_event(path, &active_attempt)?;
        operation = run_repair_operation(path, &actions, &active_attempt);
    }

    let quarantine = match operation {
        Ok(result) => result,
        Err(error) => {
            let log_result = append_repair_event(
                path,
                &RepairEvent {
                    phase: RepairPhase::Failed,
                    ts_ms: current_ts_ms(),
                    error: Some(error.to_string()),
                    ..active_attempt.clone()
                },
            );
            return match log_result {
                Ok(()) => Err(error),
                Err(log_error) => Err(MemoryError::Repair(format!(
                    "{error}; additionally failed to record repair failure: {log_error}"
                ))),
            };
        }
    };

    let after_without_log = diagnose_locked(path, false)?;
    if after_without_log.status != "ok" {
        let error = MemoryError::Repair(format!(
            "post-repair health check failed for {}",
            path.display()
        ));
        append_repair_event(
            path,
            &RepairEvent {
                phase: RepairPhase::Failed,
                ts_ms: current_ts_ms(),
                error: Some(error.to_string()),
                recovered: quarantine.recovered.clone(),
                recovery_warning: quarantine.recovery_warning.clone(),
                ..active_attempt.clone()
            },
        )?;
        return Err(error);
    }

    append_repair_event(
        path,
        &RepairEvent {
            phase: RepairPhase::Completed,
            ts_ms: current_ts_ms(),
            recovered: quarantine.recovered.clone(),
            recovery_warning: quarantine.recovery_warning.clone(),
            error: None,
            ..active_attempt.clone()
        },
    )?;
    let after = diagnose_locked(path, true)?;

    Ok(MemoryRepairReport {
        status: "ok".to_string(),
        dry_run: false,
        changed: !actions.is_empty(),
        resumed_interrupted_repair,
        actions,
        before,
        after: Some(after),
        quarantine_path: quarantine
            .base
            .as_ref()
            .map(|value| value.display().to_string()),
        quarantined_files: quarantine
            .files
            .iter()
            .map(|value| value.display().to_string())
            .collect(),
        recovered: quarantine.recovered,
        recovery_warning: quarantine.recovery_warning,
        repair_log_path: repair_log_path(path).display().to_string(),
    })
}

pub(super) fn acquire_shared_lifecycle_lock(
    path: &Path,
    create: bool,
) -> Result<Option<MemoryLifecycleLock>, MemoryError> {
    acquire_lifecycle_lock(path, create, false)
}

pub(super) fn ensure_runtime_open_allowed(path: &Path) -> Result<(), MemoryError> {
    let snapshot = read_repair_log(path)?;
    if snapshot.malformed_lines > 0 {
        return Err(MemoryError::Integrity(format!(
            "repair lifecycle log {} is malformed; run `cos agent sessions health`",
            repair_log_path(path).display()
        )));
    }
    if let Some(event) = snapshot.incomplete.last() {
        let mode = match event.mode {
            RepairMode::InPlace => "in-place",
            RepairMode::Quarantine => "quarantine",
        };
        return Err(MemoryError::Integrity(format!(
            "memory database has an interrupted {mode} repair ({}) and cannot be opened until \
             `cos agent sessions repair --yes{}` completes it",
            event.attempt_id,
            if event.mode == RepairMode::Quarantine {
                " --quarantine"
            } else {
                ""
            }
        )));
    }
    if snapshot.last_applied.as_ref().is_some_and(|event| {
        event.phase == RepairPhase::Failed && event.mode == RepairMode::Quarantine
    }) {
        return Err(MemoryError::Integrity(
            "the last quarantine repair failed; run `cos agent sessions health` and retry \
             `cos agent sessions repair --quarantine --yes` before opening memory"
                .to_string(),
        ));
    }
    Ok(())
}

pub(super) fn ensure_private_database_file(path: &Path) -> Result<(), MemoryError> {
    if path.exists() {
        reject_symlink(path)?;
        if !fs::metadata(path)?.is_file() {
            return Err(MemoryError::Repair(format!(
                "memory database path is not a regular file: {}",
                path.display()
            )));
        }
        crate::storage::set_private_file(path)?;
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(file) => {
            file.sync_all()?;
            crate::storage::set_private_file(path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            reject_symlink(path)?;
            crate::storage::set_private_file(path)?;
            Ok(())
        }
        Err(error) => Err(MemoryError::Io(error)),
    }
}

fn acquire_exclusive_lifecycle_lock(path: &Path) -> Result<MemoryLifecycleLock, MemoryError> {
    #[cfg(not(unix))]
    {
        let _ = path;
        return Err(MemoryError::Repair(
            "memory replacement requires Unix flock(2) serialization".to_string(),
        ));
    }
    #[cfg(unix)]
    {
        acquire_lifecycle_lock(path, true, true)?.ok_or_else(|| {
            MemoryError::Repair("failed to create memory lifecycle lock".to_string())
        })
    }
}

fn acquire_lifecycle_lock(
    path: &Path,
    create: bool,
    exclusive: bool,
) -> Result<Option<MemoryLifecycleLock>, MemoryError> {
    #[cfg(not(unix))]
    {
        let _ = (path, create, exclusive);
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        let lock_path = lifecycle_lock_path(path);
        if !create && !lock_path.exists() {
            return Ok(None);
        }
        if create {
            if let Some(parent) = lock_path.parent() {
                crate::storage::ensure_private_dir(parent)?;
            }
        }
        reject_symlink(&lock_path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        if create {
            options.create(true);
        }
        options.mode(0o600);
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        let file = options.open(&lock_path)?;
        crate::storage::set_private_file(&lock_path)?;
        let operation = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
            return Err(MemoryError::Io(std::io::Error::last_os_error()));
        }
        Ok(Some(MemoryLifecycleLock { file }))
    }
}

pub(super) fn database_has_no_user_schema(conn: &Connection) -> Result<bool, MemoryError> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(count == 0)
}

pub(super) fn runtime_schema_issues(conn: &Connection) -> Result<Vec<String>, MemoryError> {
    let schema = inspect_schema(conn)?;
    let mut issues = schema.missing;
    issues.extend(schema.incompatible);
    issues.extend(schema.missing_triggers);
    issues.extend(schema.altered_triggers);
    Ok(issues)
}

fn database_family_exists(path: &Path) -> bool {
    [path.to_path_buf(), wal_path(path), shm_path(path)]
        .iter()
        .any(|candidate| fs::symlink_metadata(candidate).is_ok())
}

fn create_diagnostic_snapshot(path: &Path) -> Result<Option<DiagnosticSnapshot>, MemoryError> {
    if !database_family_exists(path) {
        return Ok(None);
    }
    let parent = path.parent().ok_or_else(|| {
        MemoryError::Repair(format!("memory database has no parent: {}", path.display()))
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        MemoryError::Repair(format!(
            "memory database has no filename: {}",
            path.display()
        ))
    })?;
    let directory = parent.join(format!(
        ".{}.diagnose-{}",
        file_name.to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir(&directory)?;
    crate::storage::ensure_private_dir(&directory)?;
    let database = directory.join(file_name);

    let result = (|| -> Result<(), MemoryError> {
        copy_snapshot_file(path, &database)?;
        copy_snapshot_file(&wal_path(path), &wal_path(&database))?;
        copy_snapshot_file(&shm_path(path), &shm_path(&database))?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&directory);
        return Err(error);
    }
    Ok(Some(DiagnosticSnapshot {
        directory,
        database,
    }))
}

fn copy_snapshot_file(source: &Path, destination: &Path) -> Result<(), MemoryError> {
    match fs::symlink_metadata(source) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(MemoryError::Repair(format!(
                "refusing non-regular SQLite family member: {}",
                source.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(MemoryError::Io(error)),
    }

    let mut input = File::open(source)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let mut output = options.open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    crate::storage::set_private_file(destination)?;
    Ok(())
}

fn diagnose_locked(
    path: &Path,
    inspect_repair_log: bool,
) -> Result<MemoryHealthReport, MemoryError> {
    let repair_snapshot = if inspect_repair_log {
        read_repair_log(path)?
    } else {
        RepairLogSnapshot::default()
    };
    let repair_lifecycle = repair_lifecycle_check(path, &repair_snapshot);

    let original_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Ok(blocked_health_report(
                path,
                false,
                format!("cannot inspect database path: {error}"),
                inspect_wal_files(path, path),
                repair_lifecycle,
            ));
        }
    };

    if original_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Ok(blocked_health_report(
            path,
            true,
            "memory database path is not a regular file".to_string(),
            inspect_wal_files(path, path),
            repair_lifecycle,
        ));
    }

    let snapshot = create_diagnostic_snapshot(path)?;
    let inspection_path = snapshot
        .as_ref()
        .map(|snapshot| snapshot.database.as_path())
        .unwrap_or(path);
    let wal_inspection = inspect_wal_files(inspection_path, path);
    let metadata = match fs::symlink_metadata(inspection_path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Ok(blocked_health_report(
                path,
                original_metadata.is_some(),
                format!("cannot inspect database snapshot: {error}"),
                wal_inspection,
                repair_lifecycle,
            ));
        }
    };

    if metadata.is_none() {
        if wal_inspection.status == "fail" {
            return Ok(blocked_health_report(
                path,
                original_metadata.is_some(),
                "memory.db is missing while SQLite sidecars remain".to_string(),
                wal_inspection,
                repair_lifecycle,
            ));
        }
        let mut report = absent_health_report(path, wal_inspection, repair_lifecycle);
        if repair_snapshot
            .incomplete
            .iter()
            .any(|event| event.mode == RepairMode::Quarantine)
        {
            report.requires_quarantine = true;
            report.repairable_in_place = false;
            report.planned_repairs = vec![
                "resume_interrupted_repair".to_string(),
                "quarantine_and_initialize_replacement".to_string(),
            ];
        }
        finalize_health_report(&mut report);
        return Ok(report);
    }

    let conn = match Connection::open_with_flags(inspection_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    {
        Ok(conn) => conn,
        Err(error) => {
            return Ok(blocked_health_report(
                path,
                original_metadata.is_some(),
                format!("SQLite open failed: {error}"),
                wal_inspection,
                repair_lifecycle,
            ));
        }
    };
    if let Err(error) = conn.busy_timeout(Duration::from_secs(5)) {
        return Ok(blocked_health_report(
            path,
            original_metadata.is_some(),
            format!("SQLite setup failed: {error}"),
            wal_inspection,
            repair_lifecycle,
        ));
    }
    if let Err(error) = conn.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA foreign_keys = ON;")
    {
        return Ok(blocked_health_report(
            path,
            original_metadata.is_some(),
            format!("SQLite setup failed: {error}"),
            wal_inspection,
            repair_lifecycle,
        ));
    }

    let mut sqlite = check_sqlite_integrity(&conn);
    let schema_inspection = inspect_schema(&conn).unwrap_or_else(|error| SchemaInspection {
        incompatible: vec![format!("schema inspection failed: {error}")],
        authoritative_incompatible: true,
        ..SchemaInspection::default()
    });
    let schema = schema_health_check(&schema_inspection);
    let mut wal = wal_inspection;
    if let Ok(mode) = conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0)) {
        if mode.eq_ignore_ascii_case("wal") {
            wal.metrics.insert("journal_mode_wal".to_string(), 1);
        } else if wal.status != "fail" {
            wal = MemoryHealthCheck::warn(
                "database is not configured for WAL journaling",
                vec![format!("journal_mode is {mode}, expected wal")],
            );
            wal.metrics.insert("journal_mode_wal".to_string(), 0);
        }
    }

    let (fts, fts_requires_quarantine) = check_fts(&conn, &schema_inspection);
    let (prompt_references, prompt_ref_requires_quarantine) =
        check_prompt_references(&conn, &schema_inspection);
    let (prompt_hashes, prompt_hash_requires_quarantine) =
        check_prompt_hashes(&conn, &schema_inspection);
    let compactions = check_compactions(&conn, &schema_inspection);
    let titles = check_titles(&conn, &schema_inspection);
    let stats = match read_health_stats(&conn, &schema_inspection) {
        Ok(stats) => Some(stats),
        Err(error) if schema_inspection.messages_compatible => {
            sqlite.status = "fail".to_string();
            sqlite.summary = "authoritative message queries failed".to_string();
            sqlite.issues.push(error.to_string());
            None
        }
        Err(_) => None,
    };

    let sqlite_requires_quarantine = sqlite.status == "fail";
    let schema_requires_quarantine = schema_inspection.authoritative_incompatible;
    let wal_requires_quarantine = wal.status == "fail";
    let requires_quarantine = sqlite_requires_quarantine
        || schema_requires_quarantine
        || wal_requires_quarantine
        || fts_requires_quarantine
        || prompt_ref_requires_quarantine
        || prompt_hash_requires_quarantine;

    let mut report = MemoryHealthReport {
        status: "ok".to_string(),
        path: path.display().to_string(),
        initialized: true,
        sqlite,
        wal,
        schema,
        fts,
        prompt_references,
        prompt_hashes,
        compactions,
        titles,
        repair_lifecycle,
        stats,
        repairable_in_place: false,
        requires_quarantine,
        planned_repairs: Vec::new(),
    };
    populate_planned_repairs(&mut report);
    finalize_health_report(&mut report);
    Ok(report)
}

fn absent_health_report(
    path: &Path,
    wal: MemoryHealthCheck,
    repair_lifecycle: MemoryHealthCheck,
) -> MemoryHealthReport {
    let absent = || MemoryHealthCheck::ok("database is not initialized");
    MemoryHealthReport {
        status: "ok".to_string(),
        path: path.display().to_string(),
        initialized: false,
        sqlite: absent(),
        wal,
        schema: absent(),
        fts: absent(),
        prompt_references: absent(),
        prompt_hashes: absent(),
        compactions: absent(),
        titles: absent(),
        repair_lifecycle,
        stats: Some(MemoryHealthStats::default()),
        repairable_in_place: true,
        requires_quarantine: false,
        planned_repairs: vec!["initialize_database".to_string()],
    }
}

fn blocked_health_report(
    path: &Path,
    initialized: bool,
    error: String,
    wal: MemoryHealthCheck,
    repair_lifecycle: MemoryHealthCheck,
) -> MemoryHealthReport {
    let blocked = || {
        MemoryHealthCheck::fail(
            "check could not run because SQLite is unavailable",
            vec![error.clone()],
        )
    };
    let mut report = MemoryHealthReport {
        status: "fail".to_string(),
        path: path.display().to_string(),
        initialized,
        sqlite: MemoryHealthCheck::fail("SQLite database is unavailable", vec![error.clone()]),
        wal,
        schema: blocked(),
        fts: blocked(),
        prompt_references: blocked(),
        prompt_hashes: blocked(),
        compactions: blocked(),
        titles: blocked(),
        repair_lifecycle,
        stats: None,
        repairable_in_place: false,
        requires_quarantine: true,
        planned_repairs: vec!["quarantine_and_initialize_replacement".to_string()],
    };
    finalize_health_report(&mut report);
    report
}

fn finalize_health_report(report: &mut MemoryHealthReport) {
    let checks = [
        &report.sqlite,
        &report.wal,
        &report.schema,
        &report.fts,
        &report.prompt_references,
        &report.prompt_hashes,
        &report.compactions,
        &report.titles,
        &report.repair_lifecycle,
    ];
    report.status = if checks.iter().any(|check| check.status == "fail") {
        "fail".to_string()
    } else if checks.iter().any(|check| check.status == "warn") {
        "warn".to_string()
    } else {
        "ok".to_string()
    };
    report.repairable_in_place = !report.requires_quarantine
        && (!report.planned_repairs.is_empty() || report.status != "ok");
}

fn populate_planned_repairs(report: &mut MemoryHealthReport) {
    if report.requires_quarantine {
        report.planned_repairs = vec!["quarantine_and_initialize_replacement".to_string()];
        return;
    }
    let mut actions = Vec::new();
    if report.initialized
        && (report.wal.status != "ok"
            || report.wal.metrics.get("wal_frames").copied().unwrap_or(0) > 0)
    {
        actions.push("checkpoint_wal".to_string());
    } else if !report.initialized {
        actions.push("initialize_database".to_string());
    }

    if report.wal.status != "ok" {
        actions.push("configure_wal".to_string());
    }
    if report.schema.status != "ok" {
        actions.push("restore_schema_objects".to_string());
    }
    if report.fts.status != "ok" {
        actions.push("rebuild_fts_and_triggers".to_string());
    }
    if report.titles.metrics.get("orphaned").copied().unwrap_or(0) > 0 {
        actions.push("remove_orphaned_titles".to_string());
    }
    if report.titles.metrics.get("empty").copied().unwrap_or(0) > 0 {
        actions.push("remove_empty_titles".to_string());
    }
    if report
        .prompt_references
        .metrics
        .get("orphaned_sessions")
        .copied()
        .unwrap_or(0)
        > 0
    {
        actions.push("remove_orphaned_prompt_references".to_string());
    }
    if report
        .prompt_references
        .metrics
        .get("unreferenced_blobs")
        .copied()
        .unwrap_or(0)
        > 0
    {
        actions.push("remove_unreferenced_prompt_blobs".to_string());
    }
    if report.compactions.status != "ok" {
        actions.push("repair_compaction_projection".to_string());
    }
    if report.repair_lifecycle.status == "fail" {
        actions.insert(0, "resume_interrupted_repair".to_string());
    }
    actions.dedup();
    report.planned_repairs = actions;
}

fn run_repair_operation(
    path: &Path,
    actions: &[String],
    attempt: &RepairEvent,
) -> Result<QuarantineResult, MemoryError> {
    match attempt.mode {
        RepairMode::InPlace => perform_in_place_repair(path, actions),
        RepairMode::Quarantine => {
            let base = attempt
                .quarantine_path
                .as_deref()
                .map(PathBuf::from)
                .ok_or_else(|| {
                    MemoryError::Repair(
                        "interrupted quarantine repair has no recorded destination".to_string(),
                    )
                })?;
            quarantine_and_replace(path, &base, &attempt.attempt_id, attempt.checkpoint_source)
        }
    }
}

fn check_sqlite_integrity(conn: &Connection) -> MemoryHealthCheck {
    let result = (|| -> Result<Vec<String>, rusqlite::Error> {
        let mut stmt = conn.prepare("PRAGMA integrity_check")?;
        let mut rows = stmt.query([])?;
        let mut issues = Vec::new();
        while let Some(row) = rows.next()? {
            let message: String = row.get(0)?;
            if message != "ok" && issues.len() < 20 {
                issues.push(message);
            }
        }
        Ok(issues)
    })();
    match result {
        Ok(issues) if issues.is_empty() => MemoryHealthCheck::ok("SQLite integrity_check passed"),
        Ok(issues) => MemoryHealthCheck::fail("SQLite integrity_check failed", issues),
        Err(error) => MemoryHealthCheck::fail(
            "SQLite integrity_check could not complete",
            vec![error.to_string()],
        ),
    }
}

fn inspect_schema(conn: &Connection) -> Result<SchemaInspection, MemoryError> {
    let mut inspection = SchemaInspection::default();
    for (table, columns) in EXPECTED_TABLES {
        match schema_object_type(conn, table)? {
            None => inspection.missing.push(format!("missing table {table}")),
            Some(kind) if kind != "table" => inspection
                .incompatible
                .push(format!("{table} is {kind}, expected table")),
            Some(_) => {
                let actual = table_columns(conn, table)?;
                let absent: Vec<_> = columns
                    .iter()
                    .filter(|column| !actual.iter().any(|actual| actual == **column))
                    .copied()
                    .collect();
                if !absent.is_empty() {
                    inspection.authoritative_incompatible = true;
                    inspection.incompatible.push(format!(
                        "table {table} is missing column(s): {}",
                        absent.join(", ")
                    ));
                }
            }
        }
        if schema_object_type(conn, table)?
            .as_deref()
            .is_some_and(|kind| kind != "table")
        {
            inspection.authoritative_incompatible = true;
        }
    }
    for index in EXPECTED_INDEXES {
        if schema_object_type(conn, index)?.as_deref() != Some("index") {
            inspection.missing.push(format!("missing index {index}"));
        }
    }
    for (table, columns) in EXPECTED_COMPACTION_TABLES {
        match schema_object_type(conn, table)? {
            None => inspection.missing.push(format!("missing table {table}")),
            Some(kind) if kind != "table" => inspection
                .incompatible
                .push(format!("{table} is {kind}, expected table")),
            Some(_) => {
                let actual = table_columns(conn, table)?;
                let absent: Vec<_> = columns
                    .iter()
                    .filter(|column| !actual.iter().any(|actual| actual == **column))
                    .copied()
                    .collect();
                if !absent.is_empty() {
                    inspection.incompatible.push(format!(
                        "table {table} is missing column(s): {}",
                        absent.join(", ")
                    ));
                }
            }
        }
    }
    for index in EXPECTED_COMPACTION_INDEXES {
        if schema_object_type(conn, index)?.as_deref() != Some("index") {
            inspection.missing.push(format!("missing index {index}"));
        }
    }

    inspection.messages_compatible = table_has_columns(conn, "messages", EXPECTED_TABLES[0].1)?;
    inspection.titles_compatible = table_has_columns(conn, "session_titles", EXPECTED_TABLES[1].1)?;
    inspection.prompts_compatible =
        table_has_columns(conn, "system_prompts", EXPECTED_TABLES[2].1)?
            && table_has_columns(conn, "session_system_prompts", EXPECTED_TABLES[3].1)?;
    inspection.compactions_compatible = compaction::tables_compatible(conn)?;

    match schema_object_type(conn, "messages_fts")? {
        None => inspection
            .missing
            .push("missing virtual table messages_fts".to_string()),
        Some(kind) if kind != "table" => inspection
            .incompatible
            .push(format!("messages_fts is {kind}, expected virtual table")),
        Some(_) => {
            let sql: Option<String> = conn
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE name = 'messages_fts'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let normalized = sql.as_deref().map(normalize_sql).unwrap_or_default();
            inspection.fts_compatible = normalized.contains("usingfts5(")
                && normalized.contains("content='messages'")
                && normalized.contains("content_rowid='id'");
            if !inspection.fts_compatible {
                inspection.incompatible.push(
                    "messages_fts definition is not the expected external-content index"
                        .to_string(),
                );
            }
        }
    }

    for trigger in EXPECTED_TRIGGERS {
        let sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = ?",
                params![trigger],
                |row| row.get(0),
            )
            .optional()?;
        match sql {
            None => inspection.missing_triggers.push((*trigger).to_string()),
            Some(sql) if !trigger_definition_matches(trigger, &normalize_sql(&sql)) => {
                inspection.altered_triggers.push((*trigger).to_string())
            }
            Some(_) => {}
        }
    }
    Ok(inspection)
}

fn schema_health_check(schema: &SchemaInspection) -> MemoryHealthCheck {
    let mut issues = schema.missing.clone();
    issues.extend(schema.incompatible.clone());
    if issues.is_empty() {
        MemoryHealthCheck::ok("authoritative tables and indexes are present")
    } else {
        MemoryHealthCheck::fail("memory schema is incomplete or incompatible", issues)
    }
}

fn check_fts(conn: &Connection, schema: &SchemaInspection) -> (MemoryHealthCheck, bool) {
    let mut issues = Vec::new();
    if !schema.missing_triggers.is_empty() {
        issues.push(format!(
            "missing trigger(s): {}",
            schema.missing_triggers.join(", ")
        ));
    }
    if !schema.altered_triggers.is_empty() {
        issues.push(format!(
            "altered trigger(s): {}",
            schema.altered_triggers.join(", ")
        ));
    }
    if !schema.messages_compatible || !schema.fts_compatible {
        issues.push("FTS comparison is blocked by schema damage".to_string());
        return (
            MemoryHealthCheck::fail("FTS projection is unavailable", issues),
            false,
        );
    }

    match fts_matches_messages(conn) {
        Ok(true) => {}
        Ok(false) => {
            issues.push("FTS token index differs from authoritative message content".to_string())
        }
        Err(error) => {
            issues.push(format!("FTS projection check failed: {error}"));
        }
    }
    if issues.is_empty() {
        (
            MemoryHealthCheck::ok("FTS projection and maintenance triggers match messages"),
            false,
        )
    } else {
        (
            MemoryHealthCheck::fail("FTS projection requires an in-place rebuild", issues),
            false,
        )
    }
}

fn fts_matches_messages(conn: &Connection) -> Result<bool, MemoryError> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.memory_health_actual_vocab;
         DROP TABLE IF EXISTS temp.memory_health_expected_vocab;
         DROP TABLE IF EXISTS temp.memory_health_expected_fts;
         CREATE VIRTUAL TABLE temp.memory_health_expected_fts USING fts5(
             content,
             tokenize='unicode61 remove_diacritics 2'
         );
         INSERT INTO temp.memory_health_expected_fts(rowid, content)
             SELECT id, content FROM main.messages;
         CREATE VIRTUAL TABLE temp.memory_health_actual_vocab
             USING fts5vocab(main, messages_fts, 'instance');
         CREATE VIRTUAL TABLE temp.memory_health_expected_vocab
             USING fts5vocab(temp, memory_health_expected_fts, 'instance');",
    )?;
    let actual_has_extra = conn.query_row(
        "SELECT EXISTS(
             SELECT term, doc, col, offset FROM temp.memory_health_actual_vocab
             EXCEPT
             SELECT term, doc, col, offset FROM temp.memory_health_expected_vocab
         )",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0;
    let expected_has_extra = conn.query_row(
        "SELECT EXISTS(
             SELECT term, doc, col, offset FROM temp.memory_health_expected_vocab
             EXCEPT
             SELECT term, doc, col, offset FROM temp.memory_health_actual_vocab
         )",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0;
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.memory_health_actual_vocab;
         DROP TABLE IF EXISTS temp.memory_health_expected_vocab;
         DROP TABLE IF EXISTS temp.memory_health_expected_fts;",
    )?;
    Ok(!actual_has_extra && !expected_has_extra)
}

fn check_prompt_references(
    conn: &Connection,
    schema: &SchemaInspection,
) -> (MemoryHealthCheck, bool) {
    if !schema.prompts_compatible {
        return (
            MemoryHealthCheck::fail(
                "prompt references could not be checked",
                vec!["prompt tables are missing or incompatible".to_string()],
            ),
            false,
        );
    }
    let result = (|| -> Result<(u64, u64, u64), rusqlite::Error> {
        let dangling = conn.query_row(
            "SELECT COUNT(*)
             FROM session_system_prompts AS s
             LEFT JOIN system_prompts AS p ON p.hash = s.prompt_hash
             WHERE p.hash IS NULL",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let orphaned_sessions = if schema.messages_compatible {
            conn.query_row(
                "SELECT COUNT(*)
                 FROM session_system_prompts AS s
                 WHERE NOT EXISTS (
                     SELECT 1 FROM messages AS m WHERE m.session_id = s.session_id
                 )",
                [],
                |row| row.get::<_, i64>(0),
            )? as u64
        } else {
            0
        };
        let unreferenced_blobs = if schema.compactions_compatible {
            conn.query_row(
                "SELECT COUNT(*)
                 FROM system_prompts AS p
                 WHERE NOT EXISTS (
                     SELECT 1 FROM session_system_prompts AS s WHERE s.prompt_hash = p.hash
                 )
                   AND NOT EXISTS (
                     SELECT 1 FROM session_compactions AS c WHERE c.prompt_hash = p.hash
                 )",
                [],
                |row| row.get::<_, i64>(0),
            )? as u64
        } else {
            conn.query_row(
                "SELECT COUNT(*)
                 FROM system_prompts AS p
                 WHERE NOT EXISTS (
                     SELECT 1 FROM session_system_prompts AS s WHERE s.prompt_hash = p.hash
                 )",
                [],
                |row| row.get::<_, i64>(0),
            )? as u64
        };
        Ok((dangling, orphaned_sessions, unreferenced_blobs))
    })();
    match result {
        Ok((dangling, orphaned_sessions, unreferenced_blobs)) => {
            let mut check = if dangling > 0 {
                MemoryHealthCheck::fail(
                    "session prompt references are unsafe",
                    vec![format!(
                        "{dangling} session prompt reference(s) point to missing blobs"
                    )],
                )
            } else if orphaned_sessions > 0 || unreferenced_blobs > 0 {
                MemoryHealthCheck::warn(
                    "prompt reference projections contain orphaned rows",
                    vec![
                        format!("{orphaned_sessions} prompt reference(s) have no message session"),
                        format!("{unreferenced_blobs} prompt blob(s) are unreferenced"),
                    ],
                )
            } else {
                MemoryHealthCheck::ok("session prompt references are complete")
            };
            check
                .metrics
                .insert("dangling_hashes".to_string(), dangling);
            check
                .metrics
                .insert("orphaned_sessions".to_string(), orphaned_sessions);
            check
                .metrics
                .insert("unreferenced_blobs".to_string(), unreferenced_blobs);
            (check, dangling > 0)
        }
        Err(error) => (
            MemoryHealthCheck::fail("prompt reference check failed", vec![error.to_string()]),
            true,
        ),
    }
}

fn check_prompt_hashes(conn: &Connection, schema: &SchemaInspection) -> (MemoryHealthCheck, bool) {
    if !schema.prompts_compatible {
        return (
            MemoryHealthCheck::fail(
                "prompt hashes could not be checked",
                vec!["prompt tables are missing or incompatible".to_string()],
            ),
            false,
        );
    }
    let result = (|| -> Result<(u64, u64), rusqlite::Error> {
        let mut stmt = conn.prepare("SELECT hash, prompt FROM system_prompts ORDER BY hash")?;
        let mut rows = stmt.query([])?;
        let mut checked = 0_u64;
        let mut mismatched = 0_u64;
        while let Some(row) = rows.next()? {
            let hash: String = row.get(0)?;
            let prompt: String = row.get(1)?;
            checked += 1;
            if system_prompt_hash(&prompt) != hash {
                mismatched += 1;
            }
        }
        Ok((checked, mismatched))
    })();
    match result {
        Ok((checked, mismatched)) => {
            let mut check = if mismatched == 0 {
                MemoryHealthCheck::ok("all content-addressed prompt blobs match SHA-256")
            } else {
                MemoryHealthCheck::fail(
                    "content-addressed prompt verification failed",
                    vec![format!(
                        "{mismatched} of {checked} prompt blob(s) have the wrong SHA-256"
                    )],
                )
            };
            check.metrics.insert("checked".to_string(), checked);
            check.metrics.insert("mismatched".to_string(), mismatched);
            (check, mismatched > 0)
        }
        Err(error) => (
            MemoryHealthCheck::fail("prompt hash check failed", vec![error.to_string()]),
            true,
        ),
    }
}

fn check_compactions(conn: &Connection, schema: &SchemaInspection) -> MemoryHealthCheck {
    if !schema.compactions_compatible {
        return MemoryHealthCheck::fail(
            "durable compaction projection is unavailable",
            vec!["compaction tables are missing or incompatible".to_string()],
        );
    }
    match compaction::inspect_projection(conn) {
        Ok(inspection) => {
            let mut issues = Vec::new();
            if inspection.invalid_records > 0 {
                issues.push(format!(
                    "{} compaction record(s) failed lifecycle, source, or content-address verification",
                    inspection.invalid_records
                ));
            }
            if inspection.interrupted > 0 {
                issues.push(format!(
                    "{} compaction attempt(s) started without completing",
                    inspection.interrupted
                ));
            }
            if inspection.orphaned_sessions > 0 {
                issues.push(format!(
                    "{} compaction record(s) have no raw session rows",
                    inspection.orphaned_sessions
                ));
            }
            if inspection.unreferenced_summaries > 0 {
                issues.push(format!(
                    "{} content-addressed compaction summary blob(s) are unreferenced",
                    inspection.unreferenced_summaries
                ));
            }
            let mut check = if inspection.invalid_records > 0 {
                MemoryHealthCheck::fail(
                    "durable compaction projection contains invalid summaries",
                    issues,
                )
            } else if issues.is_empty() {
                MemoryHealthCheck::ok("durable compaction lifecycle and summaries are valid")
            } else {
                MemoryHealthCheck::warn(
                    "durable compaction projection needs in-place recovery",
                    issues,
                )
            };
            check
                .metrics
                .insert("completed".to_string(), inspection.completed);
            check
                .metrics
                .insert("failed".to_string(), inspection.failed);
            check
                .metrics
                .insert("interrupted".to_string(), inspection.interrupted);
            check.metrics.insert(
                "invalid_records".to_string(),
                inspection.invalid_records,
            );
            check.metrics.insert(
                "orphaned_sessions".to_string(),
                inspection.orphaned_sessions,
            );
            check.metrics.insert(
                "unreferenced_summaries".to_string(),
                inspection.unreferenced_summaries,
            );
            check
        }
        Err(error) => MemoryHealthCheck::fail(
            "durable compaction projection check failed",
            vec![error.to_string()],
        ),
    }
}

fn check_titles(conn: &Connection, schema: &SchemaInspection) -> MemoryHealthCheck {
    if !schema.titles_compatible || !schema.messages_compatible {
        return MemoryHealthCheck::fail(
            "session titles could not be checked",
            vec!["title or message table is missing or incompatible".to_string()],
        );
    }
    let result = (|| -> Result<(u64, u64), rusqlite::Error> {
        let orphaned = conn.query_row(
            "SELECT COUNT(*)
             FROM session_titles AS t
             WHERE NOT EXISTS (
                 SELECT 1 FROM messages AS m WHERE m.session_id = t.session_id
             )",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let empty = conn.query_row(
            "SELECT COUNT(*) FROM session_titles WHERE trim(title) = ''",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        Ok((orphaned, empty))
    })();
    match result {
        Ok((orphaned, empty)) => {
            let mut issues = Vec::new();
            if orphaned > 0 {
                issues.push(format!(
                    "{orphaned} title(s) do not belong to a session with messages"
                ));
            }
            if empty > 0 {
                issues.push(format!("{empty} title(s) are empty"));
            }
            let mut check = if issues.is_empty() {
                MemoryHealthCheck::ok("session titles belong to authoritative message sessions")
            } else {
                MemoryHealthCheck::warn("session title invariants need attention", issues)
            };
            check.metrics.insert("orphaned".to_string(), orphaned);
            check.metrics.insert("empty".to_string(), empty);
            check
        }
        Err(error) => {
            MemoryHealthCheck::fail("session title check failed", vec![error.to_string()])
        }
    }
}

fn read_health_stats(
    conn: &Connection,
    schema: &SchemaInspection,
) -> Result<MemoryHealthStats, MemoryError> {
    if !schema.messages_compatible {
        return Err(MemoryError::Integrity(
            "messages schema is unavailable".to_string(),
        ));
    }
    const DAY_MS: i64 = 86_400_000;
    let now = current_ts_ms();
    let total_messages = conn.query_row("SELECT COUNT(*) FROM messages", [], |row| {
        row.get::<_, i64>(0)
    })? as u64;
    let total_sessions = conn.query_row(
        "SELECT COUNT(DISTINCT session_id) FROM messages",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64;
    let titled_sessions = if schema.titles_compatible {
        conn.query_row("SELECT COUNT(*) FROM session_titles", [], |row| {
            row.get::<_, i64>(0)
        })? as u64
    } else {
        0
    };
    let count_since = |cutoff: i64| {
        conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE ts_ms >= ?",
            params![cutoff],
            |row| row.get::<_, i64>(0).map(|value| value as u64),
        )
    };
    let (oldest_ts_ms, newest_ts_ms) =
        conn.query_row("SELECT MIN(ts_ms), MAX(ts_ms) FROM messages", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    Ok(MemoryHealthStats {
        total_messages,
        total_sessions,
        titled_sessions,
        messages_last_1d: count_since(now.saturating_sub(DAY_MS))?,
        messages_last_7d: count_since(now.saturating_sub(7 * DAY_MS))?,
        messages_last_30d: count_since(now.saturating_sub(30 * DAY_MS))?,
        oldest_ts_ms,
        newest_ts_ms,
    })
}

fn inspect_wal_files(path: &Path, display_path: &Path) -> MemoryHealthCheck {
    let wal = wal_path(path);
    let shm = shm_path(path);
    let display_wal = wal_path(display_path);
    let display_shm = shm_path(display_path);
    let mut check =
        MemoryHealthCheck::ok("WAL sidecars are absent or pass format and checksum validation");
    let mut issues = Vec::new();

    match inspect_wal(&wal, path) {
        Ok(Some(validation)) => {
            check
                .metrics
                .insert("wal_bytes".to_string(), validation.bytes);
            check
                .metrics
                .insert("wal_frames".to_string(), validation.frames);
            check.metrics.insert(
                "wal_physical_frames".to_string(),
                validation.physical_frames,
            );
            check
                .metrics
                .insert("wal_commits".to_string(), validation.commits);
            check
                .metrics
                .insert("wal_page_size".to_string(), validation.page_size);
            check.metrics.insert(
                "wal_stale_tail_bytes".to_string(),
                validation.stale_tail_bytes,
            );
            if let Some(issue) = validation.wal_index_issue {
                issues.push(issue);
            }
        }
        Ok(None) => {
            check.metrics.insert("wal_bytes".to_string(), 0);
            check.metrics.insert("wal_frames".to_string(), 0);
            check.metrics.insert("wal_commits".to_string(), 0);
        }
        Err(error) => issues.push(
            error
                .replace(
                    &wal.display().to_string(),
                    &display_wal.display().to_string(),
                )
                .replace(
                    &path.display().to_string(),
                    &display_path.display().to_string(),
                ),
        ),
    }
    match fs::symlink_metadata(&shm) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            issues.push(format!("{} is not a regular file", display_shm.display()));
        }
        Ok(metadata) => {
            check
                .metrics
                .insert("shm_bytes".to_string(), metadata.len());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            check.metrics.insert("shm_bytes".to_string(), 0);
        }
        Err(error) => issues.push(format!("cannot inspect {}: {error}", display_shm.display())),
    }
    if !path.exists() && (wal.exists() || shm.exists()) {
        issues.push("SQLite sidecar exists without memory.db".to_string());
    }
    if issues.is_empty() {
        check
    } else if issues
        .iter()
        .all(|issue| issue.contains("WAL index") || issue.contains("SHM"))
    {
        MemoryHealthCheck {
            status: "warn".to_string(),
            summary: "WAL data is valid but its rebuildable index needs repair".to_string(),
            issues,
            metrics: check.metrics,
        }
    } else {
        MemoryHealthCheck {
            status: "fail".to_string(),
            summary: "WAL sidecar validation failed".to_string(),
            issues,
            metrics: check.metrics,
        }
    }
}

fn inspect_wal(path: &Path, database_path: &Path) -> Result<Option<WalValidation>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let size = metadata.len();
    if size == 0 {
        return Ok(Some(WalValidation {
            bytes: 0,
            frames: 0,
            physical_frames: 0,
            commits: 0,
            page_size: 0,
            stale_tail_bytes: 0,
            wal_index_issue: None,
        }));
    }
    if size < 32 {
        return Err(format!("{} is shorter than a WAL header", path.display()));
    }
    let mut file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut header = [0_u8; 32];
    file.read_exact(&mut header)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let magic = u32::from_be_bytes(header[0..4].try_into().expect("four bytes"));
    if !matches!(magic, 0x377f_0682 | 0x377f_0683) {
        return Err(format!("{} has an invalid WAL magic", path.display()));
    }
    let version = u32::from_be_bytes(header[4..8].try_into().expect("four bytes"));
    if version != WAL_FORMAT_VERSION {
        return Err(format!(
            "{} has unsupported WAL format version {version}",
            path.display()
        ));
    }
    let encoded_page_size =
        u32::from_be_bytes(header[8..12].try_into().expect("four bytes")) as u64;
    let page_size = if encoded_page_size == 1 {
        65_536
    } else {
        encoded_page_size
    };
    if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
        return Err(format!("{} has an invalid WAL page size", path.display()));
    }
    if let Some(database_page_size) = read_database_page_size(database_path)? {
        if database_page_size != page_size {
            return Err(format!(
                "{} page size {page_size} does not match database page size {database_page_size}",
                path.display()
            ));
        }
    }

    // The low magic bit selects the byte order used for checksum words.
    // Stored checksum fields themselves are always big-endian.
    let checksum_big_endian = magic & 1 != 0;
    let header_checksum = wal_checksum(&header[..24], checksum_big_endian, [0, 0]);
    let stored_header_checksum = [
        u32::from_be_bytes(header[24..28].try_into().expect("four bytes")),
        u32::from_be_bytes(header[28..32].try_into().expect("four bytes")),
    ];
    if header_checksum != stored_header_checksum {
        return Err(format!(
            "{} has an invalid WAL header checksum",
            path.display()
        ));
    }

    let frame_size = page_size + 24;
    let payload = size - 32;
    let physical_frames = payload / frame_size;
    let trailing_bytes = payload % frame_size;
    let salt: [u8; 8] = header[16..24].try_into().expect("eight bytes");
    let (wal_index, wal_index_issue) =
        match read_wal_index_hint(&shm_path(database_path), magic, page_size, &salt) {
            Ok(hint) => (hint, None),
            Err(error) => (None, Some(error)),
        };
    if let Some(hint) = wal_index {
        if hint.mx_frame > physical_frames {
            return Err(format!(
                "{} contains {physical_frames} complete frame(s), but the WAL index requires {}",
                path.display(),
                hint.mx_frame
            ));
        }
    }

    let mut rolling_checksum = header_checksum;
    let mut frame = vec![0_u8; frame_size as usize];
    let mut commits = 0_u64;
    let mut last_commit = 0_u64;
    let scan_frames = wal_index
        .map(|hint| hint.mx_frame)
        .unwrap_or(physical_frames);
    for frame_index in 0..scan_frames {
        file.read_exact(&mut frame).map_err(|error| {
            format!("read {} frame {}: {error}", path.display(), frame_index + 1)
        })?;
        if wal_index.is_none() && frame[8..16] != salt {
            break;
        }
        let frame_number = frame_index + 1;
        let (commit_size, checksum) = validate_wal_frame(
            &frame,
            frame_number,
            &salt,
            checksum_big_endian,
            rolling_checksum,
            path,
        )?;
        rolling_checksum = checksum;
        if commit_size != 0 {
            commits += 1;
            last_commit = frame_number;
        }
    }

    let logical_frames = if let Some(hint) = wal_index {
        if hint.mx_frame > 0 {
            if last_commit != hint.mx_frame {
                return Err(format!(
                    "{} WAL index points to frame {}, but that frame is not a commit",
                    path.display(),
                    hint.mx_frame
                ));
            }
            if rolling_checksum != hint.frame_checksum {
                return Err(format!(
                    "{} WAL index checksum does not match logical frame {}",
                    path.display(),
                    hint.mx_frame
                ));
            }
        }
        hint.mx_frame
    } else {
        last_commit
    };
    let logical_bytes = 32 + logical_frames * frame_size;
    let stale_tail_bytes = size.saturating_sub(logical_bytes);
    Ok(Some(WalValidation {
        bytes: size,
        frames: logical_frames,
        physical_frames,
        commits,
        page_size,
        stale_tail_bytes: stale_tail_bytes.max(trailing_bytes),
        wal_index_issue,
    }))
}

fn validate_wal_frame(
    frame: &[u8],
    frame_number: u64,
    salt: &[u8; 8],
    checksum_big_endian: bool,
    seed: [u32; 2],
    path: &Path,
) -> Result<(u32, [u32; 2]), String> {
    let page_number = u32::from_be_bytes(frame[0..4].try_into().expect("four bytes"));
    if page_number == 0 || page_number > SQLITE_MAX_PAGE_NUMBER {
        return Err(format!(
            "{} frame {frame_number} has invalid page number {page_number}",
            path.display()
        ));
    }
    let commit_size = u32::from_be_bytes(frame[4..8].try_into().expect("four bytes"));
    if commit_size > SQLITE_MAX_PAGE_NUMBER {
        return Err(format!(
            "{} frame {frame_number} has invalid commit size {commit_size}",
            path.display()
        ));
    }
    if &frame[8..16] != salt {
        return Err(format!(
            "{} frame {frame_number} has salts that do not match the WAL header",
            path.display()
        ));
    }
    // SQLite chains each frame over its page/commit words and page bytes;
    // the salt and stored checksum fields are deliberately excluded.
    let checksum = wal_checksum(&frame[..8], checksum_big_endian, seed);
    let checksum = wal_checksum(&frame[24..], checksum_big_endian, checksum);
    let stored_checksum = [
        u32::from_be_bytes(frame[16..20].try_into().expect("four bytes")),
        u32::from_be_bytes(frame[20..24].try_into().expect("four bytes")),
    ];
    if checksum != stored_checksum {
        return Err(format!(
            "{} frame {frame_number} has an invalid rolling checksum",
            path.display()
        ));
    }
    Ok((commit_size, checksum))
}

fn read_wal_index_hint(
    path: &Path,
    wal_magic: u32,
    wal_page_size: u64,
    wal_salt: &[u8; 8],
) -> Result<Option<WalIndexHint>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot inspect WAL index {}: {error}",
                path.display()
            ))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "WAL index {} is not a regular file",
            path.display()
        ));
    }
    if metadata.len() < 96 {
        return Err(format!(
            "WAL index {} is shorter than its headers",
            path.display()
        ));
    }
    let mut headers = [0_u8; 96];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut headers))
        .map_err(|error| format!("read WAL index {}: {error}", path.display()))?;
    if headers[..48] != headers[48..96] {
        return Err(format!(
            "WAL index {} header copies disagree",
            path.display()
        ));
    }
    let header = &headers[..48];
    let version = native_u32(&header[0..4]);
    if version != WAL_FORMAT_VERSION {
        return Err(format!(
            "WAL index {} has unsupported version {version}",
            path.display()
        ));
    }
    if header[12] != 1 {
        return Err(format!("WAL index {} is not initialized", path.display()));
    }
    if header[13] != (wal_magic & 1) as u8 {
        return Err(format!(
            "WAL index {} checksum byte order disagrees with WAL header",
            path.display()
        ));
    }
    let encoded_page_size = native_u16(&header[14..16]) as u64;
    let page_size = if encoded_page_size == 1 {
        65_536
    } else {
        encoded_page_size
    };
    if page_size != wal_page_size {
        return Err(format!(
            "WAL index {} page size does not match WAL header",
            path.display()
        ));
    }
    if &header[32..40] != wal_salt {
        return Err(format!(
            "WAL index {} salts do not match WAL header",
            path.display()
        ));
    }
    let expected = wal_checksum(&header[..40], cfg!(target_endian = "big"), [0, 0]);
    let stored = [native_u32(&header[40..44]), native_u32(&header[44..48])];
    if expected != stored {
        return Err(format!(
            "WAL index {} header checksum is invalid",
            path.display()
        ));
    }
    Ok(Some(WalIndexHint {
        mx_frame: native_u32(&header[16..20]) as u64,
        frame_checksum: [native_u32(&header[24..28]), native_u32(&header[28..32])],
    }))
}

fn native_u32(bytes: &[u8]) -> u32 {
    u32::from_ne_bytes(bytes.try_into().expect("four bytes"))
}

fn native_u16(bytes: &[u8]) -> u16 {
    u16::from_ne_bytes(bytes.try_into().expect("two bytes"))
}

fn wal_checksum(bytes: &[u8], big_endian_words: bool, seed: [u32; 2]) -> [u32; 2] {
    let (word_pairs, remainder) = bytes.as_chunks::<8>();
    debug_assert!(remainder.is_empty());
    let mut first = seed[0];
    let mut second = seed[1];
    for words in word_pairs {
        let left = if big_endian_words {
            u32::from_be_bytes(words[0..4].try_into().expect("four bytes"))
        } else {
            u32::from_le_bytes(words[0..4].try_into().expect("four bytes"))
        };
        let right = if big_endian_words {
            u32::from_be_bytes(words[4..8].try_into().expect("four bytes"))
        } else {
            u32::from_le_bytes(words[4..8].try_into().expect("four bytes"))
        };
        first = first.wrapping_add(left).wrapping_add(second);
        second = second.wrapping_add(right).wrapping_add(first);
    }
    [first, second]
}

fn read_database_page_size(path: &Path) -> Result<Option<u64>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    };
    if metadata.len() < 100 {
        return Ok(None);
    }
    let mut header = [0_u8; 100];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if &header[..16] != b"SQLite format 3\0" {
        return Ok(None);
    }
    let encoded = u16::from_be_bytes(header[16..18].try_into().expect("two bytes")) as u64;
    Ok(Some(if encoded == 1 { 65_536 } else { encoded }))
}

fn repair_lifecycle_check(path: &Path, snapshot: &RepairLogSnapshot) -> MemoryHealthCheck {
    if snapshot.malformed_lines > 0 {
        return MemoryHealthCheck::fail(
            "repair lifecycle log is malformed",
            vec![format!(
                "{} malformed repair log line(s) in {}",
                snapshot.malformed_lines,
                repair_log_path(path).display()
            )],
        );
    }
    if !snapshot.incomplete.is_empty() {
        return MemoryHealthCheck::fail(
            "an interrupted memory repair is recorded",
            vec![format!(
                "{} repair attempt(s) started without a terminal result; re-run explicit repair",
                snapshot.incomplete.len()
            )],
        )
        .with_metric("interrupted_attempts", snapshot.incomplete.len() as u64);
    }
    if snapshot
        .last_applied
        .as_ref()
        .is_some_and(|event| event.phase == RepairPhase::Failed)
    {
        return MemoryHealthCheck::fail(
            "the last memory repair failed",
            vec![snapshot
                .last_applied
                .as_ref()
                .and_then(|event| event.error.clone())
                .unwrap_or_else(|| "repair failed without an error detail".to_string())],
        );
    }
    MemoryHealthCheck::ok("no interrupted or failed repair is pending")
}

fn perform_in_place_repair(
    path: &Path,
    actions: &[String],
) -> Result<QuarantineResult, MemoryError> {
    inspect_wal(&wal_path(path), path)
        .map_err(|error| MemoryError::Integrity(format!("refusing WAL checkpoint: {error}")))?;
    ensure_private_database_file(path)?;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(CONNECTION_PRAGMAS)?;
    checkpoint_wal(&conn)?;

    let mut conn = conn;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if compaction::tables_compatible(&tx)? {
        tx.execute(
            "UPDATE session_compactions
             SET state = 'failed',
                 finished_ts_ms = ?,
                 failure_kind = 'interrupted_during_repair'
             WHERE state = 'started'",
            params![current_ts_ms()],
        )?;
    } else {
        tx.execute_batch(
            "DROP INDEX IF EXISTS session_compactions_one_started;
             DROP INDEX IF EXISTS session_compactions_latest;
             DROP TABLE IF EXISTS session_compactions;
             DROP TABLE IF EXISTS compaction_summaries;",
        )?;
    }
    tx.execute_batch(BASE_SCHEMA)?;
    tx.execute_batch(COMPACTION_SCHEMA)?;
    validate_prompt_integrity(&tx)?;

    tx.execute(
        "DELETE FROM session_titles
         WHERE trim(title) = ''
            OR NOT EXISTS (
                SELECT 1 FROM messages WHERE messages.session_id = session_titles.session_id
            )",
        [],
    )?;
    tx.execute(
        "DELETE FROM session_system_prompts
         WHERE NOT EXISTS (
             SELECT 1 FROM messages
             WHERE messages.session_id = session_system_prompts.session_id
         )",
        [],
    )?;
    compaction::repair_projection(&tx)?;
    tx.execute(
        "DELETE FROM system_prompts
         WHERE NOT EXISTS (
             SELECT 1 FROM session_system_prompts
             WHERE session_system_prompts.prompt_hash = system_prompts.hash
         )
           AND NOT EXISTS (
             SELECT 1 FROM session_compactions
             WHERE session_compactions.prompt_hash = system_prompts.hash
         )",
        [],
    )?;

    if actions
        .iter()
        .any(|action| action == "rebuild_fts_and_triggers")
        || schema_object_type(&tx, "messages_fts")?.is_none()
    {
        tx.execute_batch(
            "DROP TRIGGER IF EXISTS messages_ai;
             DROP TRIGGER IF EXISTS messages_ad;
             DROP TRIGGER IF EXISTS messages_au;
             DROP TABLE IF EXISTS messages_fts;",
        )?;
        tx.execute_batch(FTS_SCHEMA)?;
        tx.execute(
            "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
            [],
        )?;
    } else {
        tx.execute_batch(FTS_SCHEMA)?;
    }
    tx.commit()?;
    checkpoint_wal(&conn)?;
    harden_sqlite_files(path)?;
    Ok(QuarantineResult::default())
}

fn validate_prompt_integrity(conn: &Connection) -> Result<(), MemoryError> {
    let dangling = conn.query_row(
        "SELECT COUNT(*)
         FROM session_system_prompts AS s
         LEFT JOIN system_prompts AS p ON p.hash = s.prompt_hash
         WHERE p.hash IS NULL",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64;
    if dangling > 0 {
        return Err(MemoryError::Integrity(format!(
            "{dangling} session prompt reference(s) point to missing blobs"
        )));
    }
    let mut stmt = conn.prepare("SELECT hash, prompt FROM system_prompts")?;
    let mut rows = stmt.query([])?;
    let mut mismatched = 0_u64;
    while let Some(row) = rows.next()? {
        let hash: String = row.get(0)?;
        let prompt: String = row.get(1)?;
        if system_prompt_hash(&prompt) != hash {
            mismatched += 1;
        }
    }
    if mismatched > 0 {
        return Err(MemoryError::Integrity(format!(
            "{mismatched} system prompt blob(s) failed SHA-256 verification"
        )));
    }
    Ok(())
}

fn quarantine_and_replace(
    path: &Path,
    quarantine: &Path,
    attempt_id: &str,
    checkpoint_source: bool,
) -> Result<QuarantineResult, MemoryError> {
    let mut files = existing_quarantine_files(quarantine);
    let mut warnings = Vec::new();

    if path.exists() && quarantine.exists() {
        let source_hash = file_sha256_optional(quarantine)?;
        let live_marker = match read_replacement_marker(path) {
            Ok(marker) => marker,
            Err(error) => {
                warnings.push(format!(
                    "live database could not be validated as this attempt's replacement: {error}"
                ));
                None
            }
        };
        if let Some(marker) = live_marker {
            if replacement_marker_matches(&marker, attempt_id, quarantine, source_hash.as_deref())
                && diagnose_locked(path, false)?.status == "ok"
            {
                return Ok(QuarantineResult {
                    base: Some(quarantine.to_path_buf()),
                    files,
                    recovered: marker.recovered,
                    recovery_warning: marker.recovery_warning,
                });
            }
        }
        let unbound = suffix_path(
            quarantine,
            &format!(".unbound-live-{}", uuid::Uuid::new_v4().simple()),
        );
        move_family(path, &unbound, &mut files)?;
        warnings.push(format!(
            "preserved unbound database found at the live path as {}",
            unbound.display()
        ));
    }

    if path.exists() && !quarantine.exists() && checkpoint_source {
        ensure_wal_fully_checkpointed(path)?;
    }

    move_if_present(path, quarantine, &mut files)?;
    move_if_present(&wal_path(path), &wal_path(quarantine), &mut files)?;
    move_if_present(&shm_path(path), &shm_path(quarantine), &mut files)?;
    sync_parent(path)?;

    let source_hash = file_sha256_optional(quarantine)?;
    let replacement = staged_replacement_path(path, attempt_id);
    if replacement.exists() {
        let staged_marker = match read_replacement_marker(&replacement) {
            Ok(marker) => marker,
            Err(error) => {
                warnings.push(format!(
                    "staged database could not be validated as this attempt's replacement: {error}"
                ));
                None
            }
        };
        let staged_matches = staged_marker.as_ref().is_some_and(|marker| {
            replacement_marker_matches(marker, attempt_id, quarantine, source_hash.as_deref())
        });
        if !staged_matches {
            let invalid = suffix_path(
                quarantine,
                &format!(".unbound-staged-{}", uuid::Uuid::new_v4().simple()),
            );
            move_family(&replacement, &invalid, &mut files)?;
            warnings.push(format!(
                "preserved staged replacement not bound to repair attempt as {}",
                invalid.display()
            ));
        }
    }

    let marker = if replacement.exists() {
        read_replacement_marker(&replacement)?.ok_or_else(|| {
            MemoryError::Integrity(format!(
                "staged replacement {} has no repair marker",
                replacement.display()
            ))
        })?
    } else {
        build_staged_replacement(quarantine, &replacement, attempt_id, source_hash.clone())?
    };
    if !replacement_marker_matches(&marker, attempt_id, quarantine, source_hash.as_deref()) {
        return Err(MemoryError::Integrity(format!(
            "staged replacement {} is not bound to repair attempt {attempt_id}",
            replacement.display()
        )));
    }

    remove_if_present(&wal_path(&replacement))?;
    remove_if_present(&shm_path(&replacement))?;
    fs::rename(&replacement, path)?;
    crate::storage::set_private_file(path)?;
    harden_sqlite_files(path)?;
    sync_parent(path)?;

    let installed = read_replacement_marker(path)?.ok_or_else(|| {
        MemoryError::Integrity(format!(
            "installed replacement {} lost its repair marker",
            path.display()
        ))
    })?;
    if !replacement_marker_matches(&installed, attempt_id, quarantine, source_hash.as_deref()) {
        return Err(MemoryError::Integrity(format!(
            "installed replacement {} is not bound to repair attempt {attempt_id}",
            path.display()
        )));
    }
    if let Some(warning) = installed.recovery_warning.clone() {
        warnings.push(warning);
    }

    files = existing_quarantine_files(quarantine)
        .into_iter()
        .chain(files)
        .collect();
    files.sort();
    files.dedup();
    Ok(QuarantineResult {
        base: Some(quarantine.to_path_buf()),
        files,
        recovered: installed.recovered,
        recovery_warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
    })
}

fn build_staged_replacement(
    quarantine: &Path,
    replacement: &Path,
    attempt_id: &str,
    source_hash: Option<String>,
) -> Result<ReplacementMarker, MemoryError> {
    remove_replacement_scratch(replacement)?;
    ensure_private_database_file(replacement)?;
    let mut target = Connection::open_with_flags(replacement, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    initialize_connection(&target)?;
    prepare_target_for_recovery(&target)?;

    let mut marker = ReplacementMarker {
        attempt_id: attempt_id.to_string(),
        quarantine_path: quarantine.display().to_string(),
        source_main_sha256: source_hash,
        complete: false,
        salvage_succeeded: false,
        recovered: RecoveredRecords::default(),
        recovery_warning: None,
    };
    write_replacement_marker(&target, &marker)?;

    if quarantine.exists() {
        let result = recover_from_standalone_main(quarantine, attempt_id, &mut target)?;
        marker.salvage_succeeded = result.salvage_succeeded;
        marker.recovered = result.recovered;
        marker.recovery_warning = (!result.warnings.is_empty()).then(|| result.warnings.join("; "));
    } else {
        rebuild_target_projections(&mut target)?;
        marker.recovery_warning =
            Some("quarantine contains no main database; replacement is empty".to_string());
    }
    write_replacement_marker(&target, &marker)?;
    checkpoint_wal(&target)?;
    marker.complete = true;
    write_replacement_marker(&target, &marker)?;
    checkpoint_wal(&target)?;
    drop(target);
    harden_sqlite_files(replacement)?;
    let durable = read_replacement_marker(replacement)?.ok_or_else(|| {
        MemoryError::Integrity(format!(
            "staged replacement {} has no durable repair marker",
            replacement.display()
        ))
    })?;
    if durable != marker {
        return Err(MemoryError::Integrity(format!(
            "staged replacement {} marker or recovered counts were not checkpointed",
            replacement.display()
        )));
    }
    Ok(durable)
}

fn recover_from_standalone_main(
    quarantine: &Path,
    attempt_id: &str,
    target: &mut Connection,
) -> Result<StandaloneRecoveryResult, MemoryError> {
    let standalone_path = suffix_path(quarantine, &format!(".main-only-{attempt_id}"));
    remove_replacement_scratch(&standalone_path)?;
    maybe_fail_standalone_recovery(StandaloneRecoveryStage::Copy)?;
    copy_snapshot_file(quarantine, &standalone_path)?;
    let standalone = StandaloneMainCopy {
        path: standalone_path,
    };

    maybe_fail_standalone_recovery(StandaloneRecoveryStage::Open)?;
    let source =
        match Connection::open_with_flags(&standalone.path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(source) => source,
            Err(error) => {
                return conclusive_standalone_failure(
                    MemoryError::Sqlite(error),
                    "standalone main database could not be opened",
                    target,
                );
            }
        };
    maybe_fail_standalone_recovery(StandaloneRecoveryStage::Configure)?;
    source.busy_timeout(Duration::from_secs(5))?;
    maybe_fail_standalone_recovery(StandaloneRecoveryStage::SchemaRead)?;
    let messages_compatible = match table_has_columns(&source, "messages", EXPECTED_TABLES[0].1) {
        Ok(compatible) => compatible,
        Err(error) => {
            return conclusive_standalone_failure(
                error,
                "standalone main database schema could not be read",
                target,
            );
        }
    };
    if !messages_compatible {
        rebuild_target_projections(target)?;
        return Ok(unreadable_standalone(
            "standalone main database has no compatible messages table".to_string(),
        ));
    }

    let mut warnings = Vec::new();
    let integrity = check_sqlite_integrity(&source);
    if integrity.status != "ok" {
        warnings.push(format!(
            "global integrity_check reported secondary damage: {}",
            integrity.issues.join("; ")
        ));
    }
    let (mut recovered, sessions) = recover_authoritative_messages(&source, target)?;
    rebuild_target_projections(target)?;

    match recover_titles_projection(&source, target, &sessions) {
        Ok(count) => recovered.titles = count,
        Err(error) => warnings.push(format!("session titles were not recovered: {error}")),
    }
    match recover_prompt_projection(&source, target, &sessions) {
        Ok((count, skipped)) => {
            recovered.prompt_references = count;
            recovered.skipped_prompt_references = skipped;
        }
        Err(error) => warnings.push(format!("session prompts were not recovered: {error}")),
    }
    match compaction::recover_projection(&source, target, &sessions) {
        Ok((count, skipped)) => {
            recovered.compactions = count;
            recovered.skipped_compactions = skipped;
        }
        Err(error) => warnings.push(format!("session compactions were not recovered: {error}")),
    }

    Ok(StandaloneRecoveryResult {
        recovered,
        salvage_succeeded: true,
        warnings,
    })
}

fn unreadable_standalone(error: String) -> StandaloneRecoveryResult {
    StandaloneRecoveryResult {
        recovered: RecoveredRecords::default(),
        salvage_succeeded: false,
        warnings: vec![format!(
            "standalone main-database recovery failed; replacement is empty: {error}"
        )],
    }
}

fn conclusive_standalone_failure(
    error: MemoryError,
    context: &str,
    target: &mut Connection,
) -> Result<StandaloneRecoveryResult, MemoryError> {
    if !error.is_integrity_failure() {
        return Err(error);
    }
    rebuild_target_projections(target)?;
    Ok(unreadable_standalone(format!("{context}: {error}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StandaloneRecoveryStage {
    Copy,
    Open,
    Configure,
    SchemaRead,
}

#[cfg(not(test))]
fn maybe_fail_standalone_recovery(_stage: StandaloneRecoveryStage) -> Result<(), MemoryError> {
    Ok(())
}

#[cfg(test)]
fn maybe_fail_standalone_recovery(stage: StandaloneRecoveryStage) -> Result<(), MemoryError> {
    let should_fail = STANDALONE_RECOVERY_FAILPOINT.with(|value| value.get() == Some(stage));
    if !should_fail {
        return Ok(());
    }
    let (kind, message) = match stage {
        StandaloneRecoveryStage::Copy => (
            std::io::ErrorKind::Other,
            "No space left on device while copying standalone database",
        ),
        StandaloneRecoveryStage::Open => (
            std::io::ErrorKind::Other,
            "Too many open files while opening standalone database",
        ),
        StandaloneRecoveryStage::Configure => (
            std::io::ErrorKind::Interrupted,
            "interrupted while configuring standalone database",
        ),
        StandaloneRecoveryStage::SchemaRead => (
            std::io::ErrorKind::WouldBlock,
            "temporary I/O failure while reading standalone schema",
        ),
    };
    Err(MemoryError::Io(std::io::Error::new(kind, message)))
}

fn write_replacement_marker(
    conn: &Connection,
    marker: &ReplacementMarker,
) -> Result<(), MemoryError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_repair_install (
             singleton                 INTEGER PRIMARY KEY CHECK(singleton = 1),
             attempt_id                TEXT NOT NULL,
             quarantine_path           TEXT NOT NULL,
             source_main_sha256         TEXT,
             complete                  INTEGER NOT NULL,
             salvage_succeeded         INTEGER NOT NULL,
             recovered_messages        INTEGER NOT NULL,
             recovered_titles          INTEGER NOT NULL,
             recovered_prompt_refs     INTEGER NOT NULL,
             skipped_prompt_refs       INTEGER NOT NULL,
             recovered_compactions     INTEGER NOT NULL DEFAULT 0,
             skipped_compactions       INTEGER NOT NULL DEFAULT 0,
             recovery_warning          TEXT
         );",
    )?;
    let marker_columns = table_columns(conn, REPLACEMENT_MARKER_TABLE)?;
    if !marker_columns
        .iter()
        .any(|column| column == "recovered_compactions")
    {
        conn.execute_batch(
            "ALTER TABLE memory_repair_install
             ADD COLUMN recovered_compactions INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    let marker_columns = table_columns(conn, REPLACEMENT_MARKER_TABLE)?;
    if !marker_columns
        .iter()
        .any(|column| column == "skipped_compactions")
    {
        conn.execute_batch(
            "ALTER TABLE memory_repair_install
             ADD COLUMN skipped_compactions INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    conn.execute(
        "INSERT INTO memory_repair_install(
             singleton, attempt_id, quarantine_path, source_main_sha256,
             complete, salvage_succeeded, recovered_messages, recovered_titles,
             recovered_prompt_refs, skipped_prompt_refs, recovered_compactions,
             skipped_compactions, recovery_warning
         ) VALUES(1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(singleton) DO UPDATE SET
             attempt_id = excluded.attempt_id,
             quarantine_path = excluded.quarantine_path,
             source_main_sha256 = excluded.source_main_sha256,
             complete = excluded.complete,
             salvage_succeeded = excluded.salvage_succeeded,
             recovered_messages = excluded.recovered_messages,
             recovered_titles = excluded.recovered_titles,
             recovered_prompt_refs = excluded.recovered_prompt_refs,
             skipped_prompt_refs = excluded.skipped_prompt_refs,
             recovered_compactions = excluded.recovered_compactions,
             skipped_compactions = excluded.skipped_compactions,
             recovery_warning = excluded.recovery_warning",
        params![
            &marker.attempt_id,
            &marker.quarantine_path,
            marker.source_main_sha256.as_deref(),
            marker.complete,
            marker.salvage_succeeded,
            marker.recovered.messages as i64,
            marker.recovered.titles as i64,
            marker.recovered.prompt_references as i64,
            marker.recovered.skipped_prompt_references as i64,
            marker.recovered.compactions as i64,
            marker.recovered.skipped_compactions as i64,
            marker.recovery_warning.as_deref(),
        ],
    )?;
    Ok(())
}

fn read_replacement_marker(path: &Path) -> Result<Option<ReplacementMarker>, MemoryError> {
    if !path.exists() {
        return Ok(None);
    }
    let standalone_path = suffix_path(
        path,
        &format!(".marker-check-{}", uuid::Uuid::new_v4().simple()),
    );
    remove_replacement_scratch(&standalone_path)?;
    copy_snapshot_file(path, &standalone_path)?;
    let standalone = StandaloneMainCopy {
        path: standalone_path,
    };
    let conn = Connection::open_with_flags(&standalone.path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    if schema_object_type(&conn, REPLACEMENT_MARKER_TABLE)?.is_none() {
        return Ok(None);
    }
    let marker_columns = table_columns(&conn, REPLACEMENT_MARKER_TABLE)?;
    if !marker_columns
        .iter()
        .any(|column| column == "recovered_compactions")
    {
        conn.execute_batch(
            "ALTER TABLE memory_repair_install
             ADD COLUMN recovered_compactions INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    let marker_columns = table_columns(&conn, REPLACEMENT_MARKER_TABLE)?;
    if !marker_columns
        .iter()
        .any(|column| column == "skipped_compactions")
    {
        conn.execute_batch(
            "ALTER TABLE memory_repair_install
             ADD COLUMN skipped_compactions INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    let marker = conn
        .query_row(
            "SELECT attempt_id, quarantine_path, source_main_sha256, complete,
                salvage_succeeded, recovered_messages, recovered_titles,
                recovered_prompt_refs, skipped_prompt_refs, recovered_compactions,
                skipped_compactions, recovery_warning
         FROM memory_repair_install
         WHERE singleton = 1",
            [],
            |row| {
                Ok(ReplacementMarker {
                    attempt_id: row.get(0)?,
                    quarantine_path: row.get(1)?,
                    source_main_sha256: row.get(2)?,
                    complete: row.get(3)?,
                    salvage_succeeded: row.get(4)?,
                    recovered: RecoveredRecords {
                        messages: row.get::<_, i64>(5)? as u64,
                        titles: row.get::<_, i64>(6)? as u64,
                        prompt_references: row.get::<_, i64>(7)? as u64,
                        skipped_prompt_references: row.get::<_, i64>(8)? as u64,
                        compactions: row.get::<_, i64>(9)? as u64,
                        skipped_compactions: row.get::<_, i64>(10)? as u64,
                    },
                    recovery_warning: row.get(11)?,
                })
            },
        )
        .optional()?;
    if let Some(marker) = marker.as_ref() {
        let messages = conn.query_row("SELECT COUNT(*) FROM messages", [], |row| {
            row.get::<_, i64>(0)
        })? as u64;
        let titles = conn.query_row("SELECT COUNT(*) FROM session_titles", [], |row| {
            row.get::<_, i64>(0)
        })? as u64;
        let prompt_references =
            conn.query_row("SELECT COUNT(*) FROM session_system_prompts", [], |row| {
                row.get::<_, i64>(0)
            })? as u64;
        let compactions = conn.query_row(
            "SELECT COUNT(*) FROM session_compactions WHERE state = 'completed'",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        if messages < marker.recovered.messages
            || titles < marker.recovered.titles
            || prompt_references < marker.recovered.prompt_references
            || compactions < marker.recovered.compactions
        {
            return Err(MemoryError::Integrity(format!(
                "replacement {} does not contain the rows recorded by its repair marker",
                path.display()
            )));
        }
    }
    Ok(marker)
}

fn replacement_marker_matches(
    marker: &ReplacementMarker,
    attempt_id: &str,
    quarantine: &Path,
    source_hash: Option<&str>,
) -> bool {
    marker.complete
        && marker.attempt_id == attempt_id
        && marker.quarantine_path == quarantine.display().to_string()
        && marker.source_main_sha256.as_deref() == source_hash
}

fn move_family(
    source: &Path,
    destination: &Path,
    moved: &mut Vec<PathBuf>,
) -> Result<(), MemoryError> {
    move_if_present(source, destination, moved)?;
    move_if_present(&wal_path(source), &wal_path(destination), moved)?;
    move_if_present(&shm_path(source), &shm_path(destination), moved)?;
    Ok(())
}

fn file_sha256_optional(path: &Path) -> Result<Option<String>, MemoryError> {
    if !path.exists() {
        return Ok(None);
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(hex::encode(hasher.finalize())))
}

fn staged_replacement_path(path: &Path, attempt_id: &str) -> PathBuf {
    suffix_path(path, &format!(".replacement-{attempt_id}"))
}

fn remove_if_present(path: &Path) -> Result<(), MemoryError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MemoryError::Io(error)),
    }
}

fn prepare_target_for_recovery(target: &Connection) -> Result<(), MemoryError> {
    target.execute_batch(
        "DROP TRIGGER IF EXISTS messages_ai;
         DROP TRIGGER IF EXISTS messages_ad;
         DROP TRIGGER IF EXISTS messages_au;
         DROP TABLE IF EXISTS messages_fts;
         DROP INDEX IF EXISTS messages_session_ts;
         DROP INDEX IF EXISTS messages_ts;",
    )?;
    Ok(())
}

fn rebuild_target_projections(target: &mut Connection) -> Result<(), MemoryError> {
    let tx = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS messages_session_ts
             ON messages(session_id, ts_ms);
         CREATE INDEX IF NOT EXISTS messages_ts
             ON messages(ts_ms);",
    )?;
    tx.execute_batch(FTS_SCHEMA)?;
    tx.execute(
        "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn recover_authoritative_messages(
    source: &Connection,
    target: &mut Connection,
) -> Result<(RecoveredRecords, HashSet<String>), MemoryError> {
    let tx = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut recovered = RecoveredRecords::default();
    let mut sessions = HashSet::new();
    {
        let mut statement = source.prepare(
            "SELECT id, session_id, role, content, ts_ms
             FROM messages NOT INDEXED
             ORDER BY rowid",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let session_id: String = row.get(1)?;
            tx.execute(
                "INSERT INTO messages(id, session_id, role, content, ts_ms)
                 VALUES (?, ?, ?, ?, ?)",
                params![
                    row.get::<_, i64>(0)?,
                    &session_id,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ],
            )?;
            sessions.insert(session_id);
            recovered.messages += 1;
        }
    }
    tx.commit()?;
    Ok((recovered, sessions))
}

fn recover_titles_projection(
    source: &Connection,
    target: &mut Connection,
    sessions: &HashSet<String>,
) -> Result<u64, MemoryError> {
    if !table_has_columns(source, "session_titles", EXPECTED_TABLES[1].1)? {
        return Err(MemoryError::Integrity(
            "session_titles table is missing or incompatible".to_string(),
        ));
    }
    let mut statement =
        source.prepare("SELECT session_id, title, ts_ms FROM session_titles NOT INDEXED")?;
    let mut rows = statement.query([])?;
    let tx = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut recovered = 0_u64;
    while let Some(row) = rows.next()? {
        let session_id: String = row.get(0)?;
        let title: String = row.get(1)?;
        if sessions.contains(&session_id) && !title.trim().is_empty() {
            tx.execute(
                "INSERT INTO session_titles(session_id, title, ts_ms) VALUES (?, ?, ?)",
                params![session_id, title, row.get::<_, i64>(2)?],
            )?;
            recovered += 1;
        }
    }
    tx.commit()?;
    Ok(recovered)
}

fn recover_prompt_projection(
    source: &Connection,
    target: &mut Connection,
    sessions: &HashSet<String>,
) -> Result<(u64, u64), MemoryError> {
    if !table_has_columns(source, "system_prompts", EXPECTED_TABLES[2].1)?
        || !table_has_columns(source, "session_system_prompts", EXPECTED_TABLES[3].1)?
    {
        return Err(MemoryError::Integrity(
            "system prompt tables are missing or incompatible".to_string(),
        ));
    }

    let mut prompts = HashMap::new();
    {
        let mut statement =
            source.prepare("SELECT hash, prompt FROM system_prompts NOT INDEXED")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            prompts.insert(row.get::<_, String>(0)?, row.get::<_, String>(1)?);
        }
    }

    let mut references = Vec::new();
    {
        let mut statement = source.prepare(
            "SELECT session_id, prompt_hash, prompt_version, ts_ms
             FROM session_system_prompts NOT INDEXED",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            references.push((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, i64>(3)?,
            ));
        }
    }

    let tx = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut recovered = 0_u64;
    let mut skipped = 0_u64;
    for (session_id, hash, prompt_version, ts_ms) in references {
        let Some(prompt) = prompts
            .get(&hash)
            .filter(|prompt| system_prompt_hash(prompt) == hash)
        else {
            skipped += 1;
            continue;
        };
        if !sessions.contains(&session_id) {
            skipped += 1;
            continue;
        }
        tx.execute(
            "INSERT OR IGNORE INTO system_prompts(hash, prompt) VALUES (?, ?)",
            params![hash, prompt],
        )?;
        tx.execute(
            "INSERT INTO session_system_prompts(
                 session_id, prompt_hash, prompt_version, ts_ms
             ) VALUES (?, ?, ?, ?)",
            params![session_id, hash, prompt_version, ts_ms],
        )?;
        recovered += 1;
    }
    tx.commit()?;
    Ok((recovered, skipped))
}

fn checkpoint_wal(conn: &Connection) -> Result<(), MemoryError> {
    let (busy, frames, checkpointed) =
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
    if busy != 0 || (frames >= 0 && checkpointed < frames) {
        return Err(MemoryError::Repair(
            "WAL checkpoint did not fully complete; an uncoordinated SQLite reader or writer may still be active"
                .to_string(),
        ));
    }
    Ok(())
}

fn ensure_wal_fully_checkpointed(path: &Path) -> Result<(), MemoryError> {
    inspect_wal(&wal_path(path), path)
        .map_err(|error| MemoryError::Integrity(format!("refusing WAL checkpoint: {error}")))?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    checkpoint_wal(&conn)?;
    drop(conn);
    match fs::metadata(wal_path(path)) {
        Ok(metadata) if metadata.len() != 0 => Err(MemoryError::Repair(format!(
            "WAL checkpoint returned without truncating {}",
            wal_path(path).display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MemoryError::Io(error)),
    }
}

fn read_repair_log(path: &Path) -> Result<RepairLogSnapshot, MemoryError> {
    let log_path = repair_log_path(path);
    let metadata = match fs::symlink_metadata(&log_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RepairLogSnapshot::default())
        }
        Err(error) => return Err(MemoryError::Io(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(RepairLogSnapshot {
            malformed_lines: 1,
            ..RepairLogSnapshot::default()
        });
    }
    if metadata.len() > MAX_REPAIR_LOG_BYTES {
        return Ok(RepairLogSnapshot {
            malformed_lines: 1,
            ..RepairLogSnapshot::default()
        });
    }

    let file = File::open(&log_path)?;
    let mut active: HashMap<String, RepairEvent> = HashMap::new();
    let mut last_applied = None;
    let mut malformed_lines = 0;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: RepairEvent = match serde_json::from_str::<RepairEvent>(&line) {
            Ok(event)
                if event.version == REPAIR_LOG_VERSION && valid_repair_event(path, &event) =>
            {
                event
            }
            Ok(_) | Err(_) => {
                malformed_lines += 1;
                continue;
            }
        };
        match event.phase {
            RepairPhase::Started => {
                active.insert(event.attempt_id.clone(), event);
            }
            RepairPhase::Completed | RepairPhase::Failed => {
                active.remove(&event.attempt_id);
                last_applied = Some(event);
            }
        }
    }
    let mut incomplete: Vec<_> = active.into_values().collect();
    incomplete.sort_by_key(|event| event.ts_ms);
    Ok(RepairLogSnapshot {
        incomplete,
        last_applied,
        malformed_lines,
    })
}

fn valid_repair_event(database: &Path, event: &RepairEvent) -> bool {
    let valid_id = !event.attempt_id.is_empty()
        && event.attempt_id.len() <= 64
        && event
            .attempt_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !valid_id {
        return false;
    }
    match event.mode {
        RepairMode::InPlace => event.quarantine_path.is_none(),
        RepairMode::Quarantine => event
            .quarantine_path
            .as_deref()
            .map(Path::new)
            .is_some_and(|path| is_quarantine_sibling(database, path)),
    }
}

fn is_quarantine_sibling(database: &Path, quarantine: &Path) -> bool {
    if database.parent() != quarantine.parent() {
        return false;
    }
    let Some(database_name) = database.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(quarantine_name) = quarantine.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    quarantine_name.starts_with(&format!("{database_name}.quarantine-"))
}

fn append_repair_event(path: &Path, event: &RepairEvent) -> Result<(), MemoryError> {
    let log_path = repair_log_path(path);
    reject_symlink(&log_path)?;
    let line = serde_json::to_string(event)
        .map_err(|error| MemoryError::Repair(format!("serialize repair event: {error}")))?;
    let mut options = OpenOptions::new();
    options.create(true).append(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let mut file = options.open(&log_path)?;
    crate::storage::set_private_file(&log_path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    sync_parent(&log_path)?;
    Ok(())
}

fn add_forced_fts_action(actions: &mut Vec<String>, force: bool) {
    if force
        && !actions
            .iter()
            .any(|action| action == "rebuild_fts_and_triggers")
    {
        actions.push("rebuild_fts_and_triggers".to_string());
    }
}

fn schema_object_type(conn: &Connection, name: &str) -> Result<Option<String>, MemoryError> {
    conn.query_row(
        "SELECT type FROM sqlite_schema WHERE name = ?",
        params![name],
        |row| row.get(0),
    )
    .optional()
    .map_err(MemoryError::from)
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, MemoryError> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns)
}

fn table_has_columns(
    conn: &Connection,
    table: &str,
    expected: &[&str],
) -> Result<bool, MemoryError> {
    if schema_object_type(conn, table)?.as_deref() != Some("table") {
        return Ok(false);
    }
    let actual = table_columns(conn, table)?;
    Ok(expected
        .iter()
        .all(|column| actual.iter().any(|actual| actual == *column)))
}

fn normalize_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace() && *character != '"')
        .flat_map(char::to_lowercase)
        .collect()
}

fn trigger_definition_matches(name: &str, sql: &str) -> bool {
    match name {
        "messages_ai" => {
            sql.contains("afterinsertonmessages")
                && sql.contains("insertintomessages_fts(rowid,content)values(new.id,new.content)")
        }
        "messages_ad" => {
            sql.contains("afterdeleteonmessages")
                && sql.contains("values('delete',old.id,old.content)")
        }
        "messages_au" => {
            sql.contains("afterupdateonmessages")
                && sql.contains("values('delete',old.id,old.content)")
                && sql.contains("values(new.id,new.content)")
        }
        _ => false,
    }
}

fn reject_symlink(path: &Path) -> Result<(), MemoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(MemoryError::Repair(format!(
            "refusing symlink in memory repair path: {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MemoryError::Io(error)),
    }
}

fn reject_non_regular_file(path: &Path) -> Result<(), MemoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(MemoryError::Repair(format!(
                "refusing non-regular memory repair path: {}",
                path.display()
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MemoryError::Io(error)),
    }
}

fn move_if_present(
    source: &Path,
    destination: &Path,
    moved: &mut Vec<PathBuf>,
) -> Result<(), MemoryError> {
    if !source.exists() {
        return Ok(());
    }
    reject_symlink(source)?;
    if destination.exists() {
        return Err(MemoryError::Repair(format!(
            "quarantine destination already exists: {}",
            destination.display()
        )));
    }
    fs::rename(source, destination)?;
    crate::storage::set_private_file(destination)?;
    moved.push(destination.to_path_buf());
    Ok(())
}

fn existing_quarantine_files(base: &Path) -> Vec<PathBuf> {
    [base.to_path_buf(), wal_path(base), shm_path(base)]
        .into_iter()
        .filter(|path| path.exists())
        .collect()
}

fn remove_replacement_scratch(path: &Path) -> Result<(), MemoryError> {
    for candidate in [path.to_path_buf(), wal_path(path), shm_path(path)] {
        if candidate.exists() {
            reject_symlink(&candidate)?;
            fs::remove_file(candidate)?;
        }
    }
    Ok(())
}

fn harden_sqlite_files(path: &Path) -> Result<(), MemoryError> {
    for candidate in [path.to_path_buf(), wal_path(path), shm_path(path)] {
        if candidate.exists() {
            reject_symlink(&candidate)?;
            crate::storage::set_private_file(&candidate)?;
        }
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), MemoryError> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn lifecycle_lock_path(path: &Path) -> PathBuf {
    suffix_path(path, ".lifecycle.lock")
}

fn repair_log_path(path: &Path) -> PathBuf {
    suffix_path(path, ".repair.jsonl")
}

fn wal_path(path: &Path) -> PathBuf {
    suffix_path(path, "-wal")
}

fn shm_path(path: &Path) -> PathBuf {
    suffix_path(path, "-shm")
}

fn quarantine_base(path: &Path, attempt_id: &str) -> PathBuf {
    suffix_path(path, &format!(".quarantine-{attempt_id}"))
}

fn suffix_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn current_ts_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory/recovery.rs"
    ));
}
