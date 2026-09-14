//! Durable Activity monetary policy, reservations, and settlements.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{load_activity, parse_id, parse_timestamp, timestamp, SqliteActivityService};
use crate::activities::monetary_budget::{
    accounted_cost, validate_runtime_identity, MAX_OUTPUT_TOKENS_PER_TURN,
    MAX_RATE_MICROUSD_PER_MILLION_TOKENS,
};
use crate::activities::{
    ActivityError, ActivityMonetaryBudget, ActivityState, MonetaryBlockedReason,
    MonetaryBudgetDraft, MonetaryReservation, MonetaryReservationRequest, MonetarySettlement,
};

pub(super) const MIGRATE_TO_V6: &str = r#"
CREATE TABLE activity_monetary_budgets (
    activity_id TEXT NOT NULL,
    owner_uid INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    revision INTEGER NOT NULL CHECK(revision > 0),
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    currency TEXT NOT NULL CHECK(currency = 'USD'),
    max_total_microusd INTEGER NOT NULL CHECK(max_total_microusd BETWEEN 1 AND 1000000000000),
    input_rate INTEGER NOT NULL CHECK(input_rate BETWEEN 1 AND 1000000000000),
    output_rate INTEGER NOT NULL CHECK(output_rate BETWEEN 1 AND 1000000000000),
    max_output_tokens INTEGER NOT NULL CHECK(max_output_tokens BETWEEN 1 AND 1000000),
    spent_microusd INTEGER NOT NULL CHECK(spent_microusd >= 0),
    reserved_microusd INTEGER NOT NULL CHECK(reserved_microusd >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(owner_uid, activity_id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id)
);

CREATE TABLE activity_monetary_ledger (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT CHECK(sequence > 0),
    call_id TEXT NOT NULL,
    activity_id TEXT NOT NULL,
    owner_uid INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    job_id TEXT NOT NULL,
    session_id TEXT,
    turn_index INTEGER NOT NULL CHECK(turn_index BETWEEN 0 AND 4294967295),
    policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
    input_upper_bound_tokens INTEGER NOT NULL CHECK(input_upper_bound_tokens >= 0),
    requested_max_output_tokens INTEGER NOT NULL CHECK(requested_max_output_tokens > 0),
    policy_max_output_tokens INTEGER NOT NULL CHECK(policy_max_output_tokens BETWEEN 1 AND 1000000),
    max_output_tokens INTEGER NOT NULL CHECK(max_output_tokens BETWEEN 1 AND 1000000),
    input_rate INTEGER NOT NULL CHECK(input_rate BETWEEN 1 AND 1000000000000),
    output_rate INTEGER NOT NULL CHECK(output_rate BETWEEN 1 AND 1000000000000),
    reserved_microusd INTEGER NOT NULL CHECK(reserved_microusd >= 0),
    charged_microusd INTEGER CHECK(charged_microusd >= 0),
    actual_input_tokens INTEGER CHECK(actual_input_tokens BETWEEN 0 AND 4294967295),
    actual_output_tokens INTEGER CHECK(actual_output_tokens BETWEEN 0 AND 4294967295),
    cache_read_tokens INTEGER CHECK(cache_read_tokens BETWEEN 0 AND 4294967295),
    cache_write_tokens INTEGER CHECK(cache_write_tokens BETWEEN 0 AND 4294967295),
    provider TEXT,
    model TEXT,
    conservative INTEGER CHECK(conservative IN (0, 1)),
    reserved_at TEXT NOT NULL,
    settled_at TEXT,
    UNIQUE(owner_uid, call_id),
    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id),
    FOREIGN KEY(owner_uid, activity_id)
        REFERENCES activity_monetary_budgets(owner_uid, activity_id),
    CHECK (
        (settled_at IS NULL AND charged_microusd IS NULL
            AND actual_input_tokens IS NULL AND actual_output_tokens IS NULL
            AND cache_read_tokens IS NULL AND cache_write_tokens IS NULL
            AND provider IS NULL AND model IS NULL AND conservative IS NULL)
        OR (settled_at IS NOT NULL AND charged_microusd IS NOT NULL
            AND provider IS NOT NULL AND model IS NOT NULL
            AND (
                (conservative = 1 AND actual_input_tokens IS NULL
                    AND actual_output_tokens IS NULL AND cache_read_tokens IS NULL
                    AND cache_write_tokens IS NULL)
                OR
                (conservative = 0 AND actual_input_tokens IS NOT NULL
                    AND actual_output_tokens IS NOT NULL AND cache_read_tokens IS NOT NULL
                    AND cache_write_tokens IS NOT NULL)
            ))
    )
);
CREATE INDEX activity_monetary_ledger_activity
    ON activity_monetary_ledger(owner_uid, activity_id, sequence);
