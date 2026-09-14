//! Owner-scoped Activity scheduling policy. This provider controls only
//! pending admission order and never creates execution authority.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{load_activity, parse_id, parse_timestamp, timestamp, SqliteActivityService};
use crate::activities::{
    Activity, ActivityError, ActivitySchedulingPolicy, ActivitySchedulingPriority, ActivityState,
};

pub(super) const MIGRATE_TO_V7: &str = r#"
CREATE TABLE activity_scheduling_policies (
    activity_id  TEXT NOT NULL,
    owner_uid    INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    revision     INTEGER NOT NULL CHECK(revision > 0),
    priority     TEXT NOT NULL CHECK(priority IN ('foreground', 'standard', 'background')),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    PRIMARY KEY(owner_uid, activity_id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id)
);
"#;

const SELECT_POLICY: &str = "SELECT activity_id, owner_uid, revision, priority,
    created_at, updated_at FROM activity_scheduling_policies";

pub(super) fn get(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
) -> Result<Option<ActivitySchedulingPolicy>, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    let mut conn = service.lock()?;
    let tx = conn.transaction()?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    let policy = load_policy(&tx, &activity)?;
    tx.commit()?;
    Ok(policy)
}

pub(super) fn set(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    expected_revision: Option<u64>,
    priority: ActivitySchedulingPriority,
) -> Result<ActivitySchedulingPolicy, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    if let Some(revision) = expected_revision {
        revision_sql(revision)?;
    }
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    require_configurable(&activity)?;
    let existing = load_policy(&tx, &activity)?;
    let previous_priority = existing.as_ref().map(|policy| policy.priority);
    let now = timestamp();
    let policy = match (existing, expected_revision) {
        (None, None) => ActivitySchedulingPolicy {
            activity_id: activity_id.clone(),
            owner_uid,
            revision: 1,
            priority,
            created_at: now.clone(),
            updated_at: now,
        },
        (Some(mut policy), Some(revision)) => {
            require_revision(&policy, revision)?;
            policy.revision = next_revision(policy.revision)?;
            policy.priority = priority;
            policy.updated_at = now.max(policy.updated_at);
            policy
        }
        (Some(_), None) => {
            return Err(ActivityError::Conflict(
                "scheduling policy already exists; supply expected_revision to update".into(),
            ));
        }
        (None, Some(_)) => {
            return Err(ActivityError::Conflict(
                "scheduling policy does not exist; omit expected_revision to create".into(),
            ));
        }
    };
    let changed = if let Some(revision) = expected_revision {
        tx.execute(
            "UPDATE activity_scheduling_policies
             SET revision = ?1, priority = ?2, updated_at = ?3
             WHERE owner_uid = ?4 AND activity_id = ?5 AND revision = ?6",
            params![
                revision_sql(policy.revision)?,
                priority_text(policy.priority),
                policy.updated_at,
                i64::from(owner_uid),
                policy.activity_id,
                revision_sql(revision)?,
            ],
        )?
    } else {
        tx.execute(
            "INSERT INTO activity_scheduling_policies (
                activity_id, owner_uid, revision, priority, created_at, updated_at
             ) VALUES (?1, ?2, 1, ?3, ?4, ?4)",
            params![
                policy.activity_id,
                i64::from(owner_uid),
                priority_text(policy.priority),
                policy.created_at,
            ],
        )?
    };
    verify_write(&tx, &activity, &policy, changed)?;
    super::continuity::bump_if_changed(
        &tx,
        owner_uid,
        &activity_id,
        previous_priority != Some(policy.priority),
    )?;
    tx.commit()?;
    Ok(policy)
}

fn require_configurable(activity: &Activity) -> Result<(), ActivityError> {
    if !matches!(
        activity.state,
        ActivityState::Active | ActivityState::Paused
    ) {
        return Err(ActivityError::Conflict(
            "reopen a terminal Activity before configuring its scheduling policy".into(),
        ));
    }
    Ok(())
}

fn require_revision(policy: &ActivitySchedulingPolicy, expected: u64) -> Result<(), ActivityError> {
    if policy.revision != expected {
        return Err(ActivityError::Conflict(
            "scheduling policy revision changed".into(),
        ));
    }
    Ok(())
}

fn revision_sql(revision: u64) -> Result<i64, ActivityError> {
    if revision == 0 {
        return Err(ActivityError::Invalid(
            "scheduling policy revision must be positive".into(),
        ));
    }
    i64::try_from(revision).map_err(|_| {
        ActivityError::Invalid("scheduling policy revision is not representable".into())
    })
}

fn next_revision(revision: u64) -> Result<u64, ActivityError> {
    let exhausted = || ActivityError::Conflict("scheduling policy revision is exhausted".into());
    let next = revision.checked_add(1).ok_or_else(exhausted)?;
    i64::try_from(next).map_err(|_| exhausted())?;
    Ok(next)
}

