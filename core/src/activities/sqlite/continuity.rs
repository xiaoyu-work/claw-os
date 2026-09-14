//! Schema-8 Activity continuity identity and atomic import/export.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{load_activity, timestamp, SqliteActivityService};
use crate::activities::continuity::portable_references;
use crate::activities::{
    Activity, ActivityContinuityDocument, ActivityContinuityImport, ActivityContinuityLineage,
    ActivityError, ActivityExecutionPlacement, ActivitySchedulingPolicy, ActivityState,
    PortableActivityIntent, PortableActivityRules, PortableExecutionLimits,
    PortableSchedulingPreference, MAX_ACTIVITIES_PER_OWNER,
};

pub(super) const MIGRATE_TO_V8: &str = r#"
CREATE TABLE activity_continuity (
    activity_id   TEXT NOT NULL,
    owner_uid     INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    continuity_id TEXT NOT NULL,
    revision      INTEGER NOT NULL CHECK(revision > 0),
    PRIMARY KEY(owner_uid, activity_id),
    UNIQUE(owner_uid, continuity_id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id)
);
"#;

const SELECT_LINEAGE: &str =
    "SELECT activity_id, owner_uid, continuity_id, revision FROM activity_continuity";

pub(super) fn migrate(conn: &Connection) -> Result<(), ActivityError> {
    conn.execute_batch(MIGRATE_TO_V8)?;
    let records = {
        let mut statement =
            conn.prepare("SELECT id, owner_uid FROM activities ORDER BY owner_uid, id")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (activity_id, owner_uid) in records {
        let owner_uid = u32::try_from(owner_uid)
            .map_err(|_| corrupt("legacy Activity owner UID is invalid"))?;
        let canonical = super::parse_id(&activity_id)
            .map_err(|_| corrupt("legacy Activity UUID is invalid"))?;
        if canonical != activity_id {
            return Err(corrupt("legacy Activity UUID is not canonical"));
        }
        insert_lineage(
            conn,
            owner_uid,
            &activity_id,
            &legacy_continuity_id(&activity_id),
            1,
        )?;
    }
    Ok(())
}

pub(super) fn insert_new(
    conn: &Connection,
    owner_uid: u32,
    activity_id: &str,
) -> Result<(), ActivityError> {
    insert_lineage(
        conn,
        owner_uid,
        activity_id,
        &uuid::Uuid::new_v4().to_string(),
        1,
    )
}

pub(super) fn bump_if_changed(
    conn: &Connection,
    owner_uid: u32,
    activity_id: &str,
    changed: bool,
) -> Result<(), ActivityError> {
    if !changed {
        return Ok(());
    }
    let lineage = load_lineage(conn, owner_uid, activity_id)?;
    let next = lineage
        .revision
        .checked_add(1)
        .filter(|revision| i64::try_from(*revision).is_ok())
        .ok_or_else(|| ActivityError::Conflict("continuity revision is exhausted".into()))?;
    let updated = conn.execute(
        "UPDATE activity_continuity SET revision = ?1
         WHERE owner_uid = ?2 AND activity_id = ?3 AND revision = ?4",
        params![
            i64::try_from(next).map_err(|_| corrupt("continuity revision overflow"))?,
            i64::from(owner_uid),
            activity_id,
            i64::try_from(lineage.revision).map_err(|_| corrupt("continuity revision overflow"))?,
        ],
    )?;
    if updated != 1 {
        return Err(ActivityError::Conflict(
            "continuity revision changed concurrently".into(),
        ));
    }
    Ok(())
}

pub(super) fn portable_fields_changed(
    before: &Activity,
    after: &Activity,
) -> Result<bool, ActivityError> {
    Ok(before.title != after.title
        || before.goal != after.goal
        || before.completion_criteria != after.completion_criteria
        || before.boundaries != after.boundaries
        || portable_references(&before.resources)? != portable_references(&after.resources)?)
}

pub(super) fn export(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
) -> Result<ActivityContinuityDocument, ActivityError> {
    let activity_id = super::parse_id(activity_id)?;
    let mut conn = service.lock()?;
    let tx = conn.transaction()?;
    let document = export_from_conn(&tx, owner_uid, &activity_id)?;
    tx.commit()?;
    Ok(document)
}

pub(super) fn import(
    service: &SqliteActivityService,
    owner_uid: u32,
    placement: ActivityExecutionPlacement,
    document: ActivityContinuityDocument,
) -> Result<ActivityContinuityImport, ActivityError> {
    document.validate()?;
    let draft = document.draft()?.normalized()?;
    match placement {
        ActivityExecutionPlacement::Local => {}
    }

    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM activities WHERE owner_uid = ?1",
        [i64::from(owner_uid)],
        |row| row.get(0),
    )?;
    if count >= MAX_ACTIVITIES_PER_OWNER {
        return Err(ActivityError::LimitReached);
    }
    let duplicate: bool = tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM activity_continuity
            WHERE owner_uid = ?1 AND continuity_id = ?2
         )",
        params![i64::from(owner_uid), document.lineage.id],
        |row| row.get(0),
    )?;
    if duplicate {
        return Err(ActivityError::Conflict(
            "continuity identity already exists for this owner".into(),
        ));
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
        state: ActivityState::Paused,
        completion_note: None,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    tx.execute(
        "INSERT INTO activities (
            id, owner_uid, title, goal, completion_criteria, boundaries,
            resources_json, state, completion_note, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'paused', NULL, ?8, ?8)",
        params![
            activity.id,
            i64::from(owner_uid),
            activity.title,
            activity.goal,
            activity.completion_criteria,
            activity.boundaries,
            serde_json::to_string(&activity.resources)?,
            activity.created_at,
        ],
    )?;
    insert_lineage(
        &tx,
        owner_uid,
        &activity.id,
        &document.lineage.id,
        document.lineage.revision,
    )?;
    if let Some(limits) = &document.rules.execution_limits {
        tx.execute(
            "INSERT INTO activity_execution_limits (
                activity_id, owner_uid, revision, enabled, max_attempts, max_turns_per_attempt,
                expires_at, used_attempts, created_at, updated_at
             ) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, 0, ?7, ?7)",
            params![
                activity.id,
                i64::from(owner_uid),
                i64::from(limits.enabled),
                i64::from(limits.max_attempts),
                i64::from(limits.max_turns_per_attempt),
                limits.expires_at,
                now,
            ],
        )?;
    }
    if let Some(scheduling) = &document.rules.scheduling {
        tx.execute(
            "INSERT INTO activity_scheduling_policies (
                activity_id, owner_uid, revision, priority, created_at, updated_at
             ) VALUES (?1, ?2, 1, ?3, ?4, ?4)",
            params![
                activity.id,
                i64::from(owner_uid),
                priority_text(scheduling.priority),
                now,
            ],
        )?;
    }
    if load_activity(&tx, owner_uid, &activity.id)? != activity
        || export_from_conn(&tx, owner_uid, &activity.id)? != document
    {
        return Err(corrupt(
            "continuity import did not preserve the exact paused intent and rules",
        ));
    }
    tx.commit()?;
    Ok(ActivityContinuityImport {
        activity,
        continuity_id: document.lineage.id,
        continuity_revision: document.lineage.revision,
        placement,
    })
}