"#;

pub(super) fn get(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
) -> Result<Option<ActivityMonetaryBudget>, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    let conn = service.lock()?;
    load_activity(&conn, owner_uid, &activity_id)?;
    load_policy(&conn, owner_uid, &activity_id)
}

pub(super) fn set(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    expected_revision: Option<u64>,
    draft: MonetaryBudgetDraft,
) -> Result<ActivityMonetaryBudget, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    draft.validate()?;
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    if !matches!(
        activity.state,
        ActivityState::Active | ActivityState::Paused
    ) {
        return blocked(MonetaryBlockedReason::Inactive);
    }
    let existing = load_policy(&tx, owner_uid, &activity_id)?;
    let now = timestamp();
    match (existing, expected_revision) {
        (None, None) => {
            tx.execute(
                "INSERT INTO activity_monetary_budgets (
                    activity_id, owner_uid, revision, enabled, currency, max_total_microusd,
                    input_rate, output_rate, max_output_tokens, spent_microusd,
                    reserved_microusd, created_at, updated_at
                 ) VALUES (?1, ?2, 1, 1, 'USD', ?3, ?4, ?5, ?6, 0, 0, ?7, ?7)",
                params![
                    activity_id,
                    i64::from(owner_uid),
                    sql_u64(draft.max_total_microusd)?,
                    sql_u64(draft.input_microusd_per_million_tokens)?,
                    sql_u64(draft.output_microusd_per_million_tokens)?,
                    i64::from(draft.max_output_tokens_per_turn),
                    now,
                ],
            )?;
        }
        (Some(policy), Some(revision)) if policy.revision == revision => {
            let next = next_revision(revision)?;
            let changed = tx.execute(
                "UPDATE activity_monetary_budgets SET revision=?1, currency='USD',
                    max_total_microusd=?2, input_rate=?3, output_rate=?4,
                    max_output_tokens=?5, updated_at=?6
                 WHERE owner_uid=?7 AND activity_id=?8 AND revision=?9",
                params![
                    sql_u64(next)?,
                    sql_u64(draft.max_total_microusd)?,
                    sql_u64(draft.input_microusd_per_million_tokens)?,
                    sql_u64(draft.output_microusd_per_million_tokens)?,
                    i64::from(draft.max_output_tokens_per_turn),
                    now,
                    i64::from(owner_uid),
                    activity_id,
                    sql_u64(revision)?,
                ],
            )?;
            if changed != 1 {
                return Err(ActivityError::Conflict(
                    "monetary budget revision changed".into(),
                ));
            }
        }
        (Some(_), None) => {
            return Err(ActivityError::Conflict(
                "monetary budget already exists; supply expected_revision to update".into(),
            ))
        }
        (None, Some(_)) => {
            return Err(ActivityError::Conflict(
                "monetary budget does not exist; omit expected_revision to create".into(),
            ))
        }
        (Some(_), Some(_)) => return blocked(MonetaryBlockedReason::Stale),
    }
    let policy = load_policy(&tx, owner_uid, &activity_id)?
        .ok_or_else(|| corrupt("monetary policy disappeared after write"))?;
    tx.commit()?;
    Ok(policy)
}

pub(super) fn set_enabled(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    expected_revision: u64,
    enabled: bool,
) -> Result<ActivityMonetaryBudget, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    if enabled
        && !matches!(
            activity.state,
            ActivityState::Active | ActivityState::Paused
        )
    {
        return blocked(MonetaryBlockedReason::Inactive);
    }
    let policy = load_policy(&tx, owner_uid, &activity_id)?.ok_or(ActivityError::NotFound)?;
    if policy.revision != expected_revision {
        return blocked(MonetaryBlockedReason::Stale);
    }
    let next = next_revision(expected_revision)?;
    let now = timestamp();
    let changed = tx.execute(
        "UPDATE activity_monetary_budgets SET revision=?1, enabled=?2, updated_at=?3
         WHERE owner_uid=?4 AND activity_id=?5 AND revision=?6",
        params![
            sql_u64(next)?,
            i64::from(enabled),
            now,
            i64::from(owner_uid),
            activity_id,
            sql_u64(expected_revision)?,
        ],
    )?;
    if changed != 1 {
        return Err(ActivityError::Conflict(
            "monetary budget revision changed".into(),
        ));
    }
    let result = load_policy(&tx, owner_uid, &activity_id)?
        .ok_or_else(|| corrupt("monetary policy disappeared after enable write"))?;
    tx.commit()?;
    Ok(result)
}

