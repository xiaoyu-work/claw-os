//! Durable execution constraints and conservative attempt accounting.
//! This provider never opens jobs, starts work, grants authority, or refunds attempts.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{load_activity, parse_id, parse_timestamp, timestamp, SqliteActivityService};
use crate::activities::execution_limits::{
    revision_sql, validate_job_id, validate_requested_turns, MAX_ATTEMPTS, MAX_TURNS_PER_ATTEMPT,
};
use crate::activities::{
    Activity, ActivityError, ActivityExecutionLimits, ActivityState, ExecutionBlockedReason,
    ExecutionLimitsDraft, ExecutionReservation,
};

pub(super) const MIGRATE_TO_V4: &str = r#"
CREATE TABLE activity_execution_limits (
    activity_id           TEXT NOT NULL,
    owner_uid             INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    revision              INTEGER NOT NULL CHECK(revision > 0),
    enabled               INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    max_attempts          INTEGER NOT NULL CHECK(max_attempts BETWEEN 1 AND 1000),
    max_turns_per_attempt INTEGER NOT NULL CHECK(max_turns_per_attempt BETWEEN 1 AND 100),
    expires_at            TEXT NOT NULL,
    used_attempts         INTEGER NOT NULL CHECK(used_attempts BETWEEN 0 AND 1000),
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL,
    PRIMARY KEY(owner_uid, activity_id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id)
);

CREATE TABLE activity_execution_reservations (
    sequence             INTEGER PRIMARY KEY AUTOINCREMENT CHECK(sequence > 0),
    id                   TEXT NOT NULL,
    activity_id          TEXT NOT NULL,
    owner_uid            INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    job_id               TEXT NOT NULL,
    policy_revision      INTEGER NOT NULL CHECK(policy_revision > 0),
    policy_max_turns      INTEGER NOT NULL CHECK(policy_max_turns BETWEEN 1 AND 100),
    requested_max_turns   INTEGER CHECK(requested_max_turns BETWEEN 1 AND 4294967295),
    max_turns            INTEGER NOT NULL CHECK(max_turns BETWEEN 1 AND 100),
    expires_at           TEXT NOT NULL,
    reserved_at          TEXT NOT NULL,
    UNIQUE(owner_uid, id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id),
    FOREIGN KEY(owner_uid, activity_id)
        REFERENCES activity_execution_limits(owner_uid, activity_id)
);
CREATE INDEX activity_execution_reservations_activity
    ON activity_execution_reservations(owner_uid, activity_id, sequence);
"#;

const SELECT_POLICY: &str =
    "SELECT activity_id, owner_uid, revision, enabled, max_attempts, max_turns_per_attempt,
        expires_at, used_attempts, created_at, updated_at FROM activity_execution_limits";
const SELECT_RESERVATION: &str =
    "SELECT sequence, id, activity_id, owner_uid, job_id, policy_revision, policy_max_turns,
        requested_max_turns, max_turns, expires_at, reserved_at FROM activity_execution_reservations";

pub(super) fn get(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
) -> Result<Option<ActivityExecutionLimits>, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    let mut conn = service.lock()?;
    let tx = conn.transaction()?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    let policy = load_state(&tx, &activity)?.map(|state| state.policy);
    tx.commit()?;
    Ok(policy)
}

