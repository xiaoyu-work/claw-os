//! Transactional local execution state; never part of portable continuity.

use chrono::{Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{load_activity, parse_id, timestamp, SqliteActivityService};
use crate::activities::{
    Activity, ActivityError, ActivityState, ActivityWork, ActivityWorkChange, ActivityWorkNext,
    ActivityWorkOutcome, ActivityWorkPhase, ActivityWorkRecord, ActivityWorkSegment,
    MAX_STAGNANT_WORK_SEGMENTS, MAX_WORK_HISTORY,
};

pub(super) const MIGRATE_TO_V9: &str = r#"
CREATE TABLE activity_work (
    owner_uid INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    activity_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    state_json TEXT NOT NULL,
    PRIMARY KEY(owner_uid, activity_id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id)
);
CREATE TABLE activity_work_history (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_uid INTEGER NOT NULL,
    activity_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    record_json TEXT NOT NULL,
    UNIQUE(owner_uid, job_id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id)
);
CREATE INDEX activity_work_history_activity
    ON activity_work_history(owner_uid, activity_id, sequence DESC);
"#;

fn corrupt(message: impl std::fmt::Display) -> ActivityError {
    ActivityError::Corrupt(format!("Activity work: {message}"))
}

fn load(conn: &Connection, activity: &Activity) -> Result<Option<ActivityWork>, ActivityError> {
    let row = conn
        .query_row(
            "SELECT revision, state_json FROM activity_work WHERE owner_uid=?1 AND activity_id=?2",
            params![activity.owner_uid, activity.id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if row.is_none() {
        let retained: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM activity_work_history WHERE owner_uid=?1 AND activity_id=?2)",
            params![activity.owner_uid,activity.id],|row|row.get(0),
        )?;
        if retained {
            return Err(corrupt(
                "work state is missing while handoff history remains",
            ));
        }
    }
    row.map(|(revision, json)| {
        let work: ActivityWork = serde_json::from_str(&json).map_err(corrupt)?;
        work.validate().map_err(corrupt)?;
        if work.owner_uid != activity.owner_uid
            || work.activity_id != activity.id
            || i64::try_from(work.revision).ok() != Some(revision)
        {
            return Err(corrupt("stored identity/revision mismatch"));
        }
        let latest = conn
            .query_row(
                "SELECT job_id,record_json FROM activity_work_history
             WHERE owner_uid=?1 AND activity_id=?2 ORDER BY sequence DESC LIMIT 1",
                params![activity.owner_uid, activity.id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(job_id, json)| decode_record(&job_id, &json))
            .transpose()?;
        if latest != work.last_checkpoint {
            return Err(corrupt("the latest handoff differs from durable history"));
        }
        Ok(work)
    })
    .transpose()
}

pub(super) fn validate_schema(conn: &Connection) -> Result<(), ActivityError> {
    conn.prepare("SELECT owner_uid, activity_id, revision, state_json FROM activity_work LIMIT 0")?;
    conn.prepare("SELECT sequence, owner_uid, activity_id, job_id, record_json FROM activity_work_history LIMIT 0")?;
    let foreign_keys: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('activity_work') WHERE \"table\"='activities'",
        [], |row| row.get(0),
    )?;
    if foreign_keys != 2 {
        return Err(corrupt("work ownership foreign key is missing"));
    }
    Ok(())
}

pub(super) fn get(
    service: &SqliteActivityService,
    owner: u32,
    id: &str,
) -> Result<Option<ActivityWork>, ActivityError> {
    let id = parse_id(id)?;
    let conn = service.lock()?;
    let activity = load_activity(&conn, owner, &id)?;
    load(&conn, &activity)
}

impl SqliteActivityService {
    pub(crate) fn work_entries(&self) -> Result<Vec<(u32, String)>, ActivityError> {
        let conn = self.lock()?;
        let mut query = conn.prepare(
            "SELECT owner_uid,activity_id FROM activity_work ORDER BY owner_uid,activity_id",
        )?;
        let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(ActivityError::from)
    }
}

pub(super) fn history(
    service: &SqliteActivityService,
    owner: u32,
    id: &str,
    limit: usize,
) -> Result<Vec<ActivityWorkRecord>, ActivityError> {
    let id = parse_id(id)?;
    let conn = service.lock()?;
    load_activity(&conn, owner, &id)?;
    let mut query = conn.prepare(
        "SELECT job_id, record_json FROM activity_work_history
         WHERE owner_uid=?1 AND activity_id=?2 ORDER BY sequence DESC LIMIT ?3",
    )?;
    let rows = query.query_map(params![owner, id, super::list_limit(limit)?], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (job_id, json) = row?;
        decode_record(&job_id, &json)
    })
    .collect()
}

