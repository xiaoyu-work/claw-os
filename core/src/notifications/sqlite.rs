use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Timelike;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use super::{
    ChangeBatch, DeliveryChannel, DeliveryClaim, DeliveryPolicy, DeliveryResult, DeliveryState,
    DeliveryStatus, Notification, NotificationAction, NotificationChange, NotificationDraft,
    NotificationError, NotificationMutation, NotificationPreferences, NotificationService,
    NotificationState, Severity, DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT, SCHEMA_VERSION,
};

const DEDUPE_WINDOW_MS: i64 = 15 * 60 * 1_000;
const MAX_ACTIVE_PER_OWNER: i64 = 1_000;
const DELIVERY_ERROR_CODE_MAX: usize = 128;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA busy_timeout = 5000;
PRAGMA foreign_keys = ON;
PRAGMA wal_autocheckpoint = 1000;
PRAGMA user_version = 1;

CREATE TABLE IF NOT EXISTS notifications (
    sequence            INTEGER PRIMARY KEY AUTOINCREMENT,
    id                  TEXT NOT NULL UNIQUE,
    owner_uid           INTEGER NOT NULL,
    source              TEXT NOT NULL,
    kind                TEXT NOT NULL,
    severity            INTEGER NOT NULL,
    title               TEXT NOT NULL,
    body                TEXT NOT NULL,
    delivery_policy     INTEGER NOT NULL,
    dedupe_key          TEXT,
    task_id             TEXT,
    session_id          TEXT,
    job_id              TEXT,
    state               INTEGER NOT NULL,
    occurrences         INTEGER NOT NULL DEFAULT 1,
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL,
    expires_at_ms       INTEGER,
    read_at_ms          INTEGER,
    acknowledged_at_ms INTEGER,
    dismissed_at_ms     INTEGER,
    actions_json        TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS notifications_owner_updated
    ON notifications(owner_uid, updated_at_ms DESC);
CREATE INDEX IF NOT EXISTS notifications_owner_dedupe
    ON notifications(owner_uid, dedupe_key, updated_at_ms DESC);

CREATE TABLE IF NOT EXISTS notification_changes (
    cursor          INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_uid       INTEGER NOT NULL,
    notification_id TEXT NOT NULL,
    change_kind     TEXT NOT NULL,
    changed_at_ms   INTEGER NOT NULL,
    FOREIGN KEY(notification_id) REFERENCES notifications(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS notification_changes_owner_cursor
    ON notification_changes(owner_uid, cursor);

CREATE TABLE IF NOT EXISTS notification_deliveries (
    notification_id    TEXT NOT NULL,
    channel            INTEGER NOT NULL,
    state              INTEGER NOT NULL,
    attempts           INTEGER NOT NULL DEFAULT 0,
    next_attempt_at_ms INTEGER,
    delivered_at_ms    INTEGER,
    last_error_code    TEXT,
    PRIMARY KEY(notification_id, channel),
    FOREIGN KEY(notification_id) REFERENCES notifications(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS notification_delivery_queue
    ON notification_deliveries(channel, state, next_attempt_at_ms);

CREATE TABLE IF NOT EXISTS notification_preferences (
    owner_uid                INTEGER PRIMARY KEY,
    web_enabled              INTEGER NOT NULL,
    desktop_enabled          INTEGER NOT NULL,
    ntfy_enabled             INTEGER NOT NULL,
    web_min_severity         INTEGER NOT NULL,
    desktop_min_severity     INTEGER NOT NULL,
    ntfy_min_severity        INTEGER NOT NULL,
    muted_kinds_json         TEXT NOT NULL,
    dnd_start_minute_utc     INTEGER,
    dnd_end_minute_utc       INTEGER,
    critical_bypasses_dnd    INTEGER NOT NULL,
    retention_days           INTEGER NOT NULL,
    ntfy_server              TEXT NOT NULL,
    ntfy_topic               TEXT
);
"#;

#[derive(Clone)]
pub struct SqliteNotificationService {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteNotificationService {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, NotificationError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            crate::storage::ensure_private_dir(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        crate::storage::set_private_file(path)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn open_in_memory() -> Result<Self, NotificationError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, NotificationError> {
        self.conn.lock().map_err(|_| NotificationError::Poisoned)
    }
}

impl NotificationService for SqliteNotificationService {
    fn publish(
        &self,
        owner_uid: u32,
        draft: NotificationDraft,
    ) -> Result<Notification, NotificationError> {
        draft.validate()?;
        let now = super::now_ms();
        let actions_json = serde_json::to_string(&draft.actions)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let preferences = preferences_tx(&tx, owner_uid)?;
        preferences.validate()?;
        let existing = match draft.dedupe_key.as_deref() {
            Some(key) => tx
                .query_row(
                    "SELECT id FROM notifications
                     WHERE owner_uid = ?1 AND dedupe_key = ?2
                       AND state != ?3
                       AND (expires_at_ms IS NULL OR expires_at_ms > ?4)
                       AND updated_at_ms >= ?5
                     ORDER BY updated_at_ms DESC LIMIT 1",
                    params![
                        owner_uid,
                        key,
                        notification_state_code(NotificationState::Dismissed),
                        now,
                        now - DEDUPE_WINDOW_MS
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()?,
            None => None,
        };

        let (id, change_kind) = if let Some(id) = existing {
            tx.execute(
                "UPDATE notifications
                 SET source = ?1, kind = ?2, severity = ?3, title = ?4,
                     body = ?5, delivery_policy = ?6, task_id = ?7,
                     session_id = ?8, job_id = ?9, updated_at_ms = ?10,
                     expires_at_ms = ?11, actions_json = ?12,
                     state = ?13, read_at_ms = NULL,
                     acknowledged_at_ms = NULL, dismissed_at_ms = NULL,
                     occurrences = occurrences + 1
                 WHERE id = ?14 AND owner_uid = ?15",
                params![
                    draft.source,
                    draft.kind,
                    severity_code(draft.severity),
                    draft.title,
                    draft.body,
                    delivery_policy_code(draft.delivery_policy),
                    draft.task_id,
                    draft.session_id,
                    draft.job_id,
                    now,
                    draft.expires_at_ms,
                    actions_json,
                    notification_state_code(NotificationState::Unread),
                    id,
                    owner_uid
                ],
            )?;
            (id, "updated")
        } else {
            let id = format!("notif-{}", uuid::Uuid::new_v4().simple());
            tx.execute(
                "INSERT INTO notifications (
                    id, owner_uid, source, kind, severity, title, body,
                    delivery_policy, dedupe_key, task_id, session_id, job_id,
                    state, occurrences, created_at_ms, updated_at_ms,
                    expires_at_ms, actions_json
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                    ?13, 1, ?14, ?14, ?15, ?16
                 )",
                params![
                    id,
                    owner_uid,
                    draft.source,
                    draft.kind,
                    severity_code(draft.severity),
                    draft.title,
                    draft.body,
                    delivery_policy_code(draft.delivery_policy),
                    draft.dedupe_key,
                    draft.task_id,
                    draft.session_id,
                    draft.job_id,
                    notification_state_code(NotificationState::Unread),
                    now,
                    draft.expires_at_ms,
                    actions_json
                ],
            )?;
            (id, "published")
        };

        route_deliveries_tx(&tx, &id, &draft, &preferences, now)?;
        insert_change_tx(&tx, owner_uid, &id, change_kind, now)?;
        prune_tx(&tx, owner_uid, preferences.retention_days, now)?;
        tx.commit()?;
        load_notification(&conn, owner_uid, &id)
    }

    fn list(
        &self,
        owner_uid: u32,
        include_dismissed: bool,
        limit: usize,
    ) -> Result<Vec<Notification>, NotificationError> {
        let conn = self.lock()?;
        let now = super::now_ms();
        let limit = normalize_limit(limit);
        let mut statement = conn.prepare(
            "SELECT id FROM notifications
             WHERE owner_uid = ?1
               AND (?2 = 1 OR state != ?3)
               AND (expires_at_ms IS NULL OR expires_at_ms > ?4)
             ORDER BY updated_at_ms DESC, sequence DESC
             LIMIT ?5",
        )?;
        let ids = statement
            .query_map(
                params![
                    owner_uid,
                    include_dismissed as i64,
                    notification_state_code(NotificationState::Dismissed),
                    now,
                    limit as i64
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        ids.into_iter()
            .map(|id| load_notification(&conn, owner_uid, &id))
            .collect()
    }

    fn changes(
        &self,
        owner_uid: u32,
        after_cursor: u64,
        limit: usize,
    ) -> Result<ChangeBatch, NotificationError> {
        let conn = self.lock()?;
        let limit = normalize_limit(limit);
        let mut statement = conn.prepare(
            "SELECT cursor, notification_id, change_kind
             FROM notification_changes
             WHERE owner_uid = ?1 AND cursor > ?2
             ORDER BY cursor ASC
             LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![owner_uid, u64_to_i64(after_cursor), limit as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut cursor = after_cursor;
        let mut changes = Vec::with_capacity(rows.len());
        for (raw_cursor, id, change) in rows {
            cursor = raw_cursor.max(0) as u64;
            match load_notification(&conn, owner_uid, &id) {
                Ok(notification) => changes.push(NotificationChange {
                    cursor,
                    change,
                    notification,
                }),
                Err(NotificationError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(ChangeBatch { cursor, changes })
    }

    fn cursor(&self, owner_uid: u32) -> Result<u64, NotificationError> {
        let conn = self.lock()?;
        let value = conn.query_row(
            "SELECT COALESCE(MAX(cursor), 0) FROM notification_changes WHERE owner_uid = ?1",
            params![owner_uid],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(value.max(0) as u64)
    }

    fn mutate(
        &self,
        owner_uid: u32,
        id: &str,
        mutation: NotificationMutation,
    ) -> Result<Notification, NotificationError> {
        let now = super::now_ms();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, read_at, acknowledged_at, dismissed_at, change) = match mutation {
            NotificationMutation::Read => (NotificationState::Read, Some(now), None, None, "read"),
            NotificationMutation::Acknowledge => (
                NotificationState::Acknowledged,
                Some(now),
                Some(now),
                None,
                "acknowledged",
            ),
            NotificationMutation::Dismiss => (
                NotificationState::Dismissed,
                Some(now),
                None,
                Some(now),
                "dismissed",
            ),
        };
        let changed = tx.execute(
            "UPDATE notifications
             SET state = ?1,
                 read_at_ms = COALESCE(?2, read_at_ms),
                 acknowledged_at_ms = COALESCE(?3, acknowledged_at_ms),
                 dismissed_at_ms = COALESCE(?4, dismissed_at_ms),
                 updated_at_ms = ?5
             WHERE id = ?6 AND owner_uid = ?7",
            params![
                notification_state_code(state),
                read_at,
                acknowledged_at,
                dismissed_at,
                now,
                id,
                owner_uid
            ],
        )?;
        if changed == 0 {
            return Err(NotificationError::NotFound);
        }
        if matches!(mutation, NotificationMutation::Dismiss) {
            tx.execute(
                "UPDATE notification_deliveries
                 SET state = ?1, next_attempt_at_ms = NULL
                 WHERE notification_id = ?2 AND state IN (?3, ?4, ?5)",
                params![
                    delivery_state_code(DeliveryState::Suppressed),
                    id,
                    delivery_state_code(DeliveryState::Queued),
                    delivery_state_code(DeliveryState::Delivering),
                    delivery_state_code(DeliveryState::Failed)
                ],
            )?;
        }
        insert_change_tx(&tx, owner_uid, id, change, now)?;
        tx.commit()?;
        load_notification(&conn, owner_uid, id)
    }

    fn preferences(&self, owner_uid: u32) -> Result<NotificationPreferences, NotificationError> {
        let conn = self.lock()?;
        preferences_conn(&conn, owner_uid)
    }

    fn set_preferences(
        &self,
        owner_uid: u32,
        preferences: NotificationPreferences,
    ) -> Result<NotificationPreferences, NotificationError> {
        preferences.validate()?;
        let muted_kinds_json = serde_json::to_string(&preferences.muted_kinds)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO notification_preferences (
                owner_uid, web_enabled, desktop_enabled, ntfy_enabled,
                web_min_severity, desktop_min_severity, ntfy_min_severity,
                muted_kinds_json,
                dnd_start_minute_utc, dnd_end_minute_utc,
                critical_bypasses_dnd, retention_days, ntfy_server, ntfy_topic
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(owner_uid) DO UPDATE SET
                web_enabled = excluded.web_enabled,
                desktop_enabled = excluded.desktop_enabled,
                ntfy_enabled = excluded.ntfy_enabled,
                web_min_severity = excluded.web_min_severity,
                desktop_min_severity = excluded.desktop_min_severity,
                ntfy_min_severity = excluded.ntfy_min_severity,
                muted_kinds_json = excluded.muted_kinds_json,
                dnd_start_minute_utc = excluded.dnd_start_minute_utc,
                dnd_end_minute_utc = excluded.dnd_end_minute_utc,
                critical_bypasses_dnd = excluded.critical_bypasses_dnd,
                retention_days = excluded.retention_days,
                ntfy_server = excluded.ntfy_server,
                ntfy_topic = excluded.ntfy_topic",
            params![
                owner_uid,
                preferences.web_enabled as i64,
                preferences.desktop_enabled as i64,
                preferences.ntfy_enabled as i64,
                severity_code(preferences.web_min_severity),
                severity_code(preferences.desktop_min_severity),
                severity_code(preferences.ntfy_min_severity),
                muted_kinds_json,
                preferences.dnd_start_minute_utc.map(i64::from),
                preferences.dnd_end_minute_utc.map(i64::from),
                preferences.critical_bypasses_dnd as i64,
                i64::from(preferences.retention_days),
                preferences.ntfy_server,
                preferences.ntfy_topic
            ],
        )?;
        reconcile_preferences_tx(&tx, owner_uid, &preferences, super::now_ms())?;
        tx.commit()?;
        Ok(preferences)
    }

    fn claim_deliveries(
        &self,
        owner_uid: Option<u32>,
        channel: DeliveryChannel,
        limit: usize,
        lease_ms: i64,
    ) -> Result<Vec<DeliveryClaim>, NotificationError> {
        if lease_ms <= 0 {
            return Err(NotificationError::Invalid(
                "delivery lease must be positive".to_string(),
            ));
        }
        let now = super::now_ms();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE notification_deliveries
             SET state = ?1
             WHERE channel = ?2 AND state = ?3
               AND next_attempt_at_ms IS NOT NULL
               AND next_attempt_at_ms <= ?4",
            params![
                delivery_state_code(DeliveryState::Queued),
                delivery_channel_code(channel),
                delivery_state_code(DeliveryState::Delivering),
                now
            ],
        )?;
        let limit = normalize_limit(limit);
        let ids = if let Some(owner_uid) = owner_uid {
            let mut statement = tx.prepare(
                "SELECT d.notification_id, n.owner_uid, d.attempts
                 FROM notification_deliveries d
                 JOIN notifications n ON n.id = d.notification_id
                 WHERE n.owner_uid = ?1 AND d.channel = ?2
                   AND d.state IN (?3, ?4)
                   AND (d.next_attempt_at_ms IS NULL OR d.next_attempt_at_ms <= ?5)
                   AND n.state != ?6
                   AND (n.expires_at_ms IS NULL OR n.expires_at_ms > ?5)
                 ORDER BY n.updated_at_ms ASC
                 LIMIT ?7",
            )?;
            let rows = statement
                .query_map(
                    params![
                        owner_uid,
                        delivery_channel_code(channel),
                        delivery_state_code(DeliveryState::Queued),
                        delivery_state_code(DeliveryState::Failed),
                        now,
                        notification_state_code(NotificationState::Dismissed),
                        limit as i64
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, u32>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        } else {
            let mut statement = tx.prepare(
                "SELECT d.notification_id, n.owner_uid, d.attempts
                 FROM notification_deliveries d
                 JOIN notifications n ON n.id = d.notification_id
                 WHERE d.channel = ?1
                   AND d.state IN (?2, ?3)
                   AND (d.next_attempt_at_ms IS NULL OR d.next_attempt_at_ms <= ?4)
                   AND n.state != ?5
                   AND (n.expires_at_ms IS NULL OR n.expires_at_ms > ?4)
                 ORDER BY n.updated_at_ms ASC
                 LIMIT ?6",
            )?;
            let rows = statement
                .query_map(
                    params![
                        delivery_channel_code(channel),
                        delivery_state_code(DeliveryState::Queued),
                        delivery_state_code(DeliveryState::Failed),
                        now,
                        notification_state_code(NotificationState::Dismissed),
                        limit as i64
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, u32>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (id, _, _) in &ids {
            tx.execute(
                "UPDATE notification_deliveries
                 SET state = ?1, attempts = attempts + 1,
                     next_attempt_at_ms = ?2, last_error_code = NULL
                 WHERE notification_id = ?3 AND channel = ?4",
                params![
                    delivery_state_code(DeliveryState::Delivering),
                    now.saturating_add(lease_ms),
                    id,
                    delivery_channel_code(channel)
                ],
            )?;
        }
        tx.commit()?;

        let mut claims = Vec::with_capacity(ids.len());
        for (id, delivery_owner_uid, previous_attempts) in ids {
            let notification = load_notification(&conn, delivery_owner_uid, &id)?;
            claims.push(DeliveryClaim {
                notification,
                channel,
                attempts: previous_attempts.saturating_add(1).max(0) as u32,
            });
        }
        Ok(claims)
    }

    fn complete_delivery(
        &self,
        owner_uid: u32,
        id: &str,
        channel: DeliveryChannel,
        result: DeliveryResult,
    ) -> Result<Notification, NotificationError> {
        let now = super::now_ms();
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, next_attempt, delivered_at, error_code, change) = match result {
            DeliveryResult::Delivered => {
                (DeliveryState::Delivered, None, Some(now), None, "delivered")
            }
            DeliveryResult::Suppressed => (
                DeliveryState::Suppressed,
                None,
                None,
                None,
                "delivery_suppressed",
            ),
            DeliveryResult::Failed {
                error_code,
                retry_at_ms,
            } => {
                if error_code.is_empty()
                    || error_code.len() > DELIVERY_ERROR_CODE_MAX
                    || !error_code.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
                {
                    return Err(NotificationError::Invalid(
                        "delivery error code is invalid".to_string(),
                    ));
                }
                (
                    DeliveryState::Failed,
                    Some(retry_at_ms),
                    None,
                    Some(error_code),
                    "delivery_failed",
                )
            }
        };
        let terminal = matches!(state, DeliveryState::Delivered | DeliveryState::Suppressed);
        let changed = tx.execute(
            "UPDATE notification_deliveries
             SET state = ?1, next_attempt_at_ms = ?2,
                 delivered_at_ms = ?3, last_error_code = ?4
             WHERE notification_id = ?5 AND channel = ?6
               AND EXISTS (
                   SELECT 1 FROM notifications n
                   WHERE n.id = ?5 AND n.owner_uid = ?7
               )
               AND NOT (state = ?1 AND ?8 = 1)",
            params![
                delivery_state_code(state),
                next_attempt,
                delivered_at,
                error_code,
                id,
                delivery_channel_code(channel),
                owner_uid,
                terminal as i64
            ],
        )?;
        if changed == 0 {
            let exists = tx
                .query_row(
                    "SELECT 1
                     FROM notification_deliveries d
                     JOIN notifications n ON n.id = d.notification_id
                     WHERE d.notification_id = ?1 AND d.channel = ?2
                       AND n.owner_uid = ?3",
                    params![id, delivery_channel_code(channel), owner_uid],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !exists {
                return Err(NotificationError::NotFound);
            }
            let notification = load_notification(&tx, owner_uid, id)?;
            tx.commit()?;
            return Ok(notification);
        }
        insert_change_tx(&tx, owner_uid, id, change, now)?;
        tx.commit()?;
        load_notification(&conn, owner_uid, id)
    }

    fn known_owner_uids(&self) -> Result<Vec<u32>, NotificationError> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT owner_uid FROM notifications
             UNION
             SELECT owner_uid FROM notification_preferences
             ORDER BY owner_uid",
        )?;
        let owners = statement
            .query_map([], |row| row.get::<_, u32>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(owners)
    }
}

fn route_deliveries_tx(
    tx: &Transaction<'_>,
    id: &str,
    draft: &NotificationDraft,
    preferences: &NotificationPreferences,
    now_ms: i64,
) -> Result<(), NotificationError> {
    let dnd = dnd_active(preferences, now_ms)
        && !(draft.severity == Severity::Critical && preferences.critical_bypasses_dnd);
    if preferences
        .muted_kinds
        .iter()
        .any(|kind| kind == &draft.kind)
    {
        tx.execute(
            "UPDATE notification_deliveries
             SET state = ?1, next_attempt_at_ms = NULL
             WHERE notification_id = ?2 AND state != ?3",
            params![
                delivery_state_code(DeliveryState::Suppressed),
                id,
                delivery_state_code(DeliveryState::Delivered)
            ],
        )?;
        return Ok(());
    }
    let candidates = [
        (
            DeliveryChannel::Web,
            preferences.web_enabled && draft.severity >= preferences.web_min_severity,
            false,
        ),
        (
            DeliveryChannel::Desktop,
            preferences.desktop_enabled
                && draft.delivery_policy == DeliveryPolicy::Immediate
                && draft.severity >= preferences.desktop_min_severity,
            dnd,
        ),
        (
            DeliveryChannel::Ntfy,
            preferences.ntfy_enabled
                && draft.delivery_policy == DeliveryPolicy::Immediate
                && draft.severity >= preferences.ntfy_min_severity,
            dnd,
        ),
    ];
    for (channel, enabled, suppressed) in candidates {
        if !enabled {
            tx.execute(
                "UPDATE notification_deliveries
                 SET state = ?1, next_attempt_at_ms = NULL
                 WHERE notification_id = ?2 AND channel = ?3 AND state != ?4",
                params![
                    delivery_state_code(DeliveryState::Suppressed),
                    id,
                    delivery_channel_code(channel),
                    delivery_state_code(DeliveryState::Delivered)
                ],
            )?;
            continue;
        }
        let state = if suppressed {
            DeliveryState::Suppressed
        } else {
            DeliveryState::Queued
        };
        tx.execute(
            "INSERT INTO notification_deliveries (
                notification_id, channel, state, attempts,
                next_attempt_at_ms, delivered_at_ms, last_error_code
             ) VALUES (?1, ?2, ?3, 0, NULL, NULL, NULL)
             ON CONFLICT(notification_id, channel) DO UPDATE SET
                state = excluded.state,
                attempts = 0,
                next_attempt_at_ms = NULL,
                delivered_at_ms = NULL,
                last_error_code = NULL",
            params![
                id,
                delivery_channel_code(channel),
                delivery_state_code(state)
            ],
        )?;
    }
    Ok(())
}

fn reconcile_preferences_tx(
    tx: &Transaction<'_>,
    owner_uid: u32,
    preferences: &NotificationPreferences,
    now_ms: i64,
) -> Result<(), NotificationError> {
    let dnd = dnd_active(preferences, now_ms);
    for (channel, suppress, keep_critical) in [
        (DeliveryChannel::Web, !preferences.web_enabled, false),
        (
            DeliveryChannel::Desktop,
            !preferences.desktop_enabled || dnd,
            dnd && preferences.critical_bypasses_dnd,
        ),
        (
            DeliveryChannel::Ntfy,
            !preferences.ntfy_enabled || dnd,
            dnd && preferences.critical_bypasses_dnd,
        ),
    ] {
        if !suppress {
            continue;
        }
        tx.execute(
            "UPDATE notification_deliveries
             SET state = ?1, next_attempt_at_ms = NULL
             WHERE channel = ?2
               AND notification_id IN (
                   SELECT id FROM notifications
                   WHERE owner_uid = ?3 AND (?4 = 0 OR severity != ?5)
               )
               AND state != ?6",
            params![
                delivery_state_code(DeliveryState::Suppressed),
                delivery_channel_code(channel),
                owner_uid,
                keep_critical as i64,
                severity_code(Severity::Critical),
                delivery_state_code(DeliveryState::Delivered)
            ],
        )?;
    }
    for (channel, minimum) in [
        (DeliveryChannel::Web, preferences.web_min_severity),
        (DeliveryChannel::Desktop, preferences.desktop_min_severity),
        (DeliveryChannel::Ntfy, preferences.ntfy_min_severity),
    ] {
        tx.execute(
            "UPDATE notification_deliveries
             SET state = ?1, next_attempt_at_ms = NULL
             WHERE channel = ?2
               AND notification_id IN (
                   SELECT id FROM notifications
                   WHERE owner_uid = ?3 AND severity < ?4
               )
               AND state != ?5",
            params![
                delivery_state_code(DeliveryState::Suppressed),
                delivery_channel_code(channel),
                owner_uid,
                severity_code(minimum),
                delivery_state_code(DeliveryState::Delivered)
            ],
        )?;
    }
    for kind in &preferences.muted_kinds {
        tx.execute(
            "UPDATE notification_deliveries
             SET state = ?1, next_attempt_at_ms = NULL
             WHERE notification_id IN (
                 SELECT id FROM notifications WHERE owner_uid = ?2 AND kind = ?3
             )
             AND state != ?4",
            params![
                delivery_state_code(DeliveryState::Suppressed),
                owner_uid,
                kind,
                delivery_state_code(DeliveryState::Delivered)
            ],
        )?;
    }
    Ok(())
}

fn dnd_active(preferences: &NotificationPreferences, now_ms: i64) -> bool {
    let (Some(start), Some(end)) = (
        preferences.dnd_start_minute_utc,
        preferences.dnd_end_minute_utc,
    ) else {
        return false;
    };
    let Some(now) = chrono::DateTime::from_timestamp_millis(now_ms) else {
        return false;
    };
    let minute = (now.hour() * 60 + now.minute()) as u16;
    if start == end {
        true
    } else if start < end {
        minute >= start && minute < end
    } else {
        minute >= start || minute < end
    }
}

fn preferences_conn(
    conn: &Connection,
    owner_uid: u32,
) -> Result<NotificationPreferences, NotificationError> {
    conn.query_row(
        "SELECT web_enabled, desktop_enabled, ntfy_enabled,
                web_min_severity, desktop_min_severity, ntfy_min_severity,
                muted_kinds_json,
                dnd_start_minute_utc, dnd_end_minute_utc,
                critical_bypasses_dnd, retention_days, ntfy_server, ntfy_topic
         FROM notification_preferences WHERE owner_uid = ?1",
        params![owner_uid],
        |row| {
            Ok(NotificationPreferences {
                web_enabled: row.get::<_, i64>(0)? != 0,
                desktop_enabled: row.get::<_, i64>(1)? != 0,
                ntfy_enabled: row.get::<_, i64>(2)? != 0,
                web_min_severity: severity_from_code(row.get(3)?)?,
                desktop_min_severity: severity_from_code(row.get(4)?)?,
                ntfy_min_severity: severity_from_code(row.get(5)?)?,
                muted_kinds: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(6)?)
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                dnd_start_minute_utc: row.get::<_, Option<i64>>(7)?.map(|value| value as u16),
                dnd_end_minute_utc: row.get::<_, Option<i64>>(8)?.map(|value| value as u16),
                critical_bypasses_dnd: row.get::<_, i64>(9)? != 0,
                retention_days: row.get::<_, i64>(10)? as u16,
                ntfy_server: row.get(11)?,
                ntfy_topic: row.get(12)?,
            })
        },
    )
    .optional()?
    .map(Ok)
    .unwrap_or_else(|| Ok(NotificationPreferences::default()))
}

fn preferences_tx(
    tx: &Transaction<'_>,
    owner_uid: u32,
) -> Result<NotificationPreferences, NotificationError> {
    preferences_conn(tx, owner_uid)
}

fn insert_change_tx(
    tx: &Transaction<'_>,
    owner_uid: u32,
    id: &str,
    change: &str,
    now: i64,
) -> Result<(), NotificationError> {
    tx.execute(
        "INSERT INTO notification_changes (
            owner_uid, notification_id, change_kind, changed_at_ms
         ) VALUES (?1, ?2, ?3, ?4)",
        params![owner_uid, id, change, now],
    )?;
    Ok(())
}

fn prune_tx(
    tx: &Transaction<'_>,
    owner_uid: u32,
    retention_days: u16,
    now: i64,
) -> Result<(), NotificationError> {
    let cutoff = now.saturating_sub(i64::from(retention_days) * 24 * 60 * 60 * 1_000);
    tx.execute(
        "DELETE FROM notifications
         WHERE owner_uid = ?1
           AND (
               created_at_ms < ?2
               OR (dismissed_at_ms IS NOT NULL AND dismissed_at_ms < ?2)
           )",
        params![owner_uid, cutoff],
    )?;
    tx.execute(
        "DELETE FROM notifications
         WHERE owner_uid = ?1 AND sequence NOT IN (
             SELECT sequence FROM notifications
             WHERE owner_uid = ?1
             ORDER BY updated_at_ms DESC, sequence DESC
             LIMIT ?2
         )",
        params![owner_uid, MAX_ACTIVE_PER_OWNER],
    )?;
    Ok(())
}

fn load_notification(
    conn: &Connection,
    owner_uid: u32,
    id: &str,
) -> Result<Notification, NotificationError> {
    let row = conn
        .query_row(
            "SELECT sequence, id, owner_uid, source, kind, severity, title, body,
                    delivery_policy, dedupe_key, task_id, session_id, job_id,
                    state, occurrences, created_at_ms, updated_at_ms,
                    expires_at_ms, read_at_ms, acknowledged_at_ms,
                    dismissed_at_ms, actions_json
             FROM notifications WHERE id = ?1 AND owner_uid = ?2",
            params![id, owner_uid],
            |row| {
                Ok(NotificationRow {
                    sequence: row.get(0)?,
                    id: row.get(1)?,
                    owner_uid: row.get(2)?,
                    source: row.get(3)?,
                    kind: row.get(4)?,
                    severity: row.get(5)?,
                    title: row.get(6)?,
                    body: row.get(7)?,
                    delivery_policy: row.get(8)?,
                    dedupe_key: row.get(9)?,
                    task_id: row.get(10)?,
                    session_id: row.get(11)?,
                    job_id: row.get(12)?,
                    state: row.get(13)?,
                    occurrences: row.get(14)?,
                    created_at_ms: row.get(15)?,
                    updated_at_ms: row.get(16)?,
                    expires_at_ms: row.get(17)?,
                    read_at_ms: row.get(18)?,
                    acknowledged_at_ms: row.get(19)?,
                    dismissed_at_ms: row.get(20)?,
                    actions_json: row.get(21)?,
                })
            },
        )
        .optional()?
        .ok_or(NotificationError::NotFound)?;

    let mut statement = conn.prepare(
        "SELECT channel, state, attempts, next_attempt_at_ms,
                delivered_at_ms, last_error_code
         FROM notification_deliveries
         WHERE notification_id = ?1 ORDER BY channel",
    )?;
    let deliveries = statement
        .query_map(params![id], |row| {
            Ok(DeliveryRow {
                channel: row.get(0)?,
                state: row.get(1)?,
                attempts: row.get(2)?,
                next_attempt_at_ms: row.get(3)?,
                delivered_at_ms: row.get(4)?,
                last_error_code: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(DeliveryStatus::try_from)
        .collect::<Result<Vec<_>, _>>()?;

    row.into_notification(deliveries)
}

struct NotificationRow {
    sequence: i64,
    id: String,
    owner_uid: u32,
    source: String,
    kind: String,
    severity: i64,
    title: String,
    body: String,
    delivery_policy: i64,
    dedupe_key: Option<String>,
    task_id: Option<String>,
    session_id: Option<String>,
    job_id: Option<String>,
    state: i64,
    occurrences: i64,
    created_at_ms: i64,
    updated_at_ms: i64,
    expires_at_ms: Option<i64>,
    read_at_ms: Option<i64>,
    acknowledged_at_ms: Option<i64>,
    dismissed_at_ms: Option<i64>,
    actions_json: String,
}

impl NotificationRow {
    fn into_notification(
        self,
        deliveries: Vec<DeliveryStatus>,
    ) -> Result<Notification, NotificationError> {
        Ok(Notification {
            schema: SCHEMA_VERSION,
            sequence: self.sequence.max(0) as u64,
            id: self.id,
            owner_uid: self.owner_uid,
            source: self.source,
            kind: self.kind,
            severity: severity_from_code(self.severity)
                .map_err(|error| NotificationError::Invalid(error.to_string()))?,
            title: self.title,
            body: self.body,
            delivery_policy: delivery_policy_from_code(self.delivery_policy)?,
            dedupe_key: self.dedupe_key,
            task_id: self.task_id,
            session_id: self.session_id,
            job_id: self.job_id,
            state: notification_state_from_code(self.state)?,
            occurrences: self.occurrences.max(0) as u32,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            expires_at_ms: self.expires_at_ms,
            read_at_ms: self.read_at_ms,
            acknowledged_at_ms: self.acknowledged_at_ms,
            dismissed_at_ms: self.dismissed_at_ms,
            actions: serde_json::from_str::<Vec<NotificationAction>>(&self.actions_json)?,
            deliveries,
        })
    }
}

struct DeliveryRow {
    channel: i64,
    state: i64,
    attempts: i64,
    next_attempt_at_ms: Option<i64>,
    delivered_at_ms: Option<i64>,
    last_error_code: Option<String>,
}

impl TryFrom<DeliveryRow> for DeliveryStatus {
    type Error = NotificationError;

    fn try_from(row: DeliveryRow) -> Result<Self, Self::Error> {
        Ok(Self {
            channel: delivery_channel_from_code(row.channel)?,
            state: delivery_state_from_code(row.state)?,
            attempts: row.attempts.max(0) as u32,
            next_attempt_at_ms: row.next_attempt_at_ms,
            delivered_at_ms: row.delivered_at_ms,
            last_error_code: row.last_error_code,
        })
    }
}

fn normalize_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_LIST_LIMIT
    } else {
        limit.min(MAX_LIST_LIMIT)
    }
}

fn u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn severity_code(value: Severity) -> i64 {
    match value {
        Severity::Info => 0,
        Severity::Warning => 1,
        Severity::Error => 2,
        Severity::Critical => 3,
    }
}

fn severity_from_code(value: i64) -> rusqlite::Result<Severity> {
    match value {
        0 => Ok(Severity::Info),
        1 => Ok(Severity::Warning),
        2 => Ok(Severity::Error),
        3 => Ok(Severity::Critical),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(0, value)),
    }
}

fn delivery_policy_code(value: DeliveryPolicy) -> i64 {
    match value {
        DeliveryPolicy::Activity => 0,
        DeliveryPolicy::Immediate => 1,
    }
}

fn delivery_policy_from_code(value: i64) -> Result<DeliveryPolicy, NotificationError> {
    match value {
        0 => Ok(DeliveryPolicy::Activity),
        1 => Ok(DeliveryPolicy::Immediate),
        _ => Err(NotificationError::Invalid(
            "stored delivery policy is invalid".to_string(),
        )),
    }
}

fn notification_state_code(value: NotificationState) -> i64 {
    match value {
        NotificationState::Unread => 0,
        NotificationState::Read => 1,
        NotificationState::Acknowledged => 2,
        NotificationState::Dismissed => 3,
    }
}

fn notification_state_from_code(value: i64) -> Result<NotificationState, NotificationError> {
    match value {
        0 => Ok(NotificationState::Unread),
        1 => Ok(NotificationState::Read),
        2 => Ok(NotificationState::Acknowledged),
        3 => Ok(NotificationState::Dismissed),
        _ => Err(NotificationError::Invalid(
            "stored notification state is invalid".to_string(),
        )),
    }
}

fn delivery_channel_code(value: DeliveryChannel) -> i64 {
    match value {
        DeliveryChannel::Web => 0,
        DeliveryChannel::Desktop => 1,
        DeliveryChannel::Ntfy => 2,
    }
}

fn delivery_channel_from_code(value: i64) -> Result<DeliveryChannel, NotificationError> {
    match value {
        0 => Ok(DeliveryChannel::Web),
        1 => Ok(DeliveryChannel::Desktop),
        2 => Ok(DeliveryChannel::Ntfy),
        _ => Err(NotificationError::Invalid(
            "stored delivery channel is invalid".to_string(),
        )),
    }
}

fn delivery_state_code(value: DeliveryState) -> i64 {
    match value {
        DeliveryState::Queued => 0,
        DeliveryState::Delivering => 1,
        DeliveryState::Delivered => 2,
        DeliveryState::Failed => 3,
        DeliveryState::Suppressed => 4,
    }
}

fn delivery_state_from_code(value: i64) -> Result<DeliveryState, NotificationError> {
    match value {
        0 => Ok(DeliveryState::Queued),
        1 => Ok(DeliveryState::Delivering),
        2 => Ok(DeliveryState::Delivered),
        3 => Ok(DeliveryState::Failed),
        4 => Ok(DeliveryState::Suppressed),
        _ => Err(NotificationError::Invalid(
            "stored delivery state is invalid".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/notifications/sqlite.rs"
    ));
}