pub(super) fn set(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    expected_revision: Option<u64>,
    draft: ExecutionLimitsDraft,
) -> Result<ActivityExecutionLimits, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    let draft = draft.canonicalized()?;
    if let Some(revision) = expected_revision {
        revision_sql(revision)?;
    }
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    let existing = load_state(&tx, &activity)?;
    require_configurable(&activity)?;
    let now = timestamp();
    require_future(&draft, parse_timestamp(&now)?)?;
    let expected = match (existing, expected_revision) {
        (None, None) => StoredState {
            policy: ActivityExecutionLimits {
                activity_id,
                owner_uid,
                revision: 1,
                enabled: true,
                limits: draft,
                used_attempts: 0,
                created_at: now.clone(),
                updated_at: now,
            },
            reservations: Vec::new(),
        },
        (Some(mut state), Some(revision)) => {
            require_revision(&state.policy, revision)?;
            state.policy.revision = next_revision(state.policy.revision)?;
            state.policy.limits = draft;
            state.policy.updated_at = now.max(state.policy.updated_at);
            state
        }
        (Some(_), None) => {
            return Err(ActivityError::Conflict(
                "execution limits already exist; supply expected_revision to update".into(),
            ));
        }
        (None, Some(_)) => {
            return Err(ActivityError::Conflict(
                "execution limits do not exist; omit expected_revision to create".into(),
            ));
        }
    };
    let changed = if let Some(revision) = expected_revision {
        update_policy(&tx, &expected.policy, revision)?
    } else {
        let policy = &expected.policy;
        tx.execute(
            "INSERT INTO activity_execution_limits (
                activity_id, owner_uid, revision, enabled, max_attempts, max_turns_per_attempt,
                expires_at, used_attempts, created_at, updated_at
             ) VALUES (?1, ?2, 1, 1, ?3, ?4, ?5, 0, ?6, ?6)",
            params![
                policy.activity_id,
                i64::from(owner_uid),
                i64::from(policy.limits.max_attempts),
                i64::from(policy.limits.max_turns_per_attempt),
                policy.limits.expires_at,
                policy.created_at,
            ],
        )?
    };
    verify_write(&tx, &activity, &expected, changed)?;
    tx.commit()?;
    Ok(expected.policy)
}

pub(super) fn set_enabled(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    expected_revision: u64,
    enabled: bool,
) -> Result<ActivityExecutionLimits, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    revision_sql(expected_revision)?;
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    let mut expected = load_state(&tx, &activity)?.ok_or(ActivityError::NotFound)?;
    require_revision(&expected.policy, expected_revision)?;
    let now = timestamp();
    if enabled {
        require_configurable(&activity)?;
        require_future(&expected.policy.limits, parse_timestamp(&now)?)?;
    }
    expected.policy.revision = next_revision(expected.policy.revision)?;
    expected.policy.enabled = enabled;
    expected.policy.updated_at = now.max(expected.policy.updated_at);
    let changed = update_policy(&tx, &expected.policy, expected_revision)?;
    verify_write(&tx, &activity, &expected, changed)?;
    tx.commit()?;
    Ok(expected.policy)
}