pub(super) fn reserve(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    request: MonetaryReservationRequest,
) -> Result<Option<MonetaryReservation>, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    validate_runtime_identity(&request)?;
    let call_id = uuid::Uuid::parse_str(&request.call_id)
        .map_err(|_| ActivityError::Invalid("monetary call_id must be a UUID".into()))?
        .to_string();
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let activity = load_activity(&tx, owner_uid, &activity_id)?;
    if let Some(existing) = load_reservation(&tx, owner_uid, &call_id)? {
        if existing.activity_id != activity_id
            || existing.job_id != request.job_id
            || existing.session_id != request.session_id
            || existing.turn_index != request.turn_index
            || existing.input_upper_bound_tokens != request.input_upper_bound_tokens
            || existing.requested_max_output_tokens != request.requested_max_output_tokens
        {
            return Err(ActivityError::Conflict(
                "monetary call_id is bound to a different turn identity".into(),
            ));
        }
        load_policy(&tx, owner_uid, &activity_id)?
            .ok_or_else(|| corrupt("monetary reservation exists without policy"))?;
        tx.commit()?;
        return Ok(Some(existing));
    }
    let Some(policy) = load_policy(&tx, owner_uid, &activity_id)? else {
        tx.commit()?;
        return Ok(None);
    };
    if !activity.state.allows_work() {
        return blocked(MonetaryBlockedReason::Inactive);
    }
    if !policy.enabled {
        return blocked(MonetaryBlockedReason::Disabled);
    }
    let input = accounted_cost(
        request.input_upper_bound_tokens,
        policy.budget.input_microusd_per_million_tokens,
    )?;
    let output = accounted_cost(
        u64::from(policy.budget.max_output_tokens_per_turn),
        policy.budget.output_microusd_per_million_tokens,
    )?;
    let amount = input
        .checked_add(output)
        .ok_or_else(|| ActivityError::Invalid("monetary reservation overflow".into()))?;
    let committed = policy
        .spent_microusd
        .checked_add(policy.reserved_microusd)
        .and_then(|value| value.checked_add(amount))
        .ok_or_else(|| corrupt("monetary totals overflow"))?;
    if committed > policy.budget.max_total_microusd {
        return blocked(MonetaryBlockedReason::Exhausted);
    }
    let now = timestamp();
    let max_output_tokens = request
        .requested_max_output_tokens
        .min(policy.budget.max_output_tokens_per_turn);
    let reservation = MonetaryReservation {
        call_id,
        activity_id: activity_id.clone(),
        owner_uid,
        job_id: request.job_id,
        session_id: request.session_id,
        turn_index: request.turn_index,
        policy_revision: policy.revision,
        reserved_microusd: amount,
        input_upper_bound_tokens: request.input_upper_bound_tokens,
        requested_max_output_tokens: request.requested_max_output_tokens,
        policy_max_output_tokens_per_turn: policy.budget.max_output_tokens_per_turn,
        max_output_tokens,
        input_microusd_per_million_tokens: policy.budget.input_microusd_per_million_tokens,
        output_microusd_per_million_tokens: policy.budget.output_microusd_per_million_tokens,
        reserved_at: now.clone(),
    };
    tx.execute(
        "INSERT INTO activity_monetary_ledger (
            call_id, activity_id, owner_uid, job_id, session_id, turn_index,
            policy_revision, input_upper_bound_tokens, requested_max_output_tokens,
            policy_max_output_tokens, max_output_tokens, input_rate, output_rate,
            reserved_microusd, reserved_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            reservation.call_id,
            reservation.activity_id,
            i64::from(owner_uid),
            reservation.job_id,
            reservation.session_id,
            i64::from(reservation.turn_index),
            sql_u64(reservation.policy_revision)?,
            sql_u64(reservation.input_upper_bound_tokens)?,
            i64::from(request.requested_max_output_tokens),
            i64::from(reservation.policy_max_output_tokens_per_turn),
            i64::from(reservation.max_output_tokens),
            sql_u64(reservation.input_microusd_per_million_tokens)?,
            sql_u64(reservation.output_microusd_per_million_tokens)?,
            sql_u64(amount)?,
            reservation.reserved_at,
        ],
    )?;
    tx.execute(
        "UPDATE activity_monetary_budgets SET reserved_microusd=reserved_microusd+?1,
            updated_at=?2 WHERE owner_uid=?3 AND activity_id=?4 AND revision=?5",
        params![
            sql_u64(amount)?,
            now,
            i64::from(owner_uid),
            activity_id,
            sql_u64(policy.revision)?,
        ],
    )?;
    tx.commit()?;
    Ok(Some(reservation))
}

