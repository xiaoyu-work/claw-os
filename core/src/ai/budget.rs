//! Per-app monthly AI spending ledger.
//!
//! Each row records how many abstract billing units and how many US
//! dollars an app has consumed in a given billing period (a calendar
//! month UTC, `YYYY-MM`). The gate calls `reserve` before each upstream
//! call and `charge` after — both are atomic and over-cap reservations
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
    pub usd_used: f64,
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
    #[error(
        "budget exceeded: app `{app}` used ${used:.2} of ${cap:.2} this period"
    )]
    OverDollarCap {
        app: String,
        used: f64,
        cap: f64,
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
                usd_used  REAL    NOT NULL DEFAULT 0.0,
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
        let row: Option<(i64, f64)> = self
            .conn
            .query_row(
                "SELECT units_used, usd_used FROM ai_budget \
                 WHERE app_id = ?1 AND period = ?2",
                params![app, period],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (units_used, usd_used) = row.unwrap_or((0, 0.0));
        Ok(Snapshot {
            period,
            units_used: units_used as u64,
            usd_used,
        })
    }

    /// Reserve capacity. Atomic: returns `OverUnitCap` /
    /// `OverDollarCap` if the post-reserve total would exceed the
    /// caps. `cap_units == 0` disables unit checking; `cap_usd == 0.0`
    /// disables dollar checking — at least one cap should be set in
    /// practice but enforcement is the caller's responsibility.
    pub fn reserve(
        &mut self,
        app: &str,
        units: u64,
        usd: f64,
        cap_units: u64,
        cap_usd: f64,
    ) -> Result<Snapshot, BudgetError> {
        let period = current_period_utc();
        let tx = self.conn.transaction()?;
        let row: Option<(i64, f64)> = tx
            .query_row(
                "SELECT units_used, usd_used FROM ai_budget \
                 WHERE app_id = ?1 AND period = ?2",
                params![app, period],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (cur_units, cur_usd) = row.unwrap_or((0, 0.0));
        let new_units = cur_units as u64 + units;
        let new_usd = cur_usd + usd;
        if cap_units > 0 && new_units > cap_units {
            return Err(BudgetError::OverUnitCap {
                app: app.to_string(),
                used: new_units,
                cap: cap_units,
            });
        }
        if cap_usd > 0.0 && new_usd > cap_usd {
            return Err(BudgetError::OverDollarCap {
                app: app.to_string(),
                used: new_usd,
                cap: cap_usd,
            });
        }
        tx.execute(
            "INSERT INTO ai_budget(app_id, period, units_used, usd_used) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(app_id, period) DO UPDATE SET \
                 units_used = units_used + excluded.units_used, \
                 usd_used   = usd_used   + excluded.usd_used",
            params![app, period, units as i64, usd],
        )?;
        tx.commit()?;
        Ok(Snapshot {
            period,
            units_used: new_units,
            usd_used: new_usd,
        })
    }

    /// Finalise a reservation by adjusting recorded usage to the
    /// actuals. Pass `actual_units - reserved_units` (signed) so
    /// over-estimates roll back. Never errors on bounds — settlement
    /// happens after the call has already been served.
    pub fn settle(
        &mut self,
        app: &str,
        delta_units: i64,
        delta_usd: f64,
    ) -> Result<Snapshot, BudgetError> {
        let period = current_period_utc();
        // Ensure the row exists. The initial values only matter if it
        // didn't — the on-conflict branch takes precedence otherwise.
        self.conn.execute(
            "INSERT INTO ai_budget(app_id, period, units_used, usd_used) \
             VALUES (?1, ?2, 0, 0.0) \
             ON CONFLICT(app_id, period) DO NOTHING",
            params![app, period],
        )?;
        self.conn.execute(
            "UPDATE ai_budget SET \
                 units_used = MAX(0, units_used + ?3), \
                 usd_used   = MAX(0.0, usd_used + ?4) \
             WHERE app_id = ?1 AND period = ?2",
            params![app, period, delta_units, delta_usd],
        )?;
        self.current(app)
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
            "SELECT period, units_used, usd_used FROM ai_budget \
             WHERE app_id = ?1 ORDER BY period DESC",
        )?;
        let rows = stmt
            .query_map(params![app], |r| {
                Ok(Snapshot {
                    period: r.get(0)?,
                    units_used: r.get::<_, i64>(1)? as u64,
                    usd_used: r.get(2)?,
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
                usd_used  REAL    NOT NULL DEFAULT 0.0,
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
        let snap = s.reserve("app1", 100, 0.01, 1000, 1.0).unwrap();
        assert_eq!(snap.units_used, 100);
        assert!((snap.usd_used - 0.01).abs() < 1e-9);
    }

    #[test]
    fn reserve_over_unit_cap_denied() {
        let mut s = temp_store();
        s.reserve("app1", 800, 0.0, 1000, 0.0).unwrap();
        let err = s.reserve("app1", 300, 0.0, 1000, 0.0).unwrap_err();
        match err {
            BudgetError::OverUnitCap { used, cap, .. } => {
                assert_eq!(used, 1100);
                assert_eq!(cap, 1000);
            }
            other => panic!("expected OverUnitCap, got {other:?}"),
        }
    }

    #[test]
    fn reserve_over_dollar_cap_denied() {
        let mut s = temp_store();
        s.reserve("app1", 1, 0.9, 0, 1.0).unwrap();
        let err = s.reserve("app1", 1, 0.2, 0, 1.0).unwrap_err();
        assert!(matches!(err, BudgetError::OverDollarCap { .. }));
    }

    #[test]
    fn settle_rolls_back_overestimate() {
        let mut s = temp_store();
        s.reserve("app1", 100, 0.10, 1000, 1.0).unwrap();
        let snap = s.settle("app1", -40, -0.04).unwrap();
        assert_eq!(snap.units_used, 60);
        assert!((snap.usd_used - 0.06).abs() < 1e-6);
    }

    #[test]
    fn reset_clears_period() {
        let mut s = temp_store();
        s.reserve("app1", 100, 0.10, 1000, 1.0).unwrap();
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
