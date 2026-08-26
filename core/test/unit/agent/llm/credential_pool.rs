use super::*;

fn p(name: &str, val: &str) -> PoolEntry {
    PoolEntry::from_env(name.to_string(), val.to_string())
}

fn pool(values: &[(&str, &str)], strat: SelectionStrategy) -> Pool {
    Pool::from_entries("test", values.iter().map(|(n, v)| p(n, v)).collect(), strat).unwrap()
}

// ---- construction ------------------------------------------------------

#[test]
fn from_entries_rejects_empty() {
    let err = Pool::from_entries("z", vec![], SelectionStrategy::Sticky).unwrap_err();
    assert!(matches!(err, PoolError::Empty { .. }));
}

#[test]
fn from_entries_rejects_too_many() {
    let entries: Vec<_> = (0..(MAX_POOL_SIZE + 1))
        .map(|i| PoolEntry::inline(format!("k{i}")))
        .collect();
    let err = Pool::from_entries("z", entries, SelectionStrategy::Sticky).unwrap_err();
    assert!(matches!(err, PoolError::InvalidEntry { .. }));
}

#[test]
fn from_entries_rejects_empty_value() {
    let err = Pool::from_entries(
        "z",
        vec![PoolEntry::inline("ok"), PoolEntry::inline("   ")],
        SelectionStrategy::Sticky,
    )
    .unwrap_err();
    match err {
        PoolError::InvalidEntry { reason, .. } => {
            assert!(reason.contains("entry #1"), "got: {reason}");
        }
        other => panic!("expected InvalidEntry, got {other:?}"),
    }
}

#[test]
fn from_sources_inline_only() {
    let pool = Pool::from_sources(
        "x",
        &[],
        &[],
        &["sk-a", "  ", "sk-b"],
        SelectionStrategy::Sticky,
    )
    .unwrap();
    assert_eq!(pool.len(), 2); // empty entry skipped
}

#[test]
fn from_sources_env_resolves() {
    std::env::set_var("COS_TEST_POOL_KEY_A", "sk-aa");
    std::env::set_var("COS_TEST_POOL_KEY_B", "sk-bb");
    let pool = Pool::from_sources(
        "envpool",
        &[],
        &["COS_TEST_POOL_KEY_A", "COS_TEST_POOL_KEY_B"],
        &[],
        SelectionStrategy::RoundRobin,
    )
    .unwrap();
    assert_eq!(pool.len(), 2);
    std::env::remove_var("COS_TEST_POOL_KEY_A");
    std::env::remove_var("COS_TEST_POOL_KEY_B");
}

#[test]
fn from_sources_skips_missing_env() {
    std::env::remove_var("COS_TEST_POOL_KEY_MISSING_X");
    let err = Pool::from_sources(
        "missing",
        &[],
        &["COS_TEST_POOL_KEY_MISSING_X"],
        &[],
        SelectionStrategy::Sticky,
    )
    .unwrap_err();
    assert!(matches!(err, PoolError::Empty { .. }));
}

// ---- selection: sticky -------------------------------------------------

#[test]
fn sticky_returns_first_repeatedly() {
    let pool = pool(&[("E1", "k1"), ("E2", "k2")], SelectionStrategy::Sticky);
    for _ in 0..5 {
        let l = pool.acquire().unwrap();
        assert_eq!(l.value(), "k1");
        pool.report_success(&l);
    }
}

#[test]
fn sticky_skips_cooled_down_first_key() {
    let pool = pool(&[("E1", "k1"), ("E2", "k2")], SelectionStrategy::Sticky);
    let l1 = pool.acquire().unwrap();
    assert_eq!(l1.value(), "k1");
    pool.report_failure(&l1, FailureClass::CooldownWorthy);
    let l2 = pool.acquire().unwrap();
    assert_eq!(l2.value(), "k2");
}

// ---- selection: round-robin --------------------------------------------

#[test]
fn round_robin_rotates_through_all() {
    let pool = pool(
        &[("A", "kA"), ("B", "kB"), ("C", "kC")],
        SelectionStrategy::RoundRobin,
    );
    let mut seq = vec![];
    for _ in 0..6 {
        let l = pool.acquire().unwrap();
        seq.push(l.value().to_string());
        pool.report_success(&l);
    }
    assert_eq!(
        seq,
        vec![
            "kA".to_string(),
            "kB".to_string(),
            "kC".to_string(),
            "kA".to_string(),
            "kB".to_string(),
            "kC".to_string(),
        ]
    );
}