pub(super) fn settle(
    service: &SqliteActivityService,
    owner_uid: u32,
    activity_id: &str,
    call_id: &str,
    job_id: &str,
    session_id: Option<&str>,
    turn_index: u32,
    settlement: MonetarySettlement,
) -> Result<ActivityMonetaryBudget, ActivityError> {
    let activity_id = parse_id(activity_id)?;
    let call_id = uuid::Uuid::parse_str(call_id)
        .map_err(|_| ActivityError::Invalid("monetary call_id must be a UUID".into()))?
        .to_string();
    validate_settlement(&settlement)?;
    let mut conn = service.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    load_activity(&tx, owner_uid, &activity_id)?;
    let stored = load_ledger(&tx, owner_uid, &call_id)?.ok_or(ActivityError::NotFound)?;
    if stored.reservation.activity_id != activity_id {
        return Err(ActivityError::Conflict(
            "monetary call_id belongs to another Activity".into(),
        ));
    }
    if stored.reservation.job_id != job_id
        || stored.reservation.session_id.as_deref() != session_id
        || stored.reservation.turn_index != turn_index
    {
        return Err(ActivityError::Conflict(
            "monetary call_id belongs to another execution turn".into(),
        ));
    }
    let charged = if settlement.conservative {
        stored.reservation.reserved_microusd
    } else {
        accounted_cost(
            u64::from(settlement.input_tokens.unwrap_or(0)),
            stored.reservation.input_microusd_per_million_tokens,
        )?
        .checked_add(accounted_cost(
            u64::from(settlement.output_tokens.unwrap_or(0)),
            stored.reservation.output_microusd_per_million_tokens,
        )?)
        .ok_or_else(|| ActivityError::Invalid("monetary settlement overflow".into()))?
    };
    if let Some(existing) = stored.settlement {
        if existing != settlement || stored.charged_microusd != Some(charged) {
            return Err(ActivityError::Conflict(
                "monetary call_id already has a different settlement".into(),
            ));
        }
        return load_policy(&tx, owner_uid, &activity_id)?
            .ok_or_else(|| corrupt("settled monetary ledger lost its policy"));
    }
    let now = timestamp();
    let changed = tx.execute(
        "UPDATE activity_monetary_ledger SET charged_microusd=?1, actual_input_tokens=?2,
            actual_output_tokens=?3, cache_read_tokens=?4, cache_write_tokens=?5,
            provider=?6, model=?7, conservative=?8, settled_at=?9
         WHERE owner_uid=?10 AND call_id=?11 AND settled_at IS NULL",
        params![
            sql_u64(charged)?,
            settlement.input_tokens.map(i64::from),
            settlement.output_tokens.map(i64::from),
            settlement.cache_read_tokens.map(i64::from),
            settlement.cache_write_tokens.map(i64::from),
            settlement.provider,
            settlement.model,
            i64::from(settlement.conservative),
            now,
            i64::from(owner_uid),
            call_id,
        ],
    )?;
    if changed != 1 {
        return Err(ActivityError::Conflict(
            "monetary settlement changed concurrently".into(),
        ));
    }
    let policy_changed = tx.execute(
        "UPDATE activity_monetary_budgets SET
            reserved_microusd=reserved_microusd-?1,
            spent_microusd=spent_microusd+?2, updated_at=?3
         WHERE owner_uid=?4 AND activity_id=?5 AND reserved_microusd>=?1",
        params![
            sql_u64(stored.reservation.reserved_microusd)?,
            sql_u64(charged)?,
            now,
            i64::from(owner_uid),
            activity_id,
        ],
    )?;
    if policy_changed != 1 {
        return Err(corrupt(
            "monetary settlement did not release its exact reservation",
        ));
    }
    let policy = load_policy(&tx, owner_uid, &activity_id)?
        .ok_or_else(|| corrupt("monetary policy disappeared during settlement"))?;
    tx.commit()?;
    Ok(policy)
}

