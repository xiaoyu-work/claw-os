//! Owner-scoped storage for capability constraints. No grants, approval state,
//! peer authentication or execution accounting live in this provider.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{load_activity, parse_id, parse_timestamp, timestamp, SqliteActivityService};
use crate::activities::capability_policy::MAX_POLICY_BYTES;
use crate::activities::{
    Activity, ActivityCapabilityPolicy, ActivityCapabilityRule, ActivityError, ActivityState,
    CapabilityPolicyDraft,
};

pub(super) const MIGRATE_TO_V5: &str = r#"
CREATE TABLE activity_capability_policies (
    activity_id  TEXT NOT NULL,
    owner_uid    INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    revision     INTEGER NOT NULL CHECK(revision > 0),
    enabled      INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    rules_json   TEXT NOT NULL CHECK(length(CAST(rules_json AS BLOB)) <= 16384),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    PRIMARY KEY(owner_uid, activity_id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id)
);
"#;

const SELECT_POLICY: &str = "SELECT activity_id, owner_uid, revision, enabled,
    rules_json, created_at, updated_at FROM activity_capability_policies";

pub(super) fn get(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
) -> Result<Option<ActivityCapabilityPolicy>, ActivityError> {
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
    draft: CapabilityPolicyDraft,
) -> Result<ActivityCapabilityPolicy, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    let draft = draft.canonicalized()?;
    if let Some(revision) = expected_revision {
        revision_sql(revision)?;
    }
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    let existing = load_policy(&tx, &activity)?;
    require_configurable(&activity)?;
    let now = timestamp();
    let policy = match (existing, expected_revision) {
        (None, None) => ActivityCapabilityPolicy {
            activity_id,
            owner_uid,
            revision: 1,
            enabled: true,
            rules: draft.rules,
            created_at: now.clone(),
            updated_at: now,
        },
        (Some(mut policy), Some(revision)) => {
            require_revision(&policy, revision)?;
            policy.revision = next_revision(policy.revision)?;
            policy.rules = draft.rules;
            policy.updated_at = now.max(policy.updated_at);
            policy
        }
        (Some(_), None) => {
            return Err(ActivityError::Conflict(
                "capability policy already exists; supply expected_revision to update".into(),
            ));
        }
        (None, Some(_)) => {
            return Err(ActivityError::Conflict(
                "capability policy does not exist; omit expected_revision to create".into(),
            ));
        }
    };
    let changed = if let Some(revision) = expected_revision {
        tx.execute(
            "UPDATE activity_capability_policies SET revision = ?1, rules_json = ?2, updated_at = ?3
             WHERE owner_uid = ?4 AND activity_id = ?5 AND revision = ?6",
            params![
                revision_sql(policy.revision)?, serde_json::to_string(&policy.rules)?,
                policy.updated_at, i64::from(owner_uid), policy.activity_id, revision_sql(revision)?,
            ],
        )?
    } else {
        tx.execute(
            "INSERT INTO activity_capability_policies (
                activity_id, owner_uid, revision, enabled, rules_json, created_at, updated_at
             ) VALUES (?1, ?2, 1, 1, ?3, ?4, ?4)",
            params![
                policy.activity_id,
                i64::from(owner_uid),
                serde_json::to_string(&policy.rules)?,
                policy.created_at,
            ],
        )?
    };
    verify_write(&tx, &activity, &policy, changed)?;
    tx.commit()?;
    Ok(policy)
}

pub(super) fn set_enabled(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    expected_revision: u64,
    enabled: bool,
) -> Result<ActivityCapabilityPolicy, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    revision_sql(expected_revision)?;
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    let mut policy = load_policy(&tx, &activity)?.ok_or(ActivityError::NotFound)?;
    require_revision(&policy, expected_revision)?;
    if enabled {
        require_configurable(&activity)?;
    }
    policy.revision = next_revision(policy.revision)?;
    policy.enabled = enabled;
    policy.updated_at = timestamp().max(policy.updated_at);
    let changed = tx.execute(
        "UPDATE activity_capability_policies SET revision = ?1, enabled = ?2, updated_at = ?3
         WHERE owner_uid = ?4 AND activity_id = ?5 AND revision = ?6",
        params![
            revision_sql(policy.revision)?,
            i64::from(enabled),
            policy.updated_at,
            i64::from(owner_uid),
            policy.activity_id,
            revision_sql(expected_revision)?,
        ],
    )?;
    verify_write(&tx, &activity, &policy, changed)?;
    tx.commit()?;
    Ok(policy)
}

fn require_configurable(activity: &Activity) -> Result<(), ActivityError> {
    if !matches!(
        activity.state,
        ActivityState::Active | ActivityState::Paused
    ) {
        return Err(ActivityError::Conflict(
            "reopen a terminal Activity before configuring or enabling its capability policy"
                .into(),
        ));
    }
    Ok(())
}

fn require_revision(policy: &ActivityCapabilityPolicy, expected: u64) -> Result<(), ActivityError> {
    if policy.revision != expected {
        return Err(ActivityError::Conflict(
            "capability policy revision changed".into(),
        ));
    }
    Ok(())
}