#[test]
fn round_robin_skips_keys_in_cooldown() {
    let pool = pool(
        &[("A", "kA"), ("B", "kB"), ("C", "kC")],
        SelectionStrategy::RoundRobin,
    );
    let l = pool.acquire().unwrap();
    assert_eq!(l.value(), "kA");
    pool.report_failure(&l, FailureClass::CooldownWorthy);
    let l2 = pool.acquire().unwrap();
    assert_eq!(l2.value(), "kB");
    let l3 = pool.acquire().unwrap();
    assert_eq!(l3.value(), "kC");
    let l4 = pool.acquire().unwrap();
    // wraps around — kA still cooling down, picks kB
    assert_eq!(l4.value(), "kB");
}

// ---- selection: least-errors -------------------------------------------

#[test]
fn least_errors_picks_healthier_key() {
    let pool = pool(&[("A", "kA"), ("B", "kB")], SelectionStrategy::LeastErrors);
    let l1 = pool.acquire().unwrap();
    // Tie -> first
    assert_eq!(l1.value(), "kA");
    pool.report_failure(&l1, FailureClass::Transient);
    let l2 = pool.acquire().unwrap();
    assert_eq!(l2.value(), "kB");
}

// ---- failure classes ---------------------------------------------------

#[test]
fn caller_error_does_not_count_as_failure() {
    let pool = pool(&[("A", "kA")], SelectionStrategy::Sticky);
    let l = pool.acquire().unwrap();
    pool.report_failure(&l, FailureClass::CallerError);
    let s = pool.stats();
    assert_eq!(s[0].failures, 0);
    assert_eq!(s[0].successes, 1);
}

#[test]
fn transient_failure_no_cooldown() {
    let pool = pool(&[("A", "kA")], SelectionStrategy::Sticky);
    let l = pool.acquire().unwrap();
    pool.report_failure(&l, FailureClass::Transient);
    let s = pool.stats();
    assert_eq!(s[0].failures, 1);
    assert!(s[0].cooldown_remaining_ms.is_none());
}

#[test]
fn cooldown_worthy_sets_cooldown() {
    let pool = pool(&[("A", "kA")], SelectionStrategy::Sticky);
    let l = pool.acquire().unwrap();
    pool.report_failure(&l, FailureClass::CooldownWorthy);
    let s = pool.stats();
    assert!(s[0].cooldown_remaining_ms.is_some());
}

#[test]
fn all_cooling_down_returns_error_with_wait_hint() {
    let pool = pool(&[("A", "kA"), ("B", "kB")], SelectionStrategy::RoundRobin);
    let l1 = pool.acquire().unwrap();
    pool.report_failure(&l1, FailureClass::CooldownWorthy);
    let l2 = pool.acquire().unwrap();
    pool.report_failure(&l2, FailureClass::CooldownWorthy);
    let err = pool.acquire().unwrap_err();
    match err {
        PoolError::AllCoolingDown { wait_ms, size, .. } => {
            assert_eq!(size, 2);
            // 0 < wait_ms < cooldown duration
            assert!(wait_ms > 0);
            assert!(wait_ms <= DEFAULT_COOLDOWN.as_millis());
        }
        other => panic!("expected AllCoolingDown, got {other:?}"),
    }
}

#[test]
fn cooldown_expires_at_specified_time() {
    let mut p_ = pool(&[("A", "kA")], SelectionStrategy::Sticky);
    p_.set_cooldown(Duration::from_millis(50));
    let now = Instant::now();
    let l = p_.acquire_at(now).unwrap();
    p_.report_failure_at(&l, FailureClass::CooldownWorthy, now);
    // Right at cooldown boundary -> blocked
    let err = p_.acquire_at(now + Duration::from_millis(49)).unwrap_err();
    assert!(matches!(err, PoolError::AllCoolingDown { .. }));
    // Past cooldown -> available
    let l2 = p_.acquire_at(now + Duration::from_millis(60)).unwrap();
    assert_eq!(l2.value(), "kA");
}