fn validate_settlement(settlement: &MonetarySettlement) -> Result<(), ActivityError> {
    if settlement.provider.is_empty()
        || settlement.provider.len() > 128
        || settlement.model.is_empty()
        || settlement.model.len() > 256
        || settlement.provider.chars().any(char::is_control)
        || settlement.model.chars().any(char::is_control)
    {
        return Err(ActivityError::Invalid(
            "monetary settlement provider/model is invalid".into(),
        ));
    }
    if settlement.conservative {
        if settlement.input_tokens.is_some()
            || settlement.output_tokens.is_some()
            || settlement.cache_read_tokens.is_some()
            || settlement.cache_write_tokens.is_some()
        {
            return Err(ActivityError::Invalid(
                "conservative monetary settlement must not claim actual usage".into(),
            ));
        }
    } else if settlement.input_tokens.is_none()
        || settlement.output_tokens.is_none()
        || settlement.cache_read_tokens.is_none()
        || settlement.cache_write_tokens.is_none()
    {
        return Err(ActivityError::Invalid(
            "actual monetary settlement requires complete provider usage".into(),
        ));
    }
    Ok(())
}

struct StoredLedger {
    reservation: MonetaryReservation,
    charged_microusd: Option<u64>,
    settlement: Option<MonetarySettlement>,
}

struct LedgerRow {
    activity_id: String,
    job_id: String,
    session_id: Option<String>,
    turn_index: i64,
    policy_revision: i64,
    input_upper_bound_tokens: i64,
    requested_max_output_tokens: i64,
    policy_max_output_tokens: i64,
    max_output_tokens: i64,
    input_rate: i64,
    output_rate: i64,
    reserved_microusd: i64,
    reserved_at: String,
    charged_microusd: Option<i64>,
    actual_input_tokens: Option<i64>,
    actual_output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
    provider: Option<String>,
    model: Option<String>,
    conservative: Option<i64>,
    settled_at: Option<String>,
}