fn revision_sql(revision: u64) -> Result<i64, ActivityError> {
    if revision == 0 {
        return Err(ActivityError::Invalid(
            "capability policy revision must be positive".into(),
        ));
    }
    i64::try_from(revision).map_err(|_| {
        ActivityError::Invalid("capability policy revision is not representable".into())
    })
}

fn next_revision(revision: u64) -> Result<u64, ActivityError> {
    let exhausted = || ActivityError::Conflict("capability policy revision is exhausted".into());
    let next = revision.checked_add(1).ok_or_else(exhausted)?;
    i64::try_from(next).map_err(|_| exhausted())?;
    Ok(next)
}

fn verify_write(
    conn: &Connection,
    activity: &Activity,
    expected: &ActivityCapabilityPolicy,
    changed: usize,
) -> Result<(), ActivityError> {
    if changed != 1
        || load_policy(conn, activity)?.as_ref() != Some(expected)
        || load_activity(conn, activity.owner_uid, &activity.id)? != *activity
    {
        return Err(corrupt(
            "capability policy write did not preserve the policy and Activity",
        ));
    }
    Ok(())
}

fn load_policy(
    conn: &Connection,
    activity: &Activity,
) -> Result<Option<ActivityCapabilityPolicy>, ActivityError> {
    conn.query_row(
        &format!("{SELECT_POLICY} WHERE owner_uid = ?1 AND activity_id = ?2"),
        params![i64::from(activity.owner_uid), activity.id],
        PolicyRow::from_row,
    )
    .optional()?
    .map(|row| row.into_policy(activity))
    .transpose()
}

struct PolicyRow {
    activity_id: String,
    owner_uid: i64,
    revision: i64,
    enabled: i64,
    rules_json: String,
    created_at: String,
    updated_at: String,
}

impl PolicyRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            activity_id: row.get(0)?,
            owner_uid: row.get(1)?,
            revision: row.get(2)?,
            enabled: row.get(3)?,
            rules_json: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    }

    fn into_policy(self, activity: &Activity) -> Result<ActivityCapabilityPolicy, ActivityError> {
        let owner_uid = u32::try_from(self.owner_uid)
            .map_err(|_| corrupt("invalid capability policy owner"))?;
        if self.activity_id != activity.id || owner_uid != activity.owner_uid {
            return Err(corrupt(
                "capability policy does not match its owned Activity",
            ));
        }
        let revision = u64::try_from(self.revision)
            .map_err(|_| corrupt("negative capability policy revision"))?;
        if revision == 0 {
            return Err(corrupt("capability policy revision is zero"));
        }
        let enabled = match self.enabled {
            0 => false,
            1 => true,
            _ => return Err(corrupt("capability policy enabled flag is not boolean")),
        };
        if self.rules_json.len() > MAX_POLICY_BYTES {
            return Err(corrupt("stored capability policy exceeds 16 KiB"));
        }
        let rules: Vec<ActivityCapabilityRule> = serde_json::from_str(&self.rules_json)
            .map_err(|error| corrupt(format!("invalid capability policy rules: {error}")))?;
        let raw: serde_json::Value = serde_json::from_str(&self.rules_json)
            .map_err(|error| corrupt(format!("invalid capability policy JSON: {error}")))?;
        let canonical = CapabilityPolicyDraft {
            rules: rules.clone(),
        }
        .canonicalized()
        .map_err(|error| corrupt(error.to_string()))?;
        if canonical.rules != rules || raw != serde_json::to_value(&rules)? {
            return Err(corrupt(
                "stored capability policy is not canonical or has unknown fields",
            ));
        }
        let created_at = parse_timestamp(&self.created_at)?;
        let updated_at = parse_timestamp(&self.updated_at)?;
        if updated_at < created_at {
            return Err(corrupt("capability policy updated_at precedes created_at"));
        }
        Ok(ActivityCapabilityPolicy {
            activity_id: self.activity_id,
            owner_uid,
            revision,
            enabled,
            rules,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

pub(super) fn validate_schema(conn: &Connection) -> Result<(), ActivityError> {
    conn.prepare(&format!("{SELECT_POLICY} LIMIT 0"))?;
    let unique: bool = conn.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM pragma_index_list('activity_capability_policies') AS idx
            WHERE idx.\"unique\" = 1 AND idx.partial = 0
              AND (SELECT COUNT(*) FROM pragma_index_info(idx.name)) = 2
              AND EXISTS(SELECT 1 FROM pragma_index_info(idx.name) WHERE seqno = 0 AND name = 'owner_uid')
              AND EXISTS(SELECT 1 FROM pragma_index_info(idx.name) WHERE seqno = 1 AND name = 'activity_id')
         )", [], |row| row.get(0),
    )?;
    if !unique {
        return Err(corrupt(
            "missing owner-scoped capability policy identity constraint",
        ));
    }
    let mut statement = conn.prepare(
        "SELECT \"table\", seq, \"from\", \"to\", on_update, on_delete
         FROM pragma_foreign_key_list('activity_capability_policies') ORDER BY seq",
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
            "missing or invalid capability policy ownership foreign key",
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
        "/test/unit/activities/sqlite/capability_policy.rs"
    ));
}
