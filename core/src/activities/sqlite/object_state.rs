//! Immutable, bounded object metadata in the Activity database and owner boundary.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{
    list_limit, load_activity, load_receipt, parse_id, parse_timestamp, timestamp, Activity,
    ActivityError, ActivityRow, ReceiptReport, ReceiptRow, SqliteActivityService, SELECT_ACTIVITY,
    SELECT_RECEIPT,
};
use crate::activities::object_state::canonical_reference;
use crate::activities::{
    ObjectStateContent, ObjectStateDraft, ObjectStateEntry, ObjectStateSource,
};

const MAX_ENTRIES_PER_ACTIVITY: usize = 1000;

pub(super) const MIGRATE_TO_V3: &str = r#"
CREATE UNIQUE INDEX activity_receipts_owner_activity_id
    ON activity_receipts(owner_uid, activity_id, id);

CREATE TABLE activity_object_state (
    sequence      INTEGER PRIMARY KEY AUTOINCREMENT CHECK(sequence > 0),
    id            TEXT NOT NULL,
    activity_id   TEXT NOT NULL,
    owner_uid     INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    reference     TEXT NOT NULL,
    recorded_at   TEXT NOT NULL,
    source        TEXT NOT NULL CHECK(source = 'caller_reported'),
    draft_json    TEXT NOT NULL,
    receipt_id    TEXT,
    supersedes    TEXT,
    UNIQUE(owner_uid, id),
    UNIQUE(owner_uid, activity_id, reference, id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id),
    FOREIGN KEY(owner_uid, activity_id, receipt_id)
        REFERENCES activity_receipts(owner_uid, activity_id, id),
    FOREIGN KEY(owner_uid, activity_id, reference, supersedes)
        REFERENCES activity_object_state(owner_uid, activity_id, reference, id),
    CHECK(supersedes IS NULL OR supersedes != id)
);

CREATE INDEX activity_object_state_owner_activity
    ON activity_object_state(owner_uid, activity_id, sequence DESC);
CREATE INDEX activity_object_state_reference_history
    ON activity_object_state(owner_uid, activity_id, reference, sequence DESC);
CREATE UNIQUE INDEX activity_object_state_supersession
    ON activity_object_state(owner_uid, supersedes);
"#;

const SELECT_ENTRY: &str =
    "SELECT sequence, id, activity_id, owner_uid, reference, recorded_at, source,
        draft_json, receipt_id, supersedes FROM activity_object_state";

pub(super) fn record(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    draft: ObjectStateDraft,
) -> Result<ObjectStateEntry, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    let draft = draft.canonicalized()?;
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    let existing_activity: Option<String> = tx
        .query_row(
            "SELECT activity_id FROM activity_object_state WHERE owner_uid = ?1 AND id = ?2",
            params![owner_uid, draft.id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing_activity) = existing_activity {
        if parse_id(&existing_activity).map_err(stored_error)? != existing_activity {
            return corrupt("object-state Activity UUID is not canonical");
        }
        if existing_activity != activity_id {
            load_activity(&tx, owner_uid, &existing_activity).map_err(stored_error)?;
            return Err(ActivityError::Conflict(
                "object-state id is already bound to a different Activity".into(),
            ));
        }
    }
    let now = Utc::now();
    let mut history = load_history(&tx, &activity, now)?;
    if let Some(existing) = history.iter().find(|entry| entry.id == draft.id) {
        if existing.draft != draft {
            return Err(ActivityError::Conflict(
                "object-state id is already bound to a different draft".into(),
            ));
        }
        let existing = existing.clone();
        tx.commit()?;
        return Ok(existing);
    }

    require_attached(&activity, &draft.reference)?;
    if let ObjectStateContent::Relation { target, .. } = &draft.content {
        require_attached(&activity, target)?;
    }
    let correction = if let Some(supersedes) = &draft.supersedes {
        let index = history
            .iter()
            .position(|entry| &entry.id == supersedes)
            .ok_or_else(|| {
                ActivityError::Invalid(
                    "supersedes must name an existing entry in the owned Activity".into(),
                )
            })?;
        let previous = &history[index];
        if previous.draft.reference != draft.reference {
            return Err(ActivityError::Invalid(
                "a correction must preserve the exact subject reference".into(),
            ));
        }
        if previous.superseded_by.is_some() {
            return Err(ActivityError::Conflict(
                "the object-state entry has already been superseded".into(),
            ));
        }
        if matches!(previous.draft.content, ObjectStateContent::Retracted { .. }) {
            return Err(ActivityError::Conflict(
                "a retracted entry cannot be corrected; add a fresh independent entry".into(),
            ));
        }
        Some(index)
    } else {
        None
    };
    let receipt = linked_report(&tx, &activity, &draft)?;
    if history.len() >= MAX_ENTRIES_PER_ACTIVITY {
        return Err(ActivityError::LimitReached);
    }
    let entry = ObjectStateEntry {
        id: draft.id.clone(),
        activity_id,
        owner_uid,
        recorded_at: timestamp(),
        source: ObjectStateSource::CallerReported,
        validity: draft.validity_at(now)?,
        draft,
        receipt,
        superseded_by: None,
    };
    let inserted = tx.execute(
        "INSERT INTO activity_object_state (
            id, activity_id, owner_uid, reference, recorded_at, source,
            draft_json, receipt_id, supersedes
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'caller_reported', ?6, ?7, ?8)",
        params![
            entry.id,
            entry.activity_id,
            owner_uid,
            entry.draft.reference,
            entry.recorded_at,
            serde_json::to_string(&entry.draft)?,
            receipt_id(&entry.draft),
            entry.draft.supersedes,
        ],
    )?;
    if let Some(index) = correction {
        history[index].superseded_by = Some(entry.id.clone());
    }
    history.push(entry.clone());
    if inserted != 1
        || load_history(&tx, &activity, now)? != history
        || load_activity(&tx, owner_uid, &activity.id)? != activity
    {
        return corrupt("object-state append did not preserve the immutable ledger and Activity");
    }
    tx.commit()?;
    Ok(entry)
}