impl LedgerRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            activity_id: row.get(0)?,
            job_id: row.get(1)?,
            session_id: row.get(2)?,
            turn_index: row.get(3)?,
            policy_revision: row.get(4)?,
            input_upper_bound_tokens: row.get(5)?,
            requested_max_output_tokens: row.get(6)?,
            policy_max_output_tokens: row.get(7)?,
            max_output_tokens: row.get(8)?,
            input_rate: row.get(9)?,
            output_rate: row.get(10)?,
            reserved_microusd: row.get(11)?,
            reserved_at: row.get(12)?,
            charged_microusd: row.get(13)?,
            actual_input_tokens: row.get(14)?,
            actual_output_tokens: row.get(15)?,
            cache_read_tokens: row.get(16)?,
            cache_write_tokens: row.get(17)?,
            provider: row.get(18)?,
            model: row.get(19)?,
            conservative: row.get(20)?,
            settled_at: row.get(21)?,
        })
    }

    fn into_stored(self, owner_uid: u32, call_id: &str) -> Result<StoredLedger, ActivityError> {
        let activity_id = parse_id(&self.activity_id).map_err(stored_error)?;
        let turn_index = stored_u32(self.turn_index, "turn index")?;
        let policy_revision = stored_revision(self.policy_revision)?;
        let input_upper_bound_tokens =
            stored_u64(self.input_upper_bound_tokens, "input upper bound")?;
        let requested_max_output_tokens =
            stored_u32(self.requested_max_output_tokens, "requested output limit")?;
        let policy_max_output_tokens_per_turn =
            stored_u32(self.policy_max_output_tokens, "policy output limit")?;
        let max_output_tokens = stored_u32(self.max_output_tokens, "dispatched output limit")?;
        let input_microusd_per_million_tokens = stored_u64(self.input_rate, "input rate")?;
        let output_microusd_per_million_tokens = stored_u64(self.output_rate, "output rate")?;
        let reserved_microusd = stored_u64(self.reserved_microusd, "reservation amount")?;
        let identity = MonetaryReservationRequest {
            call_id: call_id.to_string(),
            job_id: self.job_id.clone(),
            session_id: self.session_id.clone(),
            turn_index,
            input_upper_bound_tokens,
            requested_max_output_tokens,
        };
        validate_runtime_identity(&identity).map_err(stored_error)?;
        if !(1..=MAX_OUTPUT_TOKENS_PER_TURN).contains(&policy_max_output_tokens_per_turn)
            || max_output_tokens
                != requested_max_output_tokens.min(policy_max_output_tokens_per_turn)
            || !(1..=MAX_RATE_MICROUSD_PER_MILLION_TOKENS)
                .contains(&input_microusd_per_million_tokens)
            || !(1..=MAX_RATE_MICROUSD_PER_MILLION_TOKENS)
                .contains(&output_microusd_per_million_tokens)
        {
            return Err(corrupt("invalid stored monetary reservation snapshot"));
        }
        let expected_reserved =
            accounted_cost(input_upper_bound_tokens, input_microusd_per_million_tokens)
                .map_err(stored_error)?
                .checked_add(
                    accounted_cost(
                        u64::from(policy_max_output_tokens_per_turn),
                        output_microusd_per_million_tokens,
                    )
                    .map_err(stored_error)?,
                )
                .ok_or_else(|| corrupt("stored monetary reservation amount overflow"))?;
        if reserved_microusd != expected_reserved {
            return Err(corrupt(
                "stored monetary reservation does not match its pricing snapshot",
            ));
        }
        let reserved_at = parse_timestamp(&self.reserved_at)?;
        let (charged_microusd, settlement) = match self.settled_at {
            None => {
                if self.charged_microusd.is_some()
                    || self.actual_input_tokens.is_some()
                    || self.actual_output_tokens.is_some()
                    || self.cache_read_tokens.is_some()
                    || self.cache_write_tokens.is_some()
                    || self.provider.is_some()
                    || self.model.is_some()
                    || self.conservative.is_some()
                {
                    return Err(corrupt("unsettled monetary row contains settlement data"));
                }
                (None, None)
            }
            Some(settled_at) => {
                if parse_timestamp(&settled_at)? < reserved_at {
                    return Err(corrupt(
                        "monetary settlement timestamp precedes its reservation",
                    ));
                }
                let conservative = match self.conservative {
                    Some(0) => false,
                    Some(1) => true,
                    _ => return Err(corrupt("invalid stored monetary conservative flag")),
                };
                let settlement = MonetarySettlement {
                    input_tokens: stored_optional_u32(
                        self.actual_input_tokens,
                        "actual input tokens",
                    )?,
                    output_tokens: stored_optional_u32(
                        self.actual_output_tokens,
                        "actual output tokens",
                    )?,
                    cache_read_tokens: stored_optional_u32(
                        self.cache_read_tokens,
                        "cache read tokens",
                    )?,
                    cache_write_tokens: stored_optional_u32(
                        self.cache_write_tokens,
                        "cache write tokens",
                    )?,
                    provider: self
                        .provider
                        .ok_or_else(|| corrupt("settled monetary row has no provider"))?,
                    model: self
                        .model
                        .ok_or_else(|| corrupt("settled monetary row has no model"))?,
                    conservative,
                };
                validate_settlement(&settlement).map_err(stored_error)?;
                let charged = stored_u64(
                    self.charged_microusd
                        .ok_or_else(|| corrupt("settled monetary row has no charged amount"))?,
                    "charged amount",
                )?;
                let expected_charged = if conservative {
                    reserved_microusd
                } else {
                    accounted_cost(
                        u64::from(settlement.input_tokens.unwrap_or(0)),
                        input_microusd_per_million_tokens,
                    )
                    .map_err(stored_error)?
                    .checked_add(
                        accounted_cost(
                            u64::from(settlement.output_tokens.unwrap_or(0)),
                            output_microusd_per_million_tokens,
                        )
                        .map_err(stored_error)?,
                    )
                    .ok_or_else(|| corrupt("stored monetary settlement amount overflow"))?
                };
                if charged != expected_charged {
                    return Err(corrupt(
                        "stored monetary charge does not match its settlement",
                    ));
                }
                (Some(charged), Some(settlement))
            }
        };
        Ok(StoredLedger {
            reservation: MonetaryReservation {
                call_id: call_id.to_string(),
                activity_id,
                owner_uid,
                job_id: self.job_id,
                session_id: self.session_id,
                turn_index,
                policy_revision,
                reserved_microusd,
                input_upper_bound_tokens,
                requested_max_output_tokens,
                policy_max_output_tokens_per_turn,
                max_output_tokens,
                input_microusd_per_million_tokens,
                output_microusd_per_million_tokens,
                reserved_at: self.reserved_at,
            },
            charged_microusd,
            settlement,
        })
    }
}