#[test]
fn cooldown_zero_disables_cooldown() {
    let mut p_ = pool(&[("A", "kA")], SelectionStrategy::Sticky);
    p_.set_cooldown(Duration::ZERO);
    let l = p_.acquire().unwrap();
    p_.report_failure(&l, FailureClass::CooldownWorthy);
    // Counted as failure but immediately available again.
    let s = p_.stats();
    assert_eq!(s[0].failures, 1);
    assert!(s[0].cooldown_remaining_ms.is_none());
    let l2 = p_.acquire().unwrap();
    assert_eq!(l2.value(), "kA");
}

// ---- success report ----------------------------------------------------

#[test]
fn success_clears_last_failure_class_but_not_failures_count() {
    let pool = pool(&[("A", "kA")], SelectionStrategy::Sticky);
    let l = pool.acquire().unwrap();
    pool.report_failure(&l, FailureClass::Transient);
    let s = pool.stats();
    assert_eq!(s[0].last_failure_class, Some(FailureClass::Transient));
    let l2 = pool.acquire().unwrap();
    pool.report_success(&l2);
    let s2 = pool.stats();
    assert!(s2[0].last_failure_class.is_none());
    assert_eq!(s2[0].failures, 1, "lifetime failures preserved");
}

// ---- aggregate ---------------------------------------------------------

#[test]
fn aggregate_sums_across_keys() {
    let pool = pool(&[("A", "kA"), ("B", "kB")], SelectionStrategy::RoundRobin);
    for _ in 0..3 {
        let l = pool.acquire().unwrap();
        pool.report_success(&l);
    }
    let l = pool.acquire().unwrap();
    pool.report_failure(&l, FailureClass::Transient);
    let agg = pool.aggregate();
    assert_eq!(agg.size, 2);
    assert_eq!(agg.total_successes, 3);
    assert_eq!(agg.total_failures, 1);
    assert_eq!(agg.last_failure_by_class.get("transient"), Some(&1));
}

// ---- strategy parsing --------------------------------------------------

#[test]
fn strategy_str_parses_all_aliases() {
    assert_eq!(
        SelectionStrategy::from_str_lossy("rr"),
        SelectionStrategy::RoundRobin
    );
    assert_eq!(
        SelectionStrategy::from_str_lossy("ROUND_ROBIN"),
        SelectionStrategy::RoundRobin
    );
    assert_eq!(
        SelectionStrategy::from_str_lossy("least-errors"),
        SelectionStrategy::LeastErrors
    );
    assert_eq!(
        SelectionStrategy::from_str_lossy(""),
        SelectionStrategy::Sticky
    );
    assert_eq!(
        SelectionStrategy::from_str_lossy("anything-else"),
        SelectionStrategy::Sticky
    );
}

#[test]
fn strategy_as_str_round_trips() {
    for s in [
        SelectionStrategy::Sticky,
        SelectionStrategy::RoundRobin,
        SelectionStrategy::LeastErrors,
    ] {
        assert_eq!(SelectionStrategy::from_str_lossy(s.as_str()), s);
    }
}

// ---- key source display ------------------------------------------------

#[test]
fn key_source_display_round_trips() {
    assert_eq!(
        KeySource::Credential("openai_a".into()).to_string(),
        "credential:openai_a"
    );
    assert_eq!(
        KeySource::Env("OPENAI_API_KEY".into()).to_string(),
        "env:OPENAI_API_KEY"
    );
    assert_eq!(KeySource::Inline.to_string(), "inline");
}

// ---- lease ------------------------------------------------------------

#[test]
fn lease_snapshots_value_at_acquire() {
    // Even after a parallel cooldown bumps the cursor, the
    // existing lease's value remains stable.
    let pool = pool(&[("A", "kA")], SelectionStrategy::Sticky);
    let l = pool.acquire().unwrap();
    assert_eq!(l.value(), "kA");
    assert_eq!(l.policy(), LeasePolicy::SnapshotValue);
}

#[test]
fn lease_index_reflects_pool_position() {
    let pool = pool(&[("A", "kA"), ("B", "kB")], SelectionStrategy::RoundRobin);
    let l1 = pool.acquire().unwrap();
    assert_eq!(l1.index(), 0);
    pool.report_success(&l1);
    let l2 = pool.acquire().unwrap();
    assert_eq!(l2.index(), 1);
}