pub(super) fn list(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    reference: Option<&str>,
    limit: usize,
) -> Result<Vec<ObjectStateEntry>, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    let reference = reference.map(canonical_reference).transpose()?;
    let limit = usize::try_from(list_limit(limit)?)
        .map_err(|_| ActivityError::Invalid("list limit is not representable".into()))?;
    let mut conn = service.lock()?;
    let tx = conn.transaction()?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    let entries = load_history(&tx, &activity, Utc::now())?
        .into_iter()
        .rev()
        .filter(|entry| {
            reference
                .as_ref()
                .is_none_or(|reference| &entry.draft.reference == reference)
        })
        .take(limit)
        .collect();
    tx.commit()?;
    Ok(entries)
}

fn require_attached(activity: &Activity, reference: &str) -> Result<(), ActivityError> {
    if !activity
        .resources
        .iter()
        .any(|resource| resource.reference == reference)
    {
        return Err(ActivityError::Invalid(
            "object-state subject and relation targets must already be attached to the Activity"
                .into(),
        ));
    }
    Ok(())
}

fn receipt_id(draft: &ObjectStateDraft) -> Option<&str> {
    match &draft.content {
        ObjectStateContent::AppReport { receipt_id } => Some(receipt_id),
        _ => None,
    }
}

fn linked_report(
    conn: &Connection,
    activity: &Activity,
    draft: &ObjectStateDraft,
) -> Result<Option<ReceiptReport>, ActivityError> {
    let Some(id) = receipt_id(draft) else {
        return Ok(None);
    };
    let receipt = load_receipt(conn, activity.owner_uid, id)?.ok_or_else(|| {
        ActivityError::Invalid(
            "App report must link an existing receipt in the owned Activity".into(),
        )
    })?;
    let object = crate::objects::parse_reference(&draft.reference)
        .map_err(|error| ActivityError::Invalid(error.to_string()))?;
    if receipt.activity_id != activity.id || receipt.report.app_id != object.app_id {
        return Err(ActivityError::Invalid(
            "App report receipt must match the owned Activity and reference's App".into(),
        ));
    }
    Ok(Some(receipt.report))
}