fn load_reservation(
    conn: &Connection,
    owner_uid: u32,
    call_id: &str,
) -> Result<Option<MonetaryReservation>, ActivityError> {
    Ok(load_ledger(conn, owner_uid, call_id)?.map(|stored| stored.reservation))
}

fn load_ledger(
    conn: &Connection,
    owner_uid: u32,
    call_id: &str,
) -> Result<Option<StoredLedger>, ActivityError> {
    conn.query_row(
        "SELECT activity_id, job_id, session_id, turn_index, policy_revision,
            input_upper_bound_tokens, requested_max_output_tokens, policy_max_output_tokens,
            max_output_tokens, input_rate, output_rate, reserved_microusd, reserved_at,
            charged_microusd, actual_input_tokens,
            actual_output_tokens, cache_read_tokens, cache_write_tokens, provider,
            model, conservative, settled_at
         FROM activity_monetary_ledger WHERE owner_uid=?1 AND call_id=?2",
        params![i64::from(owner_uid), call_id],
        LedgerRow::from_row,
    )
    .optional()
    .map_err(ActivityError::from)?
    .map(|row| row.into_stored(owner_uid, call_id))
    .transpose()
}

struct PolicyRow {
    revision: i64,
    enabled: i64,
    currency: String,
    max_total_microusd: i64,
    input_rate: i64,
    output_rate: i64,
    max_output_tokens: i64,
    spent_microusd: i64,
    reserved_microusd: i64,
    created_at: String,
    updated_at: String,
}