fn export_from_conn(
    conn: &Connection,
    owner_uid: u32,
    activity_id: &str,
) -> Result<ActivityContinuityDocument, ActivityError> {
    let activity = load_activity(conn, owner_uid, activity_id)?;
    let lineage = load_lineage(conn, owner_uid, activity_id)?;
    let execution_limits =
        super::execution_limits::portable(conn, &activity)?.map(|(enabled, limits)| {
            PortableExecutionLimits {
                enabled,
                max_attempts: limits.max_attempts,
                max_turns_per_attempt: limits.max_turns_per_attempt,
                expires_at: limits.expires_at,
            }
        });
    let scheduling = super::scheduling_policy::portable(conn, &activity)?.map(
        |policy: ActivitySchedulingPolicy| PortableSchedulingPreference {
            priority: policy.priority,
        },
    );
    ActivityContinuityDocument::build(
        lineage,
        PortableActivityIntent {
            title: activity.title,
            goal: activity.goal,
            completion_criteria: activity.completion_criteria,
            boundaries: activity.boundaries,
        },
        portable_references(&activity.resources)?,
        PortableActivityRules {
            execution_limits,
            scheduling,
        },
    )
}

fn insert_lineage(
    conn: &Connection,
    owner_uid: u32,
    activity_id: &str,
    continuity_id: &str,
    revision: u64,
) -> Result<(), ActivityError> {
    let activity_id = super::parse_id(activity_id)?;
    let continuity_id = super::parse_id(continuity_id)?;
    let revision = i64::try_from(revision)
        .map_err(|_| ActivityError::Invalid("continuity revision is not representable".into()))?;
    if revision <= 0 {
        return Err(ActivityError::Invalid(
            "continuity revision must be positive".into(),
        ));
    }
    conn.execute(
        "INSERT INTO activity_continuity (
            activity_id, owner_uid, continuity_id, revision
         ) VALUES (?1, ?2, ?3, ?4)",
        params![activity_id, i64::from(owner_uid), continuity_id, revision],
    )?;
    Ok(())
}