fn load_history(
    conn: &Connection,
    activity: &Activity,
    now: DateTime<Utc>,
) -> Result<Vec<ObjectStateEntry>, ActivityError> {
    let scan_limit = i64::try_from(MAX_ENTRIES_PER_ACTIVITY + 1).map_err(|_| {
        ActivityError::Invalid("object-state scan limit is not representable".into())
    })?;
    let mut statement = conn.prepare(&format!(
        "{SELECT_ENTRY} WHERE owner_uid = ?1 AND activity_id = ?2
         ORDER BY sequence LIMIT ?3"
    ))?;
    let rows = statement.query_map(
        params![activity.owner_uid, activity.id, scan_limit],
        ObjectStateRow::from_row,
    )?;
    let mut history = Vec::new();
    let mut last_sequence = 0;
    for row in rows {
        let row = row?;
        if row.sequence <= last_sequence {
            return corrupt("object-state sequence is not positive and strictly increasing");
        }
        last_sequence = row.sequence;
        history.push(row.into_entry(conn, activity, now)?);
        if history.len() > MAX_ENTRIES_PER_ACTIVITY {
            return corrupt("object-state ledger exceeds the per-Activity limit");
        }
    }
    // The entire ledger is bounded. Validate edges before applying a view's
    // filter or limit so a missing/cyclic correction cannot disappear from view.
    let mut by_id = HashMap::new();
    for (index, entry) in history.iter().enumerate() {
        if by_id.insert(entry.id.as_str(), index).is_some() {
            return corrupt("duplicate object-state id in Activity history");
        }
    }
    let mut successors = vec![None; history.len()];
    for (index, entry) in history.iter().enumerate() {
        if let Some(supersedes) = &entry.draft.supersedes {
            let previous_index = *by_id.get(supersedes.as_str()).ok_or_else(|| {
                ActivityError::Corrupt(
                    "object-state correction references a missing or foreign entry".into(),
                )
            })?;
            let previous = &history[previous_index];
            if previous_index >= index
                || previous.draft.reference != entry.draft.reference
                || matches!(previous.draft.content, ObjectStateContent::Retracted { .. })
            {
                return corrupt("object-state supersession edge is invalid");
            }
            if successors[previous_index]
                .replace(entry.id.clone())
                .is_some()
            {
                return corrupt("an object-state entry has multiple corrections");
            }
        }
    }
    for (entry, successor) in history.iter_mut().zip(successors) {
        entry.superseded_by = successor;
    }
    Ok(history)
}

struct ObjectStateRow {
    sequence: i64,
    id: String,
    activity_id: String,
    owner_uid: u32,
    reference: String,
    recorded_at: String,
    source: String,
    draft_json: String,
    receipt_id: Option<String>,
    supersedes: Option<String>,
}

impl ObjectStateRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            sequence: row.get(0)?,
            id: row.get(1)?,
            activity_id: row.get(2)?,
            owner_uid: row.get(3)?,
            reference: row.get(4)?,
            recorded_at: row.get(5)?,
            source: row.get(6)?,
            draft_json: row.get(7)?,
            receipt_id: row.get(8)?,
            supersedes: row.get(9)?,
        })
    }

    fn into_entry(
        self,
        conn: &Connection,
        activity: &Activity,
        now: DateTime<Utc>,
    ) -> Result<ObjectStateEntry, ActivityError> {
        let draft: ObjectStateDraft = serde_json::from_str(&self.draft_json).map_err(|error| {
            ActivityError::Corrupt(format!("invalid object-state JSON: {error}"))
        })?;
        if draft.clone().canonicalized().map_err(stored_error)? != draft
            || self.id != draft.id
            || self.activity_id != activity.id
            || self.owner_uid != activity.owner_uid
            || self.reference != draft.reference
            || self.supersedes != draft.supersedes
            || self.receipt_id.as_deref() != receipt_id(&draft)
        {
            return corrupt("object-state columns do not match the canonical draft and Activity");
        }
        if self.source != "caller_reported" {
            return corrupt("object-state source is not caller_reported");
        }
        parse_timestamp(&self.recorded_at)?;
        let receipt = linked_report(conn, activity, &draft).map_err(stored_error)?;
        Ok(ObjectStateEntry {
            id: self.id,
            activity_id: self.activity_id,
            owner_uid: self.owner_uid,
            recorded_at: self.recorded_at,
            source: ObjectStateSource::CallerReported,
            validity: draft.validity_at(now).map_err(stored_error)?,
            draft,
            receipt,
            superseded_by: None,
        })
    }
}