impl PolicyRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            revision: row.get(0)?,
            enabled: row.get(1)?,
            currency: row.get(2)?,
            max_total_microusd: row.get(3)?,
            input_rate: row.get(4)?,
            output_rate: row.get(5)?,
            max_output_tokens: row.get(6)?,
            spent_microusd: row.get(7)?,
            reserved_microusd: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    }

    fn into_policy(
        self,
        owner_uid: u32,
        activity_id: &str,
    ) -> Result<ActivityMonetaryBudget, ActivityError> {
        let enabled = match self.enabled {
            0 => false,
            1 => true,
            _ => return Err(corrupt("stored monetary enabled flag is not boolean")),
        };
        let budget = MonetaryBudgetDraft {
            currency: self.currency,
            max_total_microusd: stored_u64(self.max_total_microusd, "maximum total")?,
            input_microusd_per_million_tokens: stored_u64(self.input_rate, "input rate")?,
            output_microusd_per_million_tokens: stored_u64(self.output_rate, "output rate")?,
            max_output_tokens_per_turn: stored_u32(self.max_output_tokens, "output limit")?,
        };
        budget.validate().map_err(stored_error)?;
        let created_at = parse_timestamp(&self.created_at)?;
        let updated_at = parse_timestamp(&self.updated_at)?;
        if updated_at < created_at {
            return Err(corrupt("monetary policy updated_at precedes created_at"));
        }
        Ok(ActivityMonetaryBudget {
            activity_id: activity_id.to_string(),
            owner_uid,
            revision: stored_revision(self.revision)?,
            enabled,
            budget,
            spent_microusd: stored_u64(self.spent_microusd, "spent total")?,
            reserved_microusd: stored_u64(self.reserved_microusd, "reserved total")?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn load_policy(
    conn: &Connection,
    owner_uid: u32,
    activity_id: &str,
) -> Result<Option<ActivityMonetaryBudget>, ActivityError> {
    let row = conn
        .query_row(
            "SELECT revision, enabled, currency, max_total_microusd, input_rate,
            output_rate, max_output_tokens, spent_microusd, reserved_microusd,
            created_at, updated_at FROM activity_monetary_budgets
         WHERE owner_uid=?1 AND activity_id=?2",
            params![i64::from(owner_uid), activity_id],
            PolicyRow::from_row,
        )
        .optional()?;
    let (ledger_count, reserved, spent): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*),
            COALESCE(SUM(CASE WHEN settled_at IS NULL THEN reserved_microusd ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN settled_at IS NOT NULL THEN charged_microusd ELSE 0 END), 0)
         FROM activity_monetary_ledger WHERE owner_uid=?1 AND activity_id=?2",
        params![i64::from(owner_uid), activity_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let Some(row) = row else {
        if ledger_count != 0 {
            return Err(corrupt("monetary ledger exists without its policy"));
        }
        return Ok(None);
    };
    let policy = row.into_policy(owner_uid, activity_id)?;
    if policy.reserved_microusd != stored_u64(reserved, "ledger reserved total")?
        || policy.spent_microusd != stored_u64(spent, "ledger spent total")?
    {
        return Err(corrupt(
            "monetary policy totals do not match the durable ledger",
        ));
    }
    Ok(Some(policy))
}

pub(super) fn validate_schema(conn: &Connection) -> Result<(), ActivityError> {
    conn.prepare(
        "SELECT activity_id, owner_uid, revision, enabled, currency,
        max_total_microusd, input_rate, output_rate, max_output_tokens,
        spent_microusd, reserved_microusd, created_at, updated_at
        FROM activity_monetary_budgets LIMIT 0",
    )?;
    conn.prepare(
        "SELECT call_id, activity_id, owner_uid, job_id, session_id,
        turn_index, policy_revision, policy_max_output_tokens, reserved_microusd, settled_at
        FROM activity_monetary_ledger INDEXED BY activity_monetary_ledger_activity LIMIT 0",
    )?;
    for (table, identity, targets) in [
        (
            "activity_monetary_budgets",
            "activity_id",
            &["activities"][..],
        ),
        (
            "activity_monetary_ledger",
            "call_id",
            &["activities", "activity_monetary_budgets"][..],
        ),
    ] {
        let unique: bool = conn.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM pragma_index_list(?1) AS idx
                WHERE idx.\"unique\" = 1 AND idx.partial = 0
                  AND (SELECT COUNT(*) FROM pragma_index_info(idx.name)) = 2
                  AND EXISTS(SELECT 1 FROM pragma_index_info(idx.name)
                    WHERE seqno = 0 AND name = 'owner_uid')
                  AND EXISTS(SELECT 1 FROM pragma_index_info(idx.name)
                    WHERE seqno = 1 AND name = ?2)
             )",
            params![table, identity],
            |row| row.get(0),
        )?;
        if !unique {
            return Err(corrupt(format!(
                "missing owner-scoped {table} identity constraint"
            )));
        }
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

fn next_revision(revision: u64) -> Result<u64, ActivityError> {
    revision
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or_else(|| ActivityError::Conflict("monetary budget revision is exhausted".into()))
}

fn stored_u32(value: i64, field: &str) -> Result<u32, ActivityError> {
    u32::try_from(value).map_err(|_| corrupt(format!("invalid stored monetary {field}")))
}

fn stored_optional_u32(value: Option<i64>, field: &str) -> Result<Option<u32>, ActivityError> {
    value.map(|value| stored_u32(value, field)).transpose()
}

fn stored_u64(value: i64, field: &str) -> Result<u64, ActivityError> {
    u64::try_from(value).map_err(|_| corrupt(format!("invalid stored monetary {field}")))
}

fn stored_revision(value: i64) -> Result<u64, ActivityError> {
    let revision = stored_u64(value, "policy revision")?;
    if revision == 0 {
        return Err(corrupt("stored monetary policy revision is zero"));
    }
    Ok(revision)
}

fn stored_error(error: ActivityError) -> ActivityError {
    match error {
        ActivityError::Database(_) | ActivityError::Io(_) | ActivityError::Corrupt(_) => error,
        _ => corrupt(error.to_string()),
    }
}

fn sql_u64(value: u64) -> Result<i64, ActivityError> {
    i64::try_from(value)
        .map_err(|_| ActivityError::Invalid("monetary value is not representable".into()))
}

fn corrupt(message: impl Into<String>) -> ActivityError {
    ActivityError::Corrupt(message.into())
}

fn blocked<T>(reason: MonetaryBlockedReason) -> Result<T, ActivityError> {
    Err(ActivityError::MonetaryBlocked(reason))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/sqlite/monetary_budget.rs"
    ));
}