fn load_lineage(
    conn: &Connection,
    owner_uid: u32,
    activity_id: &str,
) -> Result<ActivityContinuityLineage, ActivityError> {
    conn.query_row(
        &format!("{SELECT_LINEAGE} WHERE owner_uid = ?1 AND activity_id = ?2"),
        params![i64::from(owner_uid), activity_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    )
    .optional()?
    .ok_or_else(|| corrupt("Activity has no continuity identity"))
    .and_then(
        |(stored_activity_id, stored_owner_uid, continuity_id, revision)| {
            if stored_activity_id != activity_id || stored_owner_uid != i64::from(owner_uid) {
                return Err(corrupt("continuity identity does not match its Activity"));
            }
            let canonical = super::parse_id(&continuity_id)
                .map_err(|_| corrupt("continuity identity is not a UUID"))?;
            if canonical != continuity_id {
                return Err(corrupt("continuity identity is not canonical"));
            }
            let revision = u64::try_from(revision)
                .ok()
                .filter(|revision| *revision > 0)
                .ok_or_else(|| corrupt("continuity revision is invalid"))?;
            Ok(ActivityContinuityLineage {
                id: continuity_id,
                revision,
            })
        },
    )
}

fn legacy_continuity_id(activity_id: &str) -> String {
    let material = format!("claw-os/activity-continuity/legacy-id/v1\0{activity_id}");
    let mut bytes = crate::crypto::sha256_bytes(material.as_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes[..16].try_into().expect("SHA-256 UUID prefix")).to_string()
}

fn priority_text(priority: crate::activities::ActivitySchedulingPriority) -> &'static str {
    match priority {
        crate::activities::ActivitySchedulingPriority::Foreground => "foreground",
        crate::activities::ActivitySchedulingPriority::Standard => "standard",
        crate::activities::ActivitySchedulingPriority::Background => "background",
    }
}

pub(super) fn validate_schema(conn: &Connection) -> Result<(), ActivityError> {
    conn.prepare(&format!("{SELECT_LINEAGE} LIMIT 0"))?;
    let unique: bool = conn.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM pragma_index_list('activity_continuity') AS idx
            WHERE idx.\"unique\" = 1 AND idx.partial = 0
              AND (SELECT COUNT(*) FROM pragma_index_info(idx.name)) = 2
              AND EXISTS(SELECT 1 FROM pragma_index_info(idx.name)
                         WHERE seqno = 0 AND name = 'owner_uid')
              AND EXISTS(SELECT 1 FROM pragma_index_info(idx.name)
                         WHERE seqno = 1 AND name = 'continuity_id')
         )",
        [],
        |row| row.get(0),
    )?;
    if !unique {
        return Err(corrupt(
            "missing owner-scoped continuity identity uniqueness constraint",
        ));
    }
    let missing: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM activities AS a
            LEFT JOIN activity_continuity AS c
              ON c.owner_uid = a.owner_uid AND c.activity_id = a.id
            WHERE c.activity_id IS NULL
         )",
        [],
        |row| row.get(0),
    )?;
    if missing {
        return Err(corrupt("an Activity is missing its continuity identity"));
    }
    let mut statement = conn.prepare(SELECT_LINEAGE)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (activity_id, owner_uid, continuity_id, revision) = row?;
        u32::try_from(owner_uid).map_err(|_| corrupt("continuity owner UID is invalid"))?;
        for (name, id) in [
            ("Activity", activity_id.as_str()),
            ("continuity", continuity_id.as_str()),
        ] {
            let canonical =
                super::parse_id(id).map_err(|_| corrupt(format!("{name} UUID is invalid")))?;
            if canonical != id {
                return Err(corrupt(format!("{name} UUID is not canonical")));
            }
        }
        if revision <= 0 {
            return Err(corrupt("continuity revision is invalid"));
        }
    }
    let mut foreign_keys = conn.prepare(
        "SELECT \"table\", seq, \"from\", \"to\", on_update, on_delete
         FROM pragma_foreign_key_list('activity_continuity') ORDER BY seq",
    )?;
    let actual = foreign_keys
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
    let expected = vec![
        (
            "activities".to_string(),
            0,
            "owner_uid".to_string(),
            "owner_uid".to_string(),
            "NO ACTION".to_string(),
            "NO ACTION".to_string(),
        ),
        (
            "activities".to_string(),
            1,
            "activity_id".to_string(),
            "id".to_string(),
            "NO ACTION".to_string(),
            "NO ACTION".to_string(),
        ),
    ];
    if actual != expected {
        return Err(corrupt(
            "missing or invalid continuity ownership foreign key",
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
        "/test/unit/activities/sqlite/continuity.rs"
    ));
}
