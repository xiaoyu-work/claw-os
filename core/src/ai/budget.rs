//! Per-app monthly AI token ledger.
//!
//! Each row records committed and in-flight abstract billing units for
//! an app in a calendar month UTC (`YYYY-MM`). The gate reserves before
//! each upstream call and atomically converts that reservation to actual
//! usage afterward. Cap checks always use `used + reserved`, so concurrent
//! calls cannot oversell the same remaining capacity.
//!
//! The store lives at `${COS_DATA_DIR}/ai_budget.db`. Writing is
//! serialised through SQLite's own locking; readers see consistent
//! snapshots.

use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// Current snapshot for an `(app, period)` row.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub period: String,
    pub units_used: u64,
}

/// Why a reservation was rejected.
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    #[error("budget store error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("budget exceeded: app `{app}` used {used} of {cap} units this period")]
    OverUnitCap {
        app: String,
        used: u64,
        cap: u64,
    },
    #[error("budget units exceed the SQLite integer storage range")]
    StorageRange,
    #[error(
        "budget reservation mismatch for app `{app}`: tried to settle {requested} units, \
         but only {reserved} are reserved"
    )]
    ReservationMismatch {
        app: String,
        requested: u64,
        reserved: u64,
    },
}

pub struct Store {
    conn: Connection,
}

fn db_path() -> PathBuf {
    crate::paths::data_dir().join("ai_budget.db")
}

fn current_period_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (year, month) = ymd_from_unix(secs);
    format!("{year:04}-{month:02}")
}

/// Cheap month decomposition (no chrono dep). Good enough for billing
/// periods — leap seconds and pre-1970 are not in scope.
fn ymd_from_unix(mut secs: i64) -> (i32, u32) {
    if secs < 0 {
        secs = 0;
    }
    let days = secs / 86_400;
    let mut year = 1970i32;
    let mut remaining = days;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if remaining < dy {
            break;
        }
        remaining -= dy;
        year += 1;
    }
    let months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let leap_extra = if is_leap(year) { 1 } else { 0 };
    let mut month = 1u32;
    let mut left = remaining;
    for (idx, days_in_month) in months.iter().enumerate() {
        let dm = *days_in_month + if idx == 1 { leap_extra } else { 0 };
        if left < dm as i64 {
            break;
        }
        left -= dm as i64;
        month += 1;
    }
    (year, month)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn has_reserved_column(conn: &Connection) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(ai_budget)")?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == "units_reserved" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_reserved_column(conn: &Connection) -> rusqlite::Result<()> {
    if has_reserved_column(conn)? {
        return Ok(());
    }
    if let Err(error) = conn.execute(
        "ALTER TABLE ai_budget ADD COLUMN units_reserved INTEGER NOT NULL DEFAULT 0",
        [],
    ) {
        if !has_reserved_column(conn)? {
            return Err(error);
        }
    }
    Ok(())
}