pub(super) fn reserve(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    attempt_id: &str,
    job_id: &str,
    requested_max_turns: Option<u32>,
) -> Result<Option<ExecutionReservation>, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    let attempt_id = parse_id(attempt_id)?;
    validate_job_id(job_id)?;
    validate_requested_turns(requested_max_turns)?;
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    let bound_activity: Option<String> = tx.query_row(
        "SELECT activity_id FROM activity_execution_reservations WHERE owner_uid = ?1 AND id = ?2",
        params![i64::from(owner_uid), attempt_id],
        |row| row.get(0),
    ).optional()?;
    if let Some(bound_activity) = bound_activity {
        if parse_id(&bound_activity).map_err(stored_error)? != bound_activity {
            return Err(corrupt("reservation Activity UUID is not canonical"));
        }
        if bound_activity != activity_id {
            load_activity(&tx, owner_uid, &bound_activity).map_err(stored_error)?;
            return Err(ActivityError::Conflict(
                "execution attempt id is already bound to another Activity".into(),
            ));
        }
    }
    let Some(mut expected) = load_state(&tx, &activity)? else {
        tx.commit()?;
        return Ok(None);
    };
    let original = expected
        .reservations
        .iter()
        .find(|stored| stored.entry.id == attempt_id);
    if let Some(original) = original {
        if original.entry.job_id != job_id || original.requested_max_turns != requested_max_turns {
            return Err(ActivityError::Conflict(
                "execution attempt id is already bound to a different job or request".into(),
            ));
        }
    }
    let now = timestamp();
    require_reservable(&activity, &expected.policy, parse_timestamp(&now)?)?;
    if let Some(original) = original {
        require_revision(&expected.policy, original.entry.policy_revision)?;
        let reservation = original.entry.clone();
        tx.commit()?;
        return Ok(Some(reservation));
    }
    if expected.policy.used_attempts >= expected.policy.limits.max_attempts
        || expected.policy.used_attempts >= MAX_ATTEMPTS
    {
        return blocked(ExecutionBlockedReason::AttemptLimit);
    }
    let reservation = ExecutionReservation {
        id: attempt_id,
        activity_id,
        owner_uid,
        job_id: job_id.to_string(),
        policy_revision: expected.policy.revision,
        max_turns: requested_max_turns
            .unwrap_or(expected.policy.limits.max_turns_per_attempt)
            .min(expected.policy.limits.max_turns_per_attempt),
        expires_at: expected.policy.limits.expires_at.clone(),
        reserved_at: now.clone(),
    };
    let inserted = tx.execute(
        "INSERT INTO activity_execution_reservations (
            id, activity_id, owner_uid, job_id, policy_revision, policy_max_turns,
            requested_max_turns, max_turns, expires_at, reserved_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            reservation.id,
            reservation.activity_id,
            i64::from(owner_uid),
            reservation.job_id,
            revision_sql(reservation.policy_revision)?,
            i64::from(expected.policy.limits.max_turns_per_attempt),
            requested_max_turns.map(i64::from),
            i64::from(reservation.max_turns),
            reservation.expires_at,
            reservation.reserved_at,
        ],
    )?;
    let previous_used = expected.policy.used_attempts;
    expected.policy.used_attempts = previous_used
        .checked_add(1)
        .ok_or_else(|| corrupt("execution attempt counter overflow"))?;
    expected.policy.updated_at = now.max(expected.policy.updated_at);
    expected.reservations.push(StoredReservation {
        entry: reservation.clone(),
        requested_max_turns,
        policy_max_turns: expected.policy.limits.max_turns_per_attempt,
    });
    let updated = tx.execute(
        "UPDATE activity_execution_limits SET used_attempts = ?1, updated_at = ?2
         WHERE owner_uid = ?3 AND activity_id = ?4 AND revision = ?5 AND used_attempts = ?6",
        params![
            i64::from(expected.policy.used_attempts),
            expected.policy.updated_at,
            i64::from(owner_uid),
            expected.policy.activity_id,
            revision_sql(expected.policy.revision)?,
            i64::from(previous_used),
        ],
    )?;
    if inserted != 1 {
        return Err(corrupt("execution reservation was not appended"));
    }
    verify_write(&tx, &activity, &expected, updated)?;
    tx.commit()?;
    Ok(Some(reservation))
}

fn require_configurable(activity: &Activity) -> Result<(), ActivityError> {
    if !matches!(
        activity.state,
        ActivityState::Active | ActivityState::Paused
    ) {
        return blocked(ExecutionBlockedReason::Inactive);
    }
    Ok(())
}

fn require_future(limits: &ExecutionLimitsDraft, now: DateTime<Utc>) -> Result<(), ActivityError> {
    if limits.expiry()? <= now {
        return blocked(ExecutionBlockedReason::Expired);
    }
    Ok(())
}

fn require_reservable(
    activity: &Activity,
    policy: &ActivityExecutionLimits,
    now: DateTime<Utc>,
) -> Result<(), ActivityError> {
    if !activity.state.allows_work() {
        return blocked(ExecutionBlockedReason::Inactive);
    }
    if !policy.enabled {
        return blocked(ExecutionBlockedReason::Disabled);
    }
    require_future(&policy.limits, now)
}

fn require_revision(policy: &ActivityExecutionLimits, expected: u64) -> Result<(), ActivityError> {
    if policy.revision != expected {
        return blocked(ExecutionBlockedReason::StaleRevision);
    }
    Ok(())
}

fn next_revision(revision: u64) -> Result<u64, ActivityError> {
    let exhausted = || ActivityError::Conflict("execution policy revision is exhausted".into());
    let next = revision.checked_add(1).ok_or_else(exhausted)?;
    i64::try_from(next).map_err(|_| exhausted())?;
    Ok(next)
}

