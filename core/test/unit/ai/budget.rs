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