impl Store {
    /// Open (and create on first use) the budget store.
    pub fn open() -> Result<Self, String> {
        let path = db_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(&path).map_err(|e| e.to_string())?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS ai_budget (
                app_id TEXT NOT NULL,
                period TEXT NOT NULL,
                units_used INTEGER NOT NULL DEFAULT 0,
                units_reserved INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (app_id, period)
            );
            "#,
        )
        .map_err(|e| e.to_string())?;
        ensure_reserved_column(&conn).map_err(|e| e.to_string())?;
        Ok(Self { conn })
    }

    /// Snapshot for the current period (zero-filled if no row yet).
    pub fn current(&self, app: &str) -> Result<Snapshot, BudgetError> {
        ensure_reserved_column(&self.conn)?;
        let period = current_period_utc();
        let row: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT units_used, units_reserved FROM ai_budget \
                 WHERE app_id = ?1 AND period = ?2",
                params![app, period],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (used, reserved) = row.unwrap_or((0, 0));
        Ok(Snapshot {
            period,
            units_used: (used.max(0) as u64)
                .saturating_add(reserved.max(0) as u64),
        })
    }

    /// Reserve capacity. Atomic: returns `OverUnitCap` if the
    /// post-reserve total would exceed the cap. `cap_units == 0`
    /// disables checking; enforcement is the caller's responsibility.
    ///
    /// Uses `BEGIN IMMEDIATE` so the read-then-write is serialized
    /// against concurrent reservers (the default `DEFERRED` mode
    /// would only acquire a SHARED lock for the SELECT and let
    /// another writer slip in before the INSERT, allowing two
    /// reservations to both pass the cap check). Returned snapshot
    /// includes the reserved `period`; pass it to [`Self::settle_reservation`]
    /// so settlement always lands on the SAME row even if the UTC
    /// month rolls over mid-call.
    pub fn reserve(
        &mut self,
        app: &str,
        units: u64,
        cap_units: u64,
    ) -> Result<Snapshot, BudgetError> {
        let period = current_period_utc();
        self.reserve_in_period(app, &period, units, cap_units)
    }

    /// Reserve against an explicit period for administrative or migration
    /// workflows that must not use the current UTC month.
    pub fn reserve_in_period(
        &mut self,
        app: &str,
        period: &str,
        units: u64,
        cap_units: u64,
    ) -> Result<Snapshot, BudgetError> {
        ensure_reserved_column(&self.conn)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<(i64, i64)> = tx
            .query_row(
                "SELECT units_used, units_reserved FROM ai_budget \
                 WHERE app_id = ?1 AND period = ?2",
                params![app, period],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (used, reserved) = row.unwrap_or((0, 0));
        let used = used.max(0) as u64;
        let reserved = reserved.max(0) as u64;
        let cur_units = used.saturating_add(reserved);
        // Saturate on overflow so a giant `units` value (which the
        // gate already saturates to u64::MAX for safety) reliably
        // trips the cap check instead of wrapping to a small number.
        let new_units = cur_units.saturating_add(units);
        if cap_units > 0 && new_units > cap_units {
            return Err(BudgetError::OverUnitCap {
                app: app.to_string(),
                used: new_units,
                cap: cap_units,
            });
        }
        let new_reserved = reserved.checked_add(units).ok_or(BudgetError::StorageRange)?;
        let stored_reserved =
            i64::try_from(new_reserved).map_err(|_| BudgetError::StorageRange)?;
        tx.execute(
            "INSERT INTO ai_budget(app_id, period, units_used, units_reserved) \
             VALUES (?1, ?2, 0, ?3) \
             ON CONFLICT(app_id, period) DO UPDATE SET \
                 units_reserved = excluded.units_reserved",
            params![app, period, stored_reserved],
        )?;
        tx.commit()?;
        Ok(Snapshot {
            period: period.to_string(),
            units_used: new_units,
        })
    }

    /// Atomically reserve the same request estimate across multiple budget
    /// buckets (for example the per-app and aggregate per-user rows).
    pub fn reserve_buckets(
        &mut self,
        buckets: &[(&str, u64)],
        units: u64,
    ) -> Result<Snapshot, BudgetError> {
        if buckets.is_empty() {
            return Err(BudgetError::StorageRange);
        }
        ensure_reserved_column(&self.conn)?;
        let period = current_period_utc();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut updates = Vec::with_capacity(buckets.len());
        for &(app, cap_units) in buckets {
            let row: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT units_used, units_reserved FROM ai_budget \
                     WHERE app_id = ?1 AND period = ?2",
                    params![app, period],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (used, reserved) = row.unwrap_or((0, 0));
            let used = used.max(0) as u64;
            let reserved = reserved.max(0) as u64;
            let total = used
                .checked_add(reserved)
                .and_then(|value| value.checked_add(units))
                .unwrap_or(u64::MAX);
            if cap_units > 0 && total > cap_units {
                return Err(BudgetError::OverUnitCap {
                    app: app.to_string(),
                    used: total,
                    cap: cap_units,
                });
            }
            let new_reserved = reserved
                .checked_add(units)
                .ok_or(BudgetError::StorageRange)?;
            let stored_reserved =
                i64::try_from(new_reserved).map_err(|_| BudgetError::StorageRange)?;
            updates.push((app.to_string(), stored_reserved, total));
        }
        for (app, stored_reserved, _) in &updates {
            tx.execute(
                "INSERT INTO ai_budget(app_id, period, units_used, units_reserved) \
                 VALUES (?1, ?2, 0, ?3) \
                 ON CONFLICT(app_id, period) DO UPDATE SET \
                     units_reserved = excluded.units_reserved",
                params![app, period, stored_reserved],
            )?;
        }
        tx.commit()?;
        Ok(Snapshot {
            period,
            units_used: updates[0].2,
        })
    }

    /// Apply a legacy signed adjustment. Hot-path callers should use
    /// [`Self::settle_reservation`] so one request releases exactly its
    /// own reservation while preserving other in-flight calls.
    pub fn settle(
        &mut self,
        app: &str,
        period: &str,
        delta_units: i64,
    ) -> Result<Snapshot, BudgetError> {
        self.settle_capped(app, period, delta_units, 0)
    }

    /// Apply an ad-hoc signed adjustment while preserving a hard cap.
    /// Negative adjustments consume reservations before committed usage.
    pub fn settle_capped(
        &mut self,
        app: &str,
        period: &str,
        delta_units: i64,
        cap_units: u64,
    ) -> Result<Snapshot, BudgetError> {
        ensure_reserved_column(&self.conn)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO ai_budget(app_id, period, units_used, units_reserved) \
             VALUES (?1, ?2, 0, 0) \
             ON CONFLICT(app_id, period) DO NOTHING",
            params![app, period],
        )?;
        let (current_used, current_reserved): (i64, i64) = tx.query_row(
            "SELECT units_used, units_reserved FROM ai_budget \
             WHERE app_id = ?1 AND period = ?2",
            params![app, period],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut used = current_used.max(0) as u64;
        let mut reserved = current_reserved.max(0) as u64;
        if delta_units >= 0 {
            used = used
                .checked_add(delta_units as u64)
                .ok_or(BudgetError::StorageRange)?;
        } else {
            let reduction = delta_units.unsigned_abs();
            let from_reserved = reduction.min(reserved);
            reserved -= from_reserved;
            used = used.saturating_sub(reduction - from_reserved);
        }
        let total = used.checked_add(reserved).ok_or(BudgetError::StorageRange)?;
        let stored_used = i64::try_from(used).map_err(|_| BudgetError::StorageRange)?;
        let stored_reserved =
            i64::try_from(reserved).map_err(|_| BudgetError::StorageRange)?;
        tx.execute(
            "UPDATE ai_budget SET units_used = ?3, units_reserved = ?4 \
             WHERE app_id = ?1 AND period = ?2",
            params![app, period, stored_used, stored_reserved],
        )?;
        tx.commit()?;
        if cap_units > 0 && total > cap_units {
            return Err(BudgetError::OverUnitCap {
                app: app.to_string(),
                used: total,
                cap: cap_units,
            });
        }
        Ok(Snapshot {
            period: period.to_string(),
            units_used: total,
        })
    }

    /// Settle an estimate to an unsigned actual value without lossy casts.
    pub fn settle_reservation(
        &mut self,
        app: &str,
        period: &str,
        reserved_units: u64,
        actual_units: u64,
        cap_units: u64,
    ) -> Result<Snapshot, BudgetError> {
        ensure_reserved_column(&self.conn)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO ai_budget(app_id, period, units_used, units_reserved) \
             VALUES (?1, ?2, 0, 0) \
             ON CONFLICT(app_id, period) DO NOTHING",
            params![app, period],
        )?;
        let (used, reserved): (i64, i64) = tx.query_row(
            "SELECT units_used, units_reserved FROM ai_budget \
             WHERE app_id = ?1 AND period = ?2",
            params![app, period],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let used = used.max(0) as u64;
        let reserved = reserved.max(0) as u64;
        if reserved < reserved_units {
            return Err(BudgetError::ReservationMismatch {
                app: app.to_string(),
                requested: reserved_units,
                reserved,
            });
        }
        let new_used = used
            .checked_add(actual_units)
            .ok_or(BudgetError::StorageRange)?;
        let new_reserved = reserved - reserved_units;
        let total = new_used
            .checked_add(new_reserved)
            .ok_or(BudgetError::StorageRange)?;
        let stored_used =
            i64::try_from(new_used).map_err(|_| BudgetError::StorageRange)?;
        let stored_reserved =
            i64::try_from(new_reserved).map_err(|_| BudgetError::StorageRange)?;
        tx.execute(
            "UPDATE ai_budget SET units_used = ?3, units_reserved = ?4 \
             WHERE app_id = ?1 AND period = ?2",
            params![app, period, stored_used, stored_reserved],
        )?;
        tx.commit()?;
        if cap_units > 0 && total > cap_units {
            return Err(BudgetError::OverUnitCap {
                app: app.to_string(),
                used: total,
                cap: cap_units,
            });
        }
        Ok(Snapshot {
            period: period.to_string(),
            units_used: total,
        })
    }

    /// Atomically settle multiple buckets for one request. Every entry is
    /// `(app_id, reserved_units, actual_units, cap_units)`.
    pub fn settle_reservations(
        &mut self,
        period: &str,
        buckets: &[(&str, u64, u64, u64)],
    ) -> Result<Snapshot, BudgetError> {
        if buckets.is_empty() {
            return Err(BudgetError::StorageRange);
        }
        ensure_reserved_column(&self.conn)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut updates = Vec::with_capacity(buckets.len());
        let mut cap_error: Option<BudgetError> = None;
        for &(app, reserved_units, actual_units, cap_units) in buckets {
            let row: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT units_used, units_reserved FROM ai_budget \
                     WHERE app_id = ?1 AND period = ?2",
                    params![app, period],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (used, reserved) = row.unwrap_or((0, 0));
            let used = used.max(0) as u64;
            let reserved = reserved.max(0) as u64;
            if reserved < reserved_units {
                return Err(BudgetError::ReservationMismatch {
                    app: app.to_string(),
                    requested: reserved_units,
                    reserved,
                });
            }
            let new_used = used
                .checked_add(actual_units)
                .ok_or(BudgetError::StorageRange)?;
            let new_reserved = reserved - reserved_units;
            let total = new_used
                .checked_add(new_reserved)
                .ok_or(BudgetError::StorageRange)?;
            let stored_used =
                i64::try_from(new_used).map_err(|_| BudgetError::StorageRange)?;
            let stored_reserved =
                i64::try_from(new_reserved).map_err(|_| BudgetError::StorageRange)?;
            if cap_error.is_none() && cap_units > 0 && total > cap_units {
                cap_error = Some(BudgetError::OverUnitCap {
                    app: app.to_string(),
                    used: total,
                    cap: cap_units,
                });
            }
            updates.push((
                app.to_string(),
                stored_used,
                stored_reserved,
                total,
            ));
        }
        for (app, stored_used, stored_reserved, _) in &updates {
            tx.execute(
                "UPDATE ai_budget SET units_used = ?3, units_reserved = ?4 \
                 WHERE app_id = ?1 AND period = ?2",
                params![app, period, stored_used, stored_reserved],
            )?;
        }
        tx.commit()?;
        if let Some(error) = cap_error {
            return Err(error);
        }
        Ok(Snapshot {
            period: period.to_string(),
            units_used: updates[0].3,
        })
    }

    /// Convenience wrapper for `settle(app, &current_period_utc(),
    /// delta)`. Use this only when there is no matching reservation
    /// to pin against — e.g. one-off corrections from an admin CLI.
    /// Hot-path callers should pin to `Snapshot.period`.
    pub fn settle_now(
        &mut self,
        app: &str,
        delta_units: i64,
    ) -> Result<Snapshot, BudgetError> {
        let period = current_period_utc();
        self.settle(app, &period, delta_units)
    }

    /// Reset the current period for an app (used by manual rollover
    /// and tests). Deletes the row; the next `reserve` re-creates it.
    pub fn reset(&self, app: &str) -> Result<(), BudgetError> {
        let period = current_period_utc();
        self.conn.execute(
            "DELETE FROM ai_budget WHERE app_id = ?1 AND period = ?2",
            params![app, period],
        )?;
        Ok(())
    }

    /// All recorded periods for an app, newest first.
    pub fn history(&self, app: &str) -> Result<Vec<Snapshot>, BudgetError> {
        ensure_reserved_column(&self.conn)?;
        let mut stmt = self.conn.prepare(
            "SELECT period, units_used, units_reserved FROM ai_budget \
             WHERE app_id = ?1 ORDER BY period DESC",
        )?;
        let rows = stmt
            .query_map(params![app], |r| {
                Ok(Snapshot {
                    period: r.get(0)?,
                    units_used: (r.get::<_, i64>(1)?.max(0) as u64)
                        .saturating_add(r.get::<_, i64>(2)?.max(0) as u64),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/ai/budget.rs"
    ));
}
