mod object_state;

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};

use super::{
    normalize_completion_note, normalize_resource, parse_id, validate_planning, validate_resources,
    Activity, ActivityDraft, ActivityError, ActivityPatch, ActivityReceipt, ActivityResource,
    ActivityService, ActivityState, ObjectStateDraft, ObjectStateEntry, ReceiptDeclaration,
    ReceiptReport, ReceiptSource,
    DATABASE_SCHEMA_VERSION, DEFAULT_LIST_LIMIT, MAX_ACTIVITIES_PER_OWNER, MAX_LIST_LIMIT,
};

const MAX_RECEIPTS_PER_ACTIVITY: i64 = 1000;

const SCHEMA: &str = r#"
CREATE TABLE activities (
    id                  TEXT PRIMARY KEY NOT NULL,
    owner_uid           INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    title               TEXT NOT NULL,
    goal                TEXT NOT NULL,
    completion_criteria TEXT NOT NULL,
    boundaries          TEXT NOT NULL,
    resources_json      TEXT NOT NULL,
    state               TEXT NOT NULL CHECK(state IN ('active', 'paused', 'completed', 'cancelled')),
    completion_note     TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    CHECK (
        (state = 'completed' AND completion_note IS NOT NULL AND length(completion_note) > 0)
        OR (state != 'completed' AND completion_note IS NULL)
    )
);

CREATE INDEX activities_owner_updated ON activities(owner_uid, updated_at DESC, id DESC);
CREATE INDEX activities_owner_state_updated
    ON activities(owner_uid, state, updated_at DESC, id DESC);
"#;

const MIGRATE_TO_V2: &str = r#"
CREATE UNIQUE INDEX activities_owner_id ON activities(owner_uid, id);

CREATE TABLE activity_receipts (
    sequence          INTEGER PRIMARY KEY AUTOINCREMENT,
    id                TEXT NOT NULL,
    activity_id       TEXT NOT NULL,
    owner_uid         INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    received_at       TEXT NOT NULL,
    source            TEXT NOT NULL CHECK(source = 'caller_reported'),
    report_json       TEXT NOT NULL,
    declaration_json  TEXT,
    declaration_error TEXT,
    UNIQUE(owner_uid, id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id),
    CHECK (
        (declaration_json IS NOT NULL AND declaration_error IS NULL)
        OR (declaration_json IS NULL AND declaration_error IS NOT NULL)
    )
);

CREATE INDEX activity_receipts_owner_activity
    ON activity_receipts(owner_uid, activity_id, sequence DESC);
"#;

const SELECT_RECEIPT: &str = "SELECT id, activity_id, owner_uid, received_at, source, report_json,
            declaration_json, declaration_error FROM activity_receipts";

const SELECT_ACTIVITY: &str = "SELECT id, owner_uid, title, goal, completion_criteria, boundaries,
            resources_json, state, completion_note, created_at, updated_at
     FROM activities";

#[derive(Clone)]
pub struct SqliteActivityService {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteActivityService {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ActivityError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            prepare_directory(parent)?;
        }
        secure_sidecars(path)?;
        prepare_database_file(path)?;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let service = Self::initialize(conn, true)?;
        secure_sidecars(path)?;
        Ok(service)
    }

    pub fn open_in_memory() -> Result<Self, ActivityError> {
        Self::initialize(Connection::open_in_memory()?, false)
    }

    fn initialize(mut conn: Connection, durable: bool) -> Result<Self, ActivityError> {
        conn.busy_timeout(Duration::from_secs(5))?;
        check_version(&conn)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "trusted_schema", false)?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        if durable {
            let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
            if mode != "wal" {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "activity database requires WAL journaling",
                )
                .into());
            }
        }

        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version = check_version(&tx)?;
        if version == 0 {
            tx.execute_batch(SCHEMA)?;
        }
        tx.prepare(&format!("{SELECT_ACTIVITY} LIMIT 0"))?;
        if version < 2 {
            tx.execute_batch(MIGRATE_TO_V2)?;
        }
        tx.prepare(&format!("{SELECT_RECEIPT} LIMIT 0"))?;
        if version < 3 {
            tx.execute_batch(object_state::MIGRATE_TO_V3)?;
        }
        object_state::validate_schema(&tx)?;
        let integrity: String = tx.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(ActivityError::Corrupt(format!(
                "SQLite integrity check failed: {integrity}"
            )));
        }
        let invalid_foreign_key: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
            [],
            |row| row.get(0),
        )?;
        if invalid_foreign_key {
            return Err(ActivityError::Corrupt(
                "Activity ledger ownership or reference is invalid".to_string(),
            ));
        }
        if version < i64::from(DATABASE_SCHEMA_VERSION) {
            object_state::validate_migration_records(&tx)?;
            tx.pragma_update(None, "user_version", DATABASE_SCHEMA_VERSION)?;
        }
        tx.commit()?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, ActivityError> {
        self.conn.lock().map_err(|_| ActivityError::Poisoned)
    }
}