fn decode_record(job_id: &str, json: &str) -> Result<ActivityWorkRecord, ActivityError> {
    let record: ActivityWorkRecord = serde_json::from_str(json).map_err(corrupt)?;
    record.validate().map_err(corrupt)?;
    if record.job_id != job_id {
        return Err(corrupt("history Job identity differs"));
    }
    Ok(record)
}

pub(super) fn change(
    service: &SqliteActivityService,
    owner: u32,
    id: &str,
    expected: Option<u64>,
    change: ActivityWorkChange,
) -> Result<ActivityWork, ActivityError> {
    let id = parse_id(id)?;
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner, &id)?;
    let previous = load(&tx, &activity)?;
    if previous.as_ref().map(|work| work.revision) != expected {
        return Err(ActivityError::Conflict(
            "Activity work revision changed; refresh before editing".into(),
        ));
    }
    let now = timestamp();
    let mut work = match (&previous, &change) {
        (Some(work), _) => work.clone(),
        (None, ActivityWorkChange::Configure(config)) => ActivityWork {
            activity_id: activity.id.clone(),
            owner_uid: owner,
            revision: 0,
            config: config.clone(),
            enabled: false,
            generation: None,
            phase: ActivityWorkPhase::Ready,
            next_task: config.task.clone(),
            next_wake_at: None,
            event_after: 0,
            current: None,
            last_checkpoint: None,
            stagnant_segments: 0,
            message: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        },
        _ => {
            return Err(ActivityError::Conflict(
                "configure Activity work first".into(),
            ))
        }
    };
    match change {
        ActivityWorkChange::Configure(config) => {
            config.validate()?;
            configurable(&activity)?;
            if work.current.is_some() {
                return Err(ActivityError::Conflict(
                    "finish or cancel the current Job before editing work".into(),
                ));
            }
            if matches!(
                work.phase,
                ActivityWorkPhase::Held | ActivityWorkPhase::Review
            ) {
                return Err(ActivityError::Conflict(
                    "resume inspected work with an explicit next task and review note; configuration cannot bypass review".into()
                ));
            }
            work.next_task = config.task.clone();
            work.config = config;
            work.enabled = false;
            work.phase = ActivityWorkPhase::Ready;
            work.next_wake_at = None;
            work.message = None;
            work.stagnant_segments = 0;
        }
        ActivityWorkChange::Enable {
            generation,
            next_task,
            review_note,
        } => {
            if !activity.state.allows_work() {
                return Err(ActivityError::Conflict(
                    "resume the Activity before enabling automatic work".into(),
                ));
            }
            if work.current.is_some() {
                return Err(ActivityError::Conflict(
                    "a previous work Job still needs reconciliation".into(),
                ));
            }
            let limits =
                super::execution_limits::work_policy(&tx, &activity)?.ok_or_else(|| {
                    ActivityError::Invalid("automatic work requires finite execution limits".into())
                })?;
            if !limits.enabled
                || limits.limits.expiry()? <= Utc::now()
                || limits.used_attempts >= limits.limits.max_attempts
            {
                return Err(ActivityError::Conflict(
                    "Activity execution limits block automatic work".into(),
                ));
            }
            if matches!(
                work.phase,
                ActivityWorkPhase::Review | ActivityWorkPhase::Held
            ) {
                let note = review_note.as_deref().ok_or_else(|| {
                    ActivityError::Invalid(
                        "inspect the previous work and supply a review note before resuming".into(),
                    )
                })?;
                super::super::validate_text("work review note", note, 2048, false, true)?;
                if next_task.is_none() {
                    return Err(ActivityError::Invalid(
                        "resuming reviewed work requires an explicit next task".into(),
                    ));
                }
            }
            work.generation = Some(parse_id(&generation)?);
            if let Some(task) = next_task {
                super::super::validate_text("next work task", &task, 8192, false, true)?;
                work.next_task = task;
            }
            work.enabled = true;
            work.phase = ActivityWorkPhase::Ready;
            work.next_wake_at = None;
            work.stagnant_segments = 0;
            work.message = review_note;
        }
        ActivityWorkChange::Disable => {
            work.enabled = false;
            work.message =
                Some("Automatic progression is stopped; an existing Job is not cancelled.".into());
        }
        ActivityWorkChange::Reserve {
            generation,
            job_id,
            session_id,
        } => {
            if !activity.state.allows_work()
                || !work.due_at(Utc::now())?
                || work.generation.as_deref() != Some(generation.as_str())
            {
                return Err(ActivityError::Conflict(
                    "Activity work is not ready for this scheduler generation".into(),
                ));
            }
            super::super::execution_limits::validate_job_id(&job_id)?;
            super::super::validate_text("work session", &session_id, 128, false, false)?;
            work.current = Some(ActivityWorkSegment {
                job_id,
                session_id,
                generation,
                prepared_at: now.clone(),
                checkpoint: None,
            });
            work.phase = ActivityWorkPhase::Running;
            work.next_wake_at = None;
            work.message = None;
        }
        ActivityWorkChange::Stage { job_id, checkpoint } => {
            checkpoint.validate()?;
            for reference in &checkpoint.artifacts {
                if !activity
                    .resources
                    .iter()
                    .any(|resource| resource.reference == *reference)
                {
                    return Err(ActivityError::Invalid(
                        "work artifacts must reference attached Activity objects".into(),
                    ));
                }
            }
            let segment = work
                .current
                .as_mut()
                .filter(|segment| segment.job_id == job_id)
                .ok_or_else(|| {
                    ActivityError::Conflict("work handoff is not for the current Job".into())
                })?;
            if let Some(existing) = &segment.checkpoint {
                if *existing != checkpoint {
                    return Err(ActivityError::Conflict(
                        "the Job already reported a different handoff".into(),
                    ));
                }
                return Ok(work);
            }
            segment.checkpoint = Some(checkpoint);
        }
        ActivityWorkChange::Finish {
            job_id,
            outcome,
            event_after,
        } => {
            let segment = work
                .current
                .take()
                .filter(|segment| segment.job_id == job_id)
                .ok_or_else(|| {
                    ActivityError::Conflict("work completion is not for the current Job".into())
                })?;
            let (outcome_text, message) = match outcome {
                ActivityWorkOutcome::Reported => ("reported", None),
                ActivityWorkOutcome::Failed(message) => ("failed", Some(message)),
                ActivityWorkOutcome::Indeterminate(message) => ("indeterminate", Some(message)),
                ActivityWorkOutcome::Cancelled => {
                    ("cancelled", Some("The work Job was cancelled.".into()))
                }
            };
            let record = ActivityWorkRecord {
                job_id: segment.job_id,
                session_id: segment.session_id,
                recorded_at: now.clone(),
                checkpoint: segment.checkpoint,
                outcome: outcome_text.into(),
                message: message.clone(),
            };
            record.validate()?;
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM activity_work_history WHERE owner_uid=?1 AND activity_id=?2",
                params![owner, activity.id],
                |row| row.get(0),
            )?;
            if count >= MAX_WORK_HISTORY as i64 {
                return Err(ActivityError::LimitReached);
            }
            tx.execute(
                "INSERT INTO activity_work_history(owner_uid,activity_id,job_id,record_json) VALUES(?1,?2,?3,?4)",
                params![owner, activity.id, record.job_id, serde_json::to_string(&record)?],
            )?;
            work.phase = ActivityWorkPhase::Held;
            work.enabled &= outcome_text == "reported";
            work.next_wake_at = None;
            work.event_after = event_after;
            work.message = message;
            if outcome_text == "reported" {
                if let Some(checkpoint) = &record.checkpoint {
                    work.stagnant_segments = if checkpoint.made_progress
                        || !matches!(checkpoint.next, ActivityWorkNext::Continue { .. })
                    {
                        0
                    } else {
                        (work.stagnant_segments + 1).min(MAX_STAGNANT_WORK_SEGMENTS)
                    };
                    match &checkpoint.next {
                        ActivityWorkNext::Continue { task } => {
                            work.next_task = task.clone();
                            work.phase = ActivityWorkPhase::Ready;
                        }
                        ActivityWorkNext::Wait { task, reason } => {
                            work.next_task = task.clone();
                            work.phase = ActivityWorkPhase::Waiting;
                            work.message = Some(reason.clone());
                            work.next_wake_at = Some(
                                (Utc::now()
                                    + Duration::seconds(i64::from(
                                        work.config.check_interval_secs,
                                    )))
                                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                            );
                        }
                        ActivityWorkNext::Review { reason }
                        | ActivityWorkNext::Blocked { reason } => {
                            work.phase = ActivityWorkPhase::Review;
                            work.enabled = false;
                            work.message = Some(reason.clone());
                        }
                    }
                    if work.stagnant_segments >= MAX_STAGNANT_WORK_SEGMENTS {
                        work.phase = ActivityWorkPhase::Review;
                        work.enabled = false;
                        work.message = Some(
                            "Three continuing segments reported no progress; review before continuing.".into(),
                        );
                    }
                } else {
                    work.phase = ActivityWorkPhase::Review;
                    work.enabled = false;
                    work.message = Some("The Job ended without a structured handoff; inspect its result before continuing.".into());
                }
            }
            work.last_checkpoint = Some(record);
        }
        ActivityWorkChange::Wake {
            generation,
            event_index,
        } => {
            if !activity.state.allows_work() {
                return Err(ActivityError::ExecutionBlocked(
                    crate::activities::ExecutionBlockedReason::Inactive,
                ));
            }
            if !work.enabled
                || work.phase != ActivityWorkPhase::Waiting
                || work.current.is_some()
                || work.generation.as_deref() != Some(generation.as_str())
                || event_index < work.event_after
            {
                return Err(ActivityError::Conflict(
                    "the event does not wake the current work wait".into(),
                ));
            }
            work.phase = ActivityWorkPhase::Ready;
            work.next_wake_at = None;
        }
        ActivityWorkChange::Hold(message) => {
            if work.current.is_some() {
                return Err(ActivityError::Conflict(
                    "reconcile the current work Job before holding work".into(),
                ));
            }
            work.enabled = false;
            work.phase = ActivityWorkPhase::Held;
            work.message = Some(message);
            work.next_wake_at = None;
        }
    }
    work.revision = work
        .revision
        .checked_add(1)
        .filter(|revision| *revision <= i64::MAX as u64)
        .ok_or_else(|| ActivityError::Conflict("Activity work revision exhausted".into()))?;
    work.updated_at = now.max(work.updated_at);
    work.validate()?;
    tx.execute(
        "INSERT INTO activity_work(owner_uid,activity_id,revision,state_json) VALUES(?1,?2,?3,?4)
         ON CONFLICT(owner_uid,activity_id) DO UPDATE SET revision=excluded.revision,state_json=excluded.state_json",
        params![owner, activity.id, i64::try_from(work.revision)
            .map_err(|_| corrupt("work revision is not representable"))?, serde_json::to_string(&work)?],
    )?;
    if load(&tx, &activity)?.as_ref() != Some(&work) {
        return Err(corrupt("work mutation acknowledgement differs"));
    }
    tx.commit()?;
    Ok(work)
}

fn configurable(activity: &Activity) -> Result<(), ActivityError> {
    if matches!(
        activity.state,
        ActivityState::Active | ActivityState::Paused
    ) {
        Ok(())
    } else {
        Err(ActivityError::Conflict(
            "reopen the Activity before configuring work".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/sqlite/work.rs"
    ));
}