fn priority_text(priority: ActivitySchedulingPriority) -> &'static str {
    match priority {
        ActivitySchedulingPriority::Foreground => "foreground",
        ActivitySchedulingPriority::Standard => "standard",
        ActivitySchedulingPriority::Background => "background",
    }
}

fn parse_priority(value: &str) -> Result<ActivitySchedulingPriority, ActivityError> {
    match value {
        "foreground" => Ok(ActivitySchedulingPriority::Foreground),
        "standard" => Ok(ActivitySchedulingPriority::Standard),
        "background" => Ok(ActivitySchedulingPriority::Background),
        _ => Err(corrupt("invalid Activity scheduling priority")),
    }
}

fn verify_write(
    conn: &Connection,
    activity: &Activity,
    expected: &ActivitySchedulingPolicy,
    changed: usize,
) -> Result<(), ActivityError> {
    if changed != 1
        || load_policy(conn, activity)?.as_ref() != Some(expected)
        || load_activity(conn, activity.owner_uid, &activity.id)? != *activity
    {
        return Err(corrupt(
            "scheduling policy write did not preserve the policy and Activity",
        ));
    }
    Ok(())
}

fn load_policy(
    conn: &Connection,
    activity: &Activity,
) -> Result<Option<ActivitySchedulingPolicy>, ActivityError> {
    conn.query_row(
        &format!("{SELECT_POLICY} WHERE owner_uid = ?1 AND activity_id = ?2"),
        params![i64::from(activity.owner_uid), activity.id],
        PolicyRow::from_row,
    )
    .optional()?
    .map(|row| row.into_policy(activity))
    .transpose()
}

pub(super) fn portable(
    conn: &Connection,
    activity: &Activity,
) -> Result<Option<ActivitySchedulingPolicy>, ActivityError> {
    load_policy(conn, activity)
}

struct PolicyRow {
    activity_id: String,
    owner_uid: i64,
    revision: i64,
    priority: String,
    created_at: String,
    updated_at: String,
}

impl PolicyRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            activity_id: row.get(0)?,
            owner_uid: row.get(1)?,
            revision: row.get(2)?,
            priority: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    }

    fn into_policy(self, activity: &Activity) -> Result<ActivitySchedulingPolicy, ActivityError> {
        let owner_uid = u32::try_from(self.owner_uid)
            .map_err(|_| corrupt("invalid scheduling policy owner"))?;
        if self.activity_id != activity.id || owner_uid != activity.owner_uid {
            return Err(corrupt(
                "scheduling policy does not match its owned Activity",
            ));
        }
        let revision = u64::try_from(self.revision)
            .map_err(|_| corrupt("negative scheduling policy revision"))?;
        if revision == 0 {
            return Err(corrupt("scheduling policy revision is zero"));
        }
        let created_at = parse_timestamp(&self.created_at)?;
        let updated_at = parse_timestamp(&self.updated_at)?;
        if updated_at < created_at {
            return Err(corrupt("scheduling policy updated_at precedes created_at"));
        }
        Ok(ActivitySchedulingPolicy {
            activity_id: self.activity_id,
            owner_uid,
            revision,
            priority: parse_priority(&self.priority)?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

pub(super) fn validate_schema(conn: &Connection) -> Result<(), ActivityError> {
    conn.prepare(&format!("{SELECT_POLICY} LIMIT 0"))?;
    let unique: bool = conn.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM pragma_index_list('activity_scheduling_policies') AS idx
            WHERE idx.\"unique\" = 1 AND idx.partial = 0
              AND (SELECT COUNT(*) FROM pragma_index_info(idx.name)) = 2
              AND EXISTS(SELECT 1 FROM pragma_index_info(idx.name) WHERE seqno = 0 AND name = 'owner_uid')
              AND EXISTS(SELECT 1 FROM pragma_index_info(idx.name) WHERE seqno = 1 AND name = 'activity_id')
         )",
        [],
        |row| row.get(0),
    )?;
    if !unique {
        return Err(corrupt(
            "missing owner-scoped scheduling policy identity constraint",
        ));
    }
    let mut statement = conn.prepare(
        "SELECT \"table\", seq, \"from\", \"to\", on_update, on_delete
         FROM pragma_foreign_key_list('activity_scheduling_policies') ORDER BY seq",
    )?;
    let actual = statement
        .query_map([], |row| {
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
    let expected: Vec<_> = [
        (0_i64, "owner_uid", "owner_uid"),
        (1_i64, "activity_id", "id"),
    ]
    .into_iter()
    .map(|(seq, from, to)| {
        (
            "activities".to_string(),
            seq,
            from.to_string(),
            to.to_string(),
            "NO ACTION".to_string(),
            "NO ACTION".to_string(),
        )
    })
    .collect();
    if actual != expected {
        return Err(corrupt(
            "missing or invalid scheduling policy ownership foreign key",
        ));
    }
    Ok(())
}

fn corrupt(message: impl Into<String>) -> ActivityError {
    ActivityError::Corrupt(message.into())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/sqlite/scheduling_policy.rs"
    ));
}