impl ActivityService for SqliteActivityService {
    fn create(&self, owner_uid: u32, draft: ActivityDraft) -> Result<Activity, ActivityError> {
        let draft = draft.normalized()?;
        let resources_json = serde_json::to_string(&draft.resources)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM activities WHERE owner_uid = ?1",
            [owner_uid],
            |row| row.get(0),
        )?;
        if count >= MAX_ACTIVITIES_PER_OWNER {
            return Err(ActivityError::LimitReached);
        }
        let now = timestamp();
        let activity = Activity {
            id: uuid::Uuid::new_v4().to_string(),
            owner_uid,
            title: draft.title,
            goal: draft.goal,
            completion_criteria: draft.completion_criteria,
            boundaries: draft.boundaries,
            resources: draft.resources,
            state: ActivityState::Active,
            completion_note: None,
            created_at: now.clone(),
            updated_at: now,
        };
        tx.execute(
            "INSERT INTO activities (
                id, owner_uid, title, goal, completion_criteria, boundaries,
                resources_json, state, completion_note, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', NULL, ?8, ?8)",
            params![
                activity.id,
                owner_uid,
                activity.title,
                activity.goal,
                activity.completion_criteria,
                activity.boundaries,
                resources_json,
                activity.created_at,
            ],
        )?;
        tx.commit()?;
        Ok(activity)
    }

    fn get(&self, owner_uid: u32, id: &str) -> Result<Activity, ActivityError> {
        let id = parse_id(id)?;
        let conn = self.lock()?;
        load_activity(&conn, owner_uid, &id)
    }

    fn list(
        &self,
        owner_uid: u32,
        state: Option<ActivityState>,
        limit: usize,
    ) -> Result<Vec<Activity>, ActivityError> {
        let limit = list_limit(limit)?;
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{SELECT_ACTIVITY}
             WHERE owner_uid = ?1 AND (?2 IS NULL OR state = ?2)
             ORDER BY updated_at DESC, id DESC LIMIT ?3"
        ))?;
        let rows = statement.query_map(
            params![owner_uid, state.map(|state| state.as_str()), limit],
            ActivityRow::from_row,
        )?;
        rows.map(|row| row?.into_activity()).collect()
    }

    fn update(
        &self,
        owner_uid: u32,
        id: &str,
        patch: ActivityPatch,
    ) -> Result<Activity, ActivityError> {
        let id = parse_id(id)?;
        patch.validate()?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut activity = load_activity(&tx, owner_uid, &id)?;
        if matches!(
            activity.state,
            ActivityState::Completed | ActivityState::Cancelled
        ) {
            return Err(ActivityError::Conflict(
                "reopen a terminal activity before editing its planning fields".to_string(),
            ));
        }
        patch.apply(&mut activity);
        activity.updated_at = timestamp().max(activity.updated_at);
        let resources_json = serde_json::to_string(&activity.resources)?;
        tx.execute(
            "UPDATE activities SET title = ?1, goal = ?2, completion_criteria = ?3,
                boundaries = ?4, resources_json = ?5, updated_at = ?6
             WHERE owner_uid = ?7 AND id = ?8",
            params![
                activity.title,
                activity.goal,
                activity.completion_criteria,
                activity.boundaries,
                resources_json,
                activity.updated_at,
                owner_uid,
                id,
            ],
        )?;
        tx.commit()?;
        Ok(activity)
    }

    fn add_resource(
        &self,
        owner_uid: u32,
        id: &str,
        resource: ActivityResource,
    ) -> Result<Activity, ActivityError> {
        let id = parse_id(id)?;
        validate_resources(std::slice::from_ref(&resource))?;
        let resource = normalize_resource(resource);
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut activity = load_activity(&tx, owner_uid, &id)?;
        if matches!(
            activity.state,
            ActivityState::Completed | ActivityState::Cancelled
        ) {
            return Err(ActivityError::Conflict(
                "reopen a terminal activity before attaching resources".to_string(),
            ));
        }
        if let Some(existing) = activity
            .resources
            .iter_mut()
            .find(|existing| existing.reference == resource.reference)
        {
            *existing = resource;
        } else {
            activity.resources.push(resource);
        }
        validate_resources(&activity.resources)?;
        activity.updated_at = timestamp().max(activity.updated_at);
        tx.execute(
            "UPDATE activities SET resources_json = ?1, updated_at = ?2
             WHERE owner_uid = ?3 AND id = ?4",
            params![
                serde_json::to_string(&activity.resources)?,
                activity.updated_at,
                owner_uid,
                id,
            ],
        )?;
        tx.commit()?;
        Ok(activity)
    }

    fn transition(
        &self,
        owner_uid: u32,
        id: &str,
        state: ActivityState,
        completion_note: Option<String>,
    ) -> Result<Activity, ActivityError> {
        let id = parse_id(id)?;
        let completion_note = normalize_completion_note(state, completion_note)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut activity = load_activity(&tx, owner_uid, &id)?;
        if !activity.state.allows_transition(state) {
            return Err(ActivityError::Conflict(format!(
                "cannot transition from {} to {}",
                activity.state.as_str(),
                state.as_str(),
            )));
        }
        activity.state = state;
        activity.completion_note = completion_note;
        activity.updated_at = timestamp().max(activity.updated_at);
        tx.execute(
            "UPDATE activities SET state = ?1, completion_note = ?2, updated_at = ?3
             WHERE owner_uid = ?4 AND id = ?5",
            params![
                state.as_str(),
                activity.completion_note,
                activity.updated_at,
                owner_uid,
                id,
            ],
        )?;
        tx.commit()?;
        Ok(activity)
    }

    fn record_receipt(
        &self,
        owner_uid: u32,
        activity_id: &str,
        report: ReceiptReport,
        declaration: Option<ReceiptDeclaration>,
        declaration_error: Option<String>,
    ) -> Result<ActivityReceipt, ActivityError> {
        let activity_id = parse_id(activity_id)?;
        let report = report.canonicalized()?;
        super::receipts::validate_declaration(declaration.as_ref(), declaration_error.as_deref())?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        load_activity(&tx, owner_uid, &activity_id)?;
        if let Some(existing) = load_receipt(&tx, owner_uid, &report.id)? {
            if existing.activity_id != activity_id || existing.report != report {
                return Err(ActivityError::Conflict(
                    "receipt id is already bound to a different Activity or report".to_string(),
                ));
            }
            tx.commit()?;
            return Ok(existing);
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM activity_receipts WHERE owner_uid = ?1 AND activity_id = ?2",
            params![owner_uid, activity_id],
            |row| row.get(0),
        )?;
        if count >= MAX_RECEIPTS_PER_ACTIVITY {
            return Err(ActivityError::LimitReached);
        }
        let receipt = ActivityReceipt {
            id: report.id.clone(),
            activity_id,
            owner_uid,
            received_at: timestamp(),
            source: ReceiptSource::CallerReported,
            report,
            declaration,
            declaration_error,
        };
        let declaration_json = receipt
            .declaration
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let inserted = tx.execute(
            "INSERT INTO activity_receipts (
                id, activity_id, owner_uid, received_at, source,
                report_json, declaration_json, declaration_error
             ) VALUES (?1, ?2, ?3, ?4, 'caller_reported', ?5, ?6, ?7)",
            params![
                receipt.id,
                receipt.activity_id,
                owner_uid,
                receipt.received_at,
                serde_json::to_string(&receipt.report)?,
                declaration_json,
                receipt.declaration_error,
            ],
        )?;
        if inserted != 1 || load_receipt(&tx, owner_uid, &receipt.id)?.as_ref() != Some(&receipt) {
            return Err(ActivityError::Corrupt(
                "receipt append did not persist the original immutable record".to_string(),
            ));
        }
        tx.commit()?;
        Ok(receipt)
    }

    fn receipts(
        &self,
        owner_uid: u32,
        activity_id: &str,
        limit: usize,
    ) -> Result<Vec<ActivityReceipt>, ActivityError> {
        let activity_id = parse_id(activity_id)?;
        let limit = list_limit(limit)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        load_activity(&tx, owner_uid, &activity_id)?;
        let receipts = {
            let mut statement = tx.prepare(&format!(
                "{SELECT_RECEIPT} WHERE owner_uid = ?1 AND activity_id = ?2
                 ORDER BY sequence DESC LIMIT ?3"
            ))?;
            let rows = statement
                .query_map(params![owner_uid, activity_id, limit], ReceiptRow::from_row)?;
            rows.map(|row| row?.into_receipt())
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        Ok(receipts)
    }

    fn record_object_state(
        &self,
        owner_uid: u32,
        activity_id: &str,
        draft: ObjectStateDraft,
    ) -> Result<ObjectStateEntry, ActivityError> {
        object_state::record(self, owner_uid, activity_id, draft)
    }

    fn object_state(
        &self,
        owner_uid: u32,
        activity_id: &str,
        reference: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ObjectStateEntry>, ActivityError> {
        object_state::list(self, owner_uid, activity_id, reference, limit)
    }
}

fn list_limit(limit: usize) -> Result<i64, ActivityError> {
    let limit = if limit == 0 {
        DEFAULT_LIST_LIMIT
    } else {
        limit.min(MAX_LIST_LIMIT)
    };
    i64::try_from(limit)
        .map_err(|_| ActivityError::Invalid("list limit is not representable".to_string()))
}

fn check_version(conn: &Connection) -> Result<i64, ActivityError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match version {
        0 => {
            let occupied: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*')",
                [],
                |row| row.get(0),
            )?;
            if occupied {
                return Err(ActivityError::Corrupt(
                    "unversioned database contains an existing schema; refusing to initialize it"
                        .to_string(),
                ));
            }
        }
        1 | 2 => {}
        version if version == i64::from(DATABASE_SCHEMA_VERSION) => {}
        found => {
            return Err(ActivityError::SchemaVersion {
                found,
                supported: DATABASE_SCHEMA_VERSION,
            });
        }
    }
    Ok(version)
}

