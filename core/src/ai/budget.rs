//! Per-app monthly AI token ledger.
//!
//! Each row records how many abstract billing units an app has
//! consumed in a given billing period (a calendar month UTC,
//! `YYYY-MM`). The gate calls `reserve` before each upstream call
//! and `settle` after — both are atomic and over-cap reservations
//! are hard-denied.
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
                PRIMARY KEY (app_id, period)
            );
            "#,
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn })
    }

    /// Snapshot for the current period (zero-filled if no row yet).
    pub fn current(&self, app: &str) -> Result<Snapshot, BudgetError> {
        let period = current_period_utc();
        let row: Option<i64> = self
            .conn
            .query_row(
                "SELECT units_used FROM ai_budget \
                 WHERE app_id = ?1 AND period = ?2",
                params![app, period],
                |r| r.get(0),
            )
            .optional()?;
        let units_used = row.unwrap_or(0);
        Ok(Snapshot {
            period,
            units_used: units_used as u64,
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
    /// includes the reserved `period`; pass it to [`Self::settle`]
    /// so settlement always lands on the SAME row even if the UTC
    /// month rolls over mid-call.
    pub fn reserve(
        &mut self,
        app: &str,
        units: u64,
        cap_units: u64,
    ) -> Result<Snapshot, BudgetError> {
        let period = current_period_utc();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<i64> = tx
            .query_row(
                "SELECT units_used FROM ai_budget \
                 WHERE app_id = ?1 AND period = ?2",
                params![app, period],
                |r| r.get(0),
            )
            .optional()?;
        let cur_units = row.unwrap_or(0).max(0) as u64;
        // Saturate on overflow so a giant `units` value (which the
        // gate already saturates to u64::MAX for safety) reliably
        // trips the cap check instead of wrapping to a small number.
        let new_units = cur_units.checked_add(units).unwrap_or(u64::MAX);
        if cap_units > 0 && new_units > cap_units {
            return Err(BudgetError::OverUnitCap {
                app: app.to_string(),
                used: new_units,
                cap: cap_units,
            });
        }
        tx.execute(
            "INSERT INTO ai_budget(app_id, period, units_used) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(app_id, period) DO UPDATE SET \
                 units_used = units_used + excluded.units_used",
            params![app, period, units as i64],
        )?;
        tx.commit()?;
        Ok(Snapshot {
            period,
            units_used: new_units,
        })
    }

    /// Finalise a reservation by adjusting recorded usage to the
    /// actual. Pass `actual_units - reserved_units` (signed) so
    /// over-estimates roll back. Settlement is pinned to `period`
    /// so a request that straddles a UTC month boundary still
    /// settles against the row it reserved against.
    ///
    /// `period` should be the value returned from the matching
    /// [`Self::reserve`] call. Use [`Self::settle_now`] when you
    /// don't have a reservation handy (e.g. ad-hoc adjustments).
    pub fn settle(
        &mut self,
        app: &str,
        period: &str,
        delta_units: i64,
    ) -> Result<Snapshot, BudgetError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO ai_budget(app_id, period, units_used) \
             VALUES (?1, ?2, 0) \
             ON CONFLICT(app_id, period) DO NOTHING",
            params![app, period],
        )?;
        tx.execute(
            "UPDATE ai_budget SET \
                 units_used = MAX(0, units_used + ?3) \
             WHERE app_id = ?1 AND period = ?2",
            params![app, period, delta_units],
        )?;
        let row: Option<i64> = tx
            .query_row(
                "SELECT units_used FROM ai_budget \
                 WHERE app_id = ?1 AND period = ?2",
                params![app, period],
                |r| r.get(0),
            )
            .optional()?;
        tx.commit()?;
        Ok(Snapshot {
            period: period.to_string(),
            units_used: row.unwrap_or(0).max(0) as u64,
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
        let mut stmt = self.conn.prepare(
            "SELECT period, units_used FROM ai_budget \
             WHERE app_id = ?1 ORDER BY period DESC",
        )?;
        let rows = stmt
            .query_map(params![app], |r| {
                Ok(Snapshot {
                    period: r.get(0)?,
                    units_used: r.get::<_, i64>(1)? as u64,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS ai_budget (
                app_id TEXT NOT NULL,
                period TEXT NOT NULL,
                units_used INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (app_id, period)
            );
            "#,
        )
        .unwrap();
        std::mem::forget(dir);
        Store { conn }
    }

    #[test]
    fn reserve_within_cap_ok() {
        let mut s = temp_store();
        let snap = s.reserve("app1", 100, 1000).unwrap();
        assert_eq!(snap.units_used, 100);
    }

    #[test]
    fn reserve_over_unit_cap_denied() {
        let mut s = temp_store();
        s.reserve("app1", 800, 1000).unwrap();
        let err = s.reserve("app1", 300, 1000).unwrap_err();
        match err {
            BudgetError::OverUnitCap { used, cap, .. } => {
                assert_eq!(used, 1100);
                assert_eq!(cap, 1000);
            }
            other => panic!("expected OverUnitCap, got {other:?}"),
        }
    }

    #[test]
    fn settle_rolls_back_overestimate() {
        let mut s = temp_store();
        s.reserve("app1", 100, 1000).unwrap();
        let snap = s.settle_now("app1", -40).unwrap();
        assert_eq!(snap.units_used, 60);
    }

    /// `reserve` returns a `Snapshot` with a `period`; `settle`
    /// pinned to that period must operate on the same row even if
    /// the caller waits long enough to span periods. We can't fake
    /// wall-clock here, but we can verify that the API contract
    /// (pinned-period settle) round-trips.
    #[test]
    fn settle_pinned_to_reserved_period_round_trips() {
        let mut s = temp_store();
        let snap = s.reserve("app1", 200, 10_000).unwrap();
        let after = s.settle("app1", &snap.period, -50).unwrap();
        assert_eq!(after.period, snap.period);
        assert_eq!(after.units_used, 150);
    }

    /// Adding `units` to a row that's already at u64::MAX must
    /// saturate rather than wrap. A malicious caller who can pass
    /// `units == u64::MAX` should be denied via `OverUnitCap`, not
    /// allowed past the cap check by silent wrap-around.
    #[test]
    fn reserve_saturates_on_overflow() {
        let mut s = temp_store();
        let err = s.reserve("app1", u64::MAX, 1_000_000).unwrap_err();
        match err {
            BudgetError::OverUnitCap { used, .. } => assert_eq!(used, u64::MAX),
            other => panic!("expected OverUnitCap, got {other:?}"),
        }
    }

    #[test]
    fn reset_clears_period() {
        let mut s = temp_store();
        s.reserve("app1", 100, 1000).unwrap();
        s.reset("app1").unwrap();
        let snap = s.current("app1").unwrap();
        assert_eq!(snap.units_used, 0);
    }

    #[test]
    fn period_is_yyyy_mm() {
        let p = current_period_utc();
        assert_eq!(p.len(), 7);
        let bytes = p.as_bytes();
        assert_eq!(bytes[4], b'-');
    }
}