fn update_policy(
    conn: &Connection,
    policy: &ActivityExecutionLimits,
    expected_revision: u64,
) -> Result<usize, ActivityError> {
    Ok(conn.execute(
        "UPDATE activity_execution_limits SET revision = ?1, enabled = ?2, max_attempts = ?3,
            max_turns_per_attempt = ?4, expires_at = ?5, updated_at = ?6
         WHERE owner_uid = ?7 AND activity_id = ?8 AND revision = ?9",
        params![
            revision_sql(policy.revision)?,
            i64::from(policy.enabled),
            i64::from(policy.limits.max_attempts),
            i64::from(policy.limits.max_turns_per_attempt),
            policy.limits.expires_at,
            policy.updated_at,
            i64::from(policy.owner_uid),
            policy.activity_id,
            revision_sql(expected_revision)?,
        ],
    )?)
}

fn verify_write(
    conn: &Connection,
    activity: &Activity,
    expected: &StoredState,
    changed: usize,
) -> Result<(), ActivityError> {
    if changed != 1
        || load_state(conn, activity)?.as_ref() != Some(expected)
        || load_activity(conn, activity.owner_uid, &activity.id)? != *activity
    {
        return Err(corrupt(
            "execution accounting write did not preserve the policy, ledger and Activity",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredState {
    policy: ActivityExecutionLimits,
    reservations: Vec<StoredReservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredReservation {
    entry: ExecutionReservation,
    requested_max_turns: Option<u32>,
    policy_max_turns: u32,
}

fn load_state(
    conn: &Connection,
    activity: &Activity,
) -> Result<Option<StoredState>, ActivityError> {
    let row = conn
        .query_row(
            &format!("{SELECT_POLICY} WHERE owner_uid = ?1 AND activity_id = ?2"),
            params![i64::from(activity.owner_uid), activity.id],
            PolicyRow::from_row,
        )
        .optional()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM activity_execution_reservations WHERE owner_uid = ?1 AND activity_id = ?2",
        params![i64::from(activity.owner_uid), activity.id], |row| row.get(0),
    )?;
    let Some(row) = row else {
        if count != 0 {
            return Err(corrupt("execution reservations exist without their policy"));
        }
        return Ok(None);
    };
    let policy = row.into_policy(activity)?;
    if count != i64::from(policy.used_attempts) || count > i64::from(MAX_ATTEMPTS) {
        return Err(corrupt(
            "execution attempt counter does not match the bounded reservation ledger",
        ));
    }
    let mut statement = conn.prepare(&format!(
        "{SELECT_RESERVATION} WHERE owner_uid = ?1 AND activity_id = ?2 ORDER BY sequence LIMIT ?3"
    ))?;
    let rows = statement.query_map(
        params![
            i64::from(activity.owner_uid),
            activity.id,
            i64::from(MAX_ATTEMPTS) + 1
        ],
        ReservationRow::from_row,
    )?;
    let mut reservations: Vec<StoredReservation> = Vec::new();
    let mut sequence = 0;
    for row in rows {
        let row = row?;
        if row.sequence <= sequence {
            return Err(corrupt(
                "execution reservation sequence is not positive and increasing",
            ));
        }
        sequence = row.sequence;
        let reservation = row.into_reservation(&policy)?;
        if let Some(previous) = reservations.last() {
            if previous.entry.policy_revision > reservation.entry.policy_revision
                || (previous.entry.policy_revision == reservation.entry.policy_revision
                    && (previous.policy_max_turns != reservation.policy_max_turns
                        || previous.entry.expires_at != reservation.entry.expires_at))
            {
                return Err(corrupt(
                    "execution reservation policy snapshots are inconsistent",
                ));
            }
        }
        reservations.push(reservation);
    }
    let loaded = i64::try_from(reservations.len())
        .map_err(|_| corrupt("execution reservation count is not representable"))?;
    if loaded != count {
        return Err(corrupt(
            "execution reservation count changed within a snapshot",
        ));
    }
    Ok(Some(StoredState {
        policy,
        reservations,
    }))
}

struct PolicyRow {
    activity_id: String,
    owner_uid: i64,
    revision: i64,
    enabled: i64,
    max_attempts: i64,
    max_turns_per_attempt: i64,
    expires_at: String,
    used_attempts: i64,
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
            max_attempts: row.get(4)?,
            max_turns_per_attempt: row.get(5)?,
            expires_at: row.get(6)?,
            used_attempts: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }

    fn into_policy(self, activity: &Activity) -> Result<ActivityExecutionLimits, ActivityError> {
        let owner_uid = stored_u32(self.owner_uid, "policy owner")?;
        if self.activity_id != activity.id || owner_uid != activity.owner_uid {
            return Err(corrupt(
                "execution policy is not bound to its owned Activity",
            ));
        }
        let limits = ExecutionLimitsDraft {
            max_attempts: stored_u32(self.max_attempts, "max_attempts")?,
            max_turns_per_attempt: stored_u32(self.max_turns_per_attempt, "max_turns_per_attempt")?,
            expires_at: self.expires_at,
        };
        limits.validate().map_err(stored_error)?;
        parse_timestamp(&limits.expires_at)?;
        let created_at = parse_timestamp(&self.created_at)?;
        let updated_at = parse_timestamp(&self.updated_at)?;
        if updated_at < created_at {
            return Err(corrupt("execution policy updated_at precedes created_at"));
        }
        let enabled = match self.enabled {
            0 => false,
            1 => true,
            _ => return Err(corrupt("execution policy enabled flag is not boolean")),
        };
        let used_attempts = stored_u32(self.used_attempts, "used_attempts")?;
        if used_attempts > MAX_ATTEMPTS {
            return Err(corrupt(
                "execution policy exceeds the lifetime attempt limit",
            ));
        }
        Ok(ActivityExecutionLimits {
            activity_id: self.activity_id,
            owner_uid,
            revision: stored_revision(self.revision)?,
            enabled,
            limits,
            used_attempts,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

struct ReservationRow {
    sequence: i64,
    id: String,
    activity_id: String,
    owner_uid: i64,
    job_id: String,
    policy_revision: i64,
    policy_max_turns: i64,
    requested_max_turns: Option<i64>,
    max_turns: i64,
    expires_at: String,
    reserved_at: String,
}

impl ReservationRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            sequence: row.get(0)?,
            id: row.get(1)?,
            activity_id: row.get(2)?,
            owner_uid: row.get(3)?,
            job_id: row.get(4)?,
            policy_revision: row.get(5)?,
            policy_max_turns: row.get(6)?,
            requested_max_turns: row.get(7)?,
            max_turns: row.get(8)?,
            expires_at: row.get(9)?,
            reserved_at: row.get(10)?,
        })
    }

    fn into_reservation(
        self,
        policy: &ActivityExecutionLimits,
    ) -> Result<StoredReservation, ActivityError> {
        let owner_uid = stored_u32(self.owner_uid, "reservation owner")?;
        if parse_id(&self.id).map_err(stored_error)? != self.id
            || self.activity_id != policy.activity_id
            || owner_uid != policy.owner_uid
        {
            return Err(corrupt(
                "execution reservation UUID or ownership is invalid",
            ));
        }
        validate_job_id(&self.job_id).map_err(stored_error)?;
        let policy_revision = stored_revision(self.policy_revision)?;
        if policy_revision > policy.revision {
            return Err(corrupt(
                "execution reservation references a future policy revision",
            ));
        }
        let policy_max_turns = stored_u32(self.policy_max_turns, "reserved policy turn limit")?;
        let max_turns = stored_u32(self.max_turns, "reserved max_turns")?;
        let requested_max_turns = self
            .requested_max_turns
            .map(|value| stored_u32(value, "requested_max_turns"))
            .transpose()?;
        validate_requested_turns(requested_max_turns).map_err(stored_error)?;
        if !(1..=MAX_TURNS_PER_ATTEMPT).contains(&policy_max_turns)
            || max_turns
                != requested_max_turns
                    .unwrap_or(policy_max_turns)
                    .min(policy_max_turns)
        {
            return Err(corrupt(
                "execution reservation turn limit does not match its bounded request",
            ));
        }
        let expires_at = parse_timestamp(&self.expires_at)?;
        let reserved_at = parse_timestamp(&self.reserved_at)?;
        if expires_at <= reserved_at {
            return Err(corrupt(
                "execution reservation was not made before its expiry",
            ));
        }
        if policy_revision == policy.revision
            && (!policy.enabled
                || policy_max_turns != policy.limits.max_turns_per_attempt
                || self.expires_at != policy.limits.expires_at)
        {
            return Err(corrupt(
                "execution reservation does not match its current policy revision",
            ));
        }
        Ok(StoredReservation {
            entry: ExecutionReservation {
                id: self.id,
                activity_id: self.activity_id,
                owner_uid,
                job_id: self.job_id,
                policy_revision,
                max_turns,
                expires_at: self.expires_at,
                reserved_at: self.reserved_at,
            },
            requested_max_turns,
            policy_max_turns,
        })
    }
}

pub(super) fn validate_schema(conn: &Connection) -> Result<(), ActivityError> {
    conn.prepare(&format!("{SELECT_POLICY} LIMIT 0"))?;
    conn.prepare(&format!(
        "{SELECT_RESERVATION} INDEXED BY activity_execution_reservations_activity LIMIT 0"
    ))?;
    for (table, identity, targets) in [
        (
            "activity_execution_limits",
            "activity_id",
            &["activities"][..],
        ),
        (
            "activity_execution_reservations",
            "id",
            &["activities", "activity_execution_limits"][..],
        ),
    ] {
        let unique: bool = conn.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM pragma_index_list(?1) AS idx
                WHERE idx.\"unique\" = 1 AND idx.partial = 0
                  AND (SELECT COUNT(*) FROM pragma_index_info(idx.name)) = 2
                  AND EXISTS(SELECT 1 FROM pragma_index_info(idx.name) WHERE seqno = 0 AND name = 'owner_uid')
                  AND EXISTS(SELECT 1 FROM pragma_index_info(idx.name) WHERE seqno = 1 AND name = ?2)
             )",
            params![table, identity], |row| row.get(0),
        )?;
        if !unique {
            return Err(corrupt(format!(
                "missing owner-scoped {table} identity constraint"
            )));
        }
        let mut statement = conn.prepare(
            "SELECT \"table\", seq, \"from\", \"to\", on_update, on_delete FROM pragma_foreign_key_list(?1)"
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
        let mut expected = Vec::new();
        for target in targets {
            for (seq, from, to) in [
                (0_i64, "owner_uid", "owner_uid"),
                (
                    1_i64,
                    "activity_id",
                    if *target == "activities" {
                        "id"
                    } else {
                        "activity_id"
                    },
                ),
            ] {
                expected.push((
                    (*target).to_string(),
                    seq,
                    from.to_string(),
                    to.to_string(),
                    "NO ACTION".to_string(),
                    "NO ACTION".to_string(),
                ));
            }
        }
        actual.sort();
        expected.sort();
        if actual != expected {
            return Err(corrupt(format!(
                "missing or invalid {table} ownership foreign keys"
            )));
        }
    }
    Ok(())
}

fn stored_u32(value: i64, field: &str) -> Result<u32, ActivityError> {
    u32::try_from(value).map_err(|_| corrupt(format!("invalid stored execution {field}")))
}

fn stored_revision(value: i64) -> Result<u64, ActivityError> {
    let revision =
        u64::try_from(value).map_err(|_| corrupt("negative execution policy revision"))?;
    if revision == 0 {
        return Err(corrupt("execution policy revision is zero"));
    }
    Ok(revision)
}

fn stored_error(error: ActivityError) -> ActivityError {
    match error {
        ActivityError::Database(_) | ActivityError::Io(_) | ActivityError::Corrupt(_) => error,
        _ => corrupt(error.to_string()),
    }
}

fn corrupt(message: impl Into<String>) -> ActivityError {
    ActivityError::Corrupt(message.into())
}

fn blocked<T>(reason: ExecutionBlockedReason) -> Result<T, ActivityError> {
    Err(ActivityError::ExecutionBlocked(reason))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/sqlite/execution_limits.rs"
    ));
}