fn prepare_directory(path: &Path) -> Result<(), ActivityError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "activity database parent must be a real directory",
            )
            .into());
        }
        Ok(metadata) => {
            // The daemon data root is intentionally 0711 so isolated workers
            // can reach their own partitions. Preserve traversal, not listing
            // or write access, while keeping the database itself private.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o066 == 0 {
                    return Ok(());
                }
            }
            #[cfg(not(unix))]
            let _ = metadata;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    crate::storage::ensure_private_dir(path)?;
    Ok(())
}

fn prepare_database_file(path: &Path) -> Result<(), ActivityError> {
    let empty_or_missing = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            require_regular_file(path)?;
            metadata.len() == 0
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    if empty_or_missing {
        for suffix in ["-wal", "-shm", "-journal"] {
            match fs::symlink_metadata(sidecar_path(path, suffix)) {
                Ok(_) => {
                    return Err(ActivityError::Corrupt(
                        "database is missing or empty but SQLite sidecars remain; refusing to reset it"
                            .to_string(),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            require_regular_file(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    crate::storage::set_private_file(path)?;
    Ok(())
}

fn require_regular_file(path: &Path) -> Result<(), ActivityError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "activity database and journals must be regular files, not symlinks",
        )
        .into());
    }
    Ok(())
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn secure_sidecars(path: &Path) -> Result<(), ActivityError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = sidecar_path(path, suffix);
        match fs::symlink_metadata(&sidecar) {
            Ok(_) => {
                require_regular_file(&sidecar)?;
                crate::storage::set_private_file(&sidecar)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn load_activity(conn: &Connection, owner_uid: u32, id: &str) -> Result<Activity, ActivityError> {
    conn.query_row(
        &format!("{SELECT_ACTIVITY} WHERE owner_uid = ?1 AND id = ?2"),
        params![owner_uid, id],
        ActivityRow::from_row,
    )
    .optional()?
    .ok_or(ActivityError::NotFound)?
    .into_activity()
}

struct ActivityRow {
    id: String,
    owner_uid: u32,
    title: String,
    goal: String,
    completion_criteria: String,
    boundaries: String,
    resources_json: String,
    state: String,
    completion_note: Option<String>,
    created_at: String,
    updated_at: String,
}

impl ActivityRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            owner_uid: row.get(1)?,
            title: row.get(2)?,
            goal: row.get(3)?,
            completion_criteria: row.get(4)?,
            boundaries: row.get(5)?,
            resources_json: row.get(6)?,
            state: row.get(7)?,
            completion_note: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    }

    fn into_activity(self) -> Result<Activity, ActivityError> {
        let state = ActivityState::parse(&self.state)
            .map_err(|_| ActivityError::Corrupt("invalid stored state".to_string()))?;
        let resources: Vec<ActivityResource> = serde_json::from_str(&self.resources_json)?;
        validate_planning(
            &self.title,
            &self.goal,
            &self.completion_criteria,
            &self.boundaries,
            &resources,
        )
        .map_err(|error| ActivityError::Corrupt(error.to_string()))?;
        let canonical_id = parse_id(&self.id)
            .map_err(|_| ActivityError::Corrupt("invalid stored UUID".to_string()))?;
        if canonical_id != self.id {
            return Err(ActivityError::Corrupt(
                "stored UUID is not canonical".to_string(),
            ));
        }
        let note = normalize_completion_note(state, self.completion_note.clone())
            .map_err(|error| ActivityError::Corrupt(error.to_string()))?;
        let untrimmed = [
            &self.title,
            &self.goal,
            &self.completion_criteria,
            &self.boundaries,
        ]
        .into_iter()
        .any(|value| value.trim() != value)
            || resources.iter().any(|resource| {
                resource.label.trim() != resource.label
                    || resource.reference.trim() != resource.reference
            })
            || note != self.completion_note;
        if untrimmed {
            return Err(ActivityError::Corrupt(
                "stored planning text is not trimmed".to_string(),
            ));
        }
        let created_at = parse_timestamp(&self.created_at)?;
        let updated_at = parse_timestamp(&self.updated_at)?;
        if updated_at < created_at {
            return Err(ActivityError::Corrupt(
                "updated_at precedes created_at".to_string(),
            ));
        }
        Ok(Activity {
            id: self.id,
            owner_uid: self.owner_uid,
            title: self.title,
            goal: self.goal,
            completion_criteria: self.completion_criteria,
            boundaries: self.boundaries,
            resources,
            state,
            completion_note: self.completion_note,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, ActivityError> {
    let timestamp = DateTime::parse_from_rfc3339(value)
        .map_err(|_| ActivityError::Corrupt("invalid stored RFC3339 timestamp".to_string()))?
        .with_timezone(&Utc);
    if timestamp.to_rfc3339_opts(SecondsFormat::Nanos, true) != value {
        return Err(ActivityError::Corrupt(
            "stored timestamp is not canonical UTC".to_string(),
        ));
    }
    Ok(timestamp)
}

fn load_receipt(
    conn: &Connection,
    owner_uid: u32,
    id: &str,
) -> Result<Option<ActivityReceipt>, ActivityError> {
    let receipt = conn
        .query_row(
            &format!("{SELECT_RECEIPT} WHERE owner_uid = ?1 AND id = ?2"),
            params![owner_uid, id],
            ReceiptRow::from_row,
        )
        .optional()?
        .map(ReceiptRow::into_receipt)
        .transpose()?;
    if let Some(receipt) = &receipt {
        match load_activity(conn, owner_uid, &receipt.activity_id) {
            Ok(_) => {}
            Err(ActivityError::NotFound) => {
                return Err(ActivityError::Corrupt(
                    "receipt references a missing or foreign Activity".to_string(),
                ));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(receipt)
}

struct ReceiptRow {
    id: String,
    activity_id: String,
    owner_uid: u32,
    received_at: String,
    source: String,
    report_json: String,
    declaration_json: Option<String>,
    declaration_error: Option<String>,
}

impl ReceiptRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            activity_id: row.get(1)?,
            owner_uid: row.get(2)?,
            received_at: row.get(3)?,
            source: row.get(4)?,
            report_json: row.get(5)?,
            declaration_json: row.get(6)?,
            declaration_error: row.get(7)?,
        })
    }

    fn into_receipt(self) -> Result<ActivityReceipt, ActivityError> {
        let report: ReceiptReport = serde_json::from_str(&self.report_json)?;
        report
            .validate()
            .map_err(|error| ActivityError::Corrupt(error.to_string()))?;
        for id in [&self.id, &self.activity_id] {
            if parse_id(id).map_err(|error| ActivityError::Corrupt(error.to_string()))? != *id {
                return Err(ActivityError::Corrupt(
                    "receipt UUID is not canonical".to_string(),
                ));
            }
        }
        if report.id != self.id {
            return Err(ActivityError::Corrupt(
                "receipt key does not match the canonical report id".to_string(),
            ));
        }
        if self.source != "caller_reported" {
            return Err(ActivityError::Corrupt(
                "receipt source is not caller_reported".to_string(),
            ));
        }
        parse_timestamp(&self.received_at)?;
        let declaration: Option<ReceiptDeclaration> = self
            .declaration_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        super::receipts::validate_declaration(
            declaration.as_ref(),
            self.declaration_error.as_deref(),
        )
        .map_err(|error| ActivityError::Corrupt(error.to_string()))?;
        Ok(ActivityReceipt {
            id: self.id,
            activity_id: self.activity_id,
            owner_uid: self.owner_uid,
            received_at: self.received_at,
            source: ReceiptSource::CallerReported,
            report,
            declaration,
            declaration_error: self.declaration_error,
        })
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/sqlite.rs"
    ));
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/sqlite_receipts.rs"
    ));
}