pub(super) fn validate_schema(conn: &Connection) -> Result<(), ActivityError> {
    conn.prepare(&format!("{SELECT_ENTRY} LIMIT 0"))?;
    for (table, columns, unique) in [
        ("activities", &["id"][..], true),
        ("activities", &["owner_uid", "id"][..], true),
        ("activity_receipts", &["owner_uid", "id"][..], true),
        (
            "activity_receipts",
            &["owner_uid", "activity_id", "id"][..],
            true,
        ),
        ("activity_object_state", &["owner_uid", "id"][..], true),
        (
            "activity_object_state",
            &["owner_uid", "activity_id", "reference", "id"][..],
            true,
        ),
        (
            "activity_object_state",
            &["owner_uid", "supersedes"][..],
            true,
        ),
        (
            "activity_object_state",
            &["owner_uid", "activity_id", "sequence"][..],
            false,
        ),
        (
            "activity_object_state",
            &["owner_uid", "activity_id", "reference", "sequence"][..],
            false,
        ),
    ] {
        require_index(conn, table, columns, unique)?;
    }
    require_foreign_keys(
        conn,
        "activity_receipts",
        &[(
            "activities",
            &["owner_uid", "activity_id"],
            &["owner_uid", "id"],
        )],
    )?;
    require_foreign_keys(
        conn,
        "activity_object_state",
        &[
            (
                "activities",
                &["owner_uid", "activity_id"],
                &["owner_uid", "id"],
            ),
            (
                "activity_receipts",
                &["owner_uid", "activity_id", "receipt_id"],
                &["owner_uid", "activity_id", "id"],
            ),
            (
                "activity_object_state",
                &["owner_uid", "activity_id", "reference", "supersedes"],
                &["owner_uid", "activity_id", "reference", "id"],
            ),
        ],
    )
}

fn require_index(
    conn: &Connection,
    table: &str,
    columns: &[&str],
    unique: bool,
) -> Result<(), ActivityError> {
    let mut indexes = conn
        .prepare("SELECT name FROM pragma_index_list(?1) WHERE \"unique\" = ?2 AND partial = 0")?;
    let names = indexes.query_map(params![table, unique], |row| row.get::<_, String>(0))?;
    for name in names {
        let mut info = conn.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?;
        let actual = info
            .query_map([name?], |row| row.get::<_, Option<String>>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if actual
            .iter()
            .map(|value| value.as_deref())
            .eq(columns.iter().copied().map(Some))
        {
            return Ok(());
        }
    }
    corrupt(format!(
        "missing or invalid {table} index for {}",
        columns.join(", ")
    ))
}

fn require_foreign_keys(
    conn: &Connection,
    table: &str,
    relationships: &[(&str, &[&str], &[&str])],
) -> Result<(), ActivityError> {
    let mut statement = conn.prepare(
        "SELECT \"table\", seq, \"from\", \"to\", on_update, on_delete
         FROM pragma_foreign_key_list(?1)",
    )?;
    let mut actual = statement
        .query_map([table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut expected: Vec<_> = relationships
        .iter()
        .flat_map(|(target, from, to)| {
            (0_i64..)
                .zip(from.iter().zip(*to))
                .map(move |(seq, (from, to))| {
                    (
                        (*target).to_string(),
                        seq,
                        (*from).to_string(),
                        (*to).to_string(),
                        "NO ACTION".to_string(),
                        "NO ACTION".to_string(),
                    )
                })
        })
        .collect();
    actual.sort();
    expected.sort();
    if actual != expected {
        return corrupt(format!(
            "missing or invalid {table} ownership/history foreign keys"
        ));
    }
    Ok(())
}

pub(super) fn validate_migration_records(conn: &Connection) -> Result<(), ActivityError> {
    let mut activities = conn.prepare(SELECT_ACTIVITY)?;
    for row in activities.query_map([], ActivityRow::from_row)? {
        row?.into_activity().map_err(stored_error)?;
    }
    let mut receipts = conn.prepare(SELECT_RECEIPT)?;
    for row in receipts.query_map([], ReceiptRow::from_row)? {
        row?.into_receipt().map_err(stored_error)?;
    }
    Ok(())
}

fn stored_error(error: ActivityError) -> ActivityError {
    match error {
        ActivityError::Database(_) | ActivityError::Io(_) | ActivityError::Corrupt(_) => error,
        _ => ActivityError::Corrupt(error.to_string()),
    }
}

fn corrupt<T>(message: impl Into<String>) -> Result<T, ActivityError> {
    Err(ActivityError::Corrupt(message.into()))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/sqlite/object_state.rs"
    ));
}
