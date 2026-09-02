use super::*;

#[test]
fn recall_empty_query_errors() {
    let err = recall_cmd(&[]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"));
}

// ---- sessions_cmd / sessions_*_with ----

fn fresh_session_db() -> memory::sqlite_fts::MemoryDb {
    memory::sqlite_fts::MemoryDb::open_in_memory().expect("open in-memory db")
}

#[test]
fn sessions_list_with_empty_db_returns_no_sessions() {
    let db = fresh_session_db();
    let v = sessions_list_with(&db, 20).expect("list ok");
    assert_eq!(v.get("n").and_then(|n| n.as_u64()), Some(0));
    assert_eq!(v.get("limit").and_then(|n| n.as_u64()), Some(20));
    assert!(v
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(false));
}

#[test]
fn sessions_list_with_returns_recorded_sessions_in_recency_order() {
    let db = fresh_session_db();
    db.record_message("s-old", "user", "hi old").unwrap();
    // Tick to ensure a different ms.
    std::thread::sleep(std::time::Duration::from_millis(5));
    db.record_message("s-new", "user", "hi new").unwrap();

    let v = sessions_list_with(&db, 10).expect("list ok");
    let arr = v.get("sessions").and_then(|s| s.as_array()).expect("array");
    assert_eq!(arr.len(), 2);
    // Most recent first.
    assert_eq!(
        arr[0].get("session_id").and_then(|s| s.as_str()),
        Some("s-new")
    );
    assert_eq!(
        arr[1].get("session_id").and_then(|s| s.as_str()),
        Some("s-old")
    );
}

#[test]
fn sessions_title_with_returns_null_when_unset() {
    let db = fresh_session_db();
    let v = sessions_title_with(&db, "sx").expect("title ok");
    assert_eq!(v.get("set").and_then(|b| b.as_bool()), Some(false));
    assert!(v.get("title").map(|t| t.is_null()).unwrap_or(false));
}

#[test]
fn sessions_set_title_with_then_title_with_round_trips() {
    let db = fresh_session_db();
    let v = sessions_set_title_with(&db, "sx", "My Session").expect("set ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("My Session"));
    let v2 = sessions_title_with(&db, "sx").expect("title ok");
    assert_eq!(v2.get("title").and_then(|s| s.as_str()), Some("My Session"));
    assert_eq!(v2.get("set").and_then(|b| b.as_bool()), Some(true));
}

#[test]
fn sessions_set_title_overwrites_existing_title() {
    let db = fresh_session_db();
    sessions_set_title_with(&db, "sx", "first").expect("set ok");
    sessions_set_title_with(&db, "sx", "second").expect("set ok");
    let v = sessions_title_with(&db, "sx").expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("second"));
}

#[test]
fn parse_set_title_args_accepts_multi_word_title() {
    let (id, title) = parse_set_title_args(&[
        "sid".into(),
        "Hello".into(),
        "World".into(),
        "Of".into(),
        "Tests".into(),
    ])
    .expect("parse ok");
    assert_eq!(id, "sid");
    assert_eq!(title, "Hello World Of Tests");
}

#[test]
fn parse_set_title_args_stops_at_first_flag() {
    let (id, title) = parse_set_title_args(&[
        "sid".into(),
        "Hello".into(),
        "World".into(),
        "--unknown".into(),
        "ignored".into(),
    ])
    .expect("parse ok");
    assert_eq!(id, "sid");
    assert_eq!(title, "Hello World");
}

#[test]
fn parse_set_title_args_requires_title() {
    let err = parse_set_title_args(&["sid".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn parse_set_title_args_rejects_id_starting_with_double_dash() {
    let err = parse_set_title_args(&["--id".into(), "title".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn sessions_count_with_total_includes_all_sessions() {
    let db = fresh_session_db();
    db.record_message("s1", "user", "a").unwrap();
    db.record_message("s1", "assistant", "b").unwrap();
    db.record_message("s2", "user", "c").unwrap();
    let v = sessions_count_with(&db, None).expect("count ok");
    assert_eq!(v.get("total_messages").and_then(|n| n.as_i64()), Some(3));
}

#[test]
fn sessions_count_with_filters_by_session_id() {
    let db = fresh_session_db();
    db.record_message("s1", "user", "a").unwrap();
    db.record_message("s1", "assistant", "b").unwrap();
    db.record_message("s2", "user", "c").unwrap();
    let v = sessions_count_with(&db, Some("s1")).expect("count ok");
    assert_eq!(v.get("messages").and_then(|n| n.as_i64()), Some(2));
    assert_eq!(v.get("session_id").and_then(|s| s.as_str()), Some("s1"));
}

#[test]
fn sessions_clear_with_drops_session_messages_only() {
    let db = fresh_session_db();
    db.record_message("s1", "user", "a").unwrap();
    db.record_message("s1", "assistant", "b").unwrap();
    db.record_message("s2", "user", "c").unwrap();
    let v = sessions_clear_with(&db, "s1").expect("clear ok");
    assert_eq!(v.get("messages_cleared").and_then(|n| n.as_u64()), Some(2));
    // s2 should be intact.
    let total = sessions_count_with(&db, None).expect("count ok");
    assert_eq!(
        total.get("total_messages").and_then(|n| n.as_i64()),
        Some(1)
    );
}

#[test]
fn sessions_clear_refuses_without_yes_flag() {
    let err = sessions_clear(&["sx".into()]).unwrap_err();
    assert!(err.contains("--yes"));
}

#[test]
fn sessions_clear_requires_session_id() {
    let err = sessions_clear(&[]).unwrap_err();
    assert!(err.contains("usage"));
    let err2 = sessions_clear(&["--yes".into()]).unwrap_err();
    assert!(err2.contains("usage"));
}

#[test]
fn sessions_cmd_numeric_first_arg_routes_to_list() {
    // Numeric first arg keeps backward-compat: cos agent sessions 5 → list 5.
    let v = sessions_cmd(&["5".into()]).expect("legacy list ok");
    assert_eq!(v.get("limit").and_then(|n| n.as_u64()), Some(5));
}

// ---- sessions_purge ----

#[test]
fn sessions_purge_requires_older_than() {
    let err = sessions_purge(&["--yes".into()]).unwrap_err();
    assert!(err.contains("--older-than"), "got {err}");
}

#[test]
fn sessions_purge_validates_days_is_positive_integer() {
    let err = sessions_purge(&["--older-than".into(), "0".into(), "--yes".into()]).unwrap_err();
    assert!(err.contains("> 0"), "got {err}");
    let err2 = sessions_purge(&["--older-than".into(), "abc".into(), "--yes".into()]).unwrap_err();
    assert!(err2.contains("positive integer"), "got {err2}");
}

#[test]
fn sessions_purge_refuses_apply_without_yes() {
    let err = sessions_purge(&["--older-than".into(), "1".into()]).unwrap_err();
    assert!(err.contains("--yes"), "got {err}");
    assert!(err.contains("--dry-run"), "got {err}");
}

#[test]
fn sessions_purge_with_dry_run_does_not_mutate() {
    let db = fresh_session_db();
    // Insert one ancient message with explicit ts so we can
    // exercise the cutoff cleanly.
    db.record_message_at("old", "user", "ancient", 100).unwrap();
    // And one fresh row via the normal path so its ts_ms is now.
    db.record_message("new", "user", "fresh").unwrap();
    // Cutoff = 1000ms; "old" (100) is below, "new" (~now) is above.
    let v = sessions_purge_with(&db, 1000, 7, true).expect("dry ok");
    assert_eq!(v["dry_run"], json!(true));
    assert_eq!(v["messages_deleted"], json!(1));
    assert_eq!(v["sessions_emptied"], json!(1));
    // Messages still on disk after dry-run.
    let total = sessions_count_with(&db, None).unwrap();
    assert_eq!(total["total_messages"].as_i64(), Some(2));
}

#[test]
fn sessions_purge_with_apply_drops_old_rows_and_titles() {
    let db = fresh_session_db();
    db.record_message_at("old", "user", "ancient", 100).unwrap();
    db.set_title("old", "Old Convo").unwrap();
    db.record_message("new", "user", "fresh").unwrap();
    // Apply with cutoff=1000.
    let v = sessions_purge_with(&db, 1000, 7, false).expect("apply ok");
    assert_eq!(v["dry_run"], json!(false));
    assert_eq!(v["messages_deleted"], json!(1));
    assert_eq!(v["sessions_emptied"], json!(1));
    assert_eq!(v["titles_deleted"], json!(1));
    // Only "new" remains.
    let total = sessions_count_with(&db, None).unwrap();
    assert_eq!(total["total_messages"].as_i64(), Some(1));
    // Title for "old" is gone.
    let title = db.title_for("old").unwrap();
    assert!(title.is_none());
}

#[test]
fn sessions_purge_empty_db_returns_zero_counts() {
    let db = fresh_session_db();
    let v = sessions_purge_with(&db, 1000, 7, false).expect("apply ok");
    assert_eq!(v["messages_deleted"], json!(0));
    assert_eq!(v["sessions_emptied"], json!(0));
    assert_eq!(v["titles_deleted"], json!(0));
}

#[test]
fn sessions_purge_dispatched_via_sessions_cmd() {
    // Smoke test that the `purge` verb is wired through
    // sessions_cmd. We pass --dry-run --older-than 999999 to
    // ensure no rows match (so the test doesn't depend on the
    // shared default db being empty).
    let v = sessions_cmd(&[
        "purge".into(),
        "--older-than".into(),
        "999999".into(),
        "--dry-run".into(),
    ])
    .expect("dispatch ok");
    assert_eq!(v["dry_run"], json!(true));
    assert_eq!(v["older_than_days"], json!(999999u64));
}

// ---- sessions_stats ----

#[test]
fn sessions_stats_rejects_extra_args() {
    let err = sessions_stats(&["bogus".into()]).unwrap_err();
    assert!(err.contains("unexpected argument"), "got {err}");
}

#[test]
fn sessions_stats_session_flag_rejects_empty_value() {
    let err = sessions_stats(&["--session".into(), "".into()]).unwrap_err();
    assert!(err.contains("must not be empty"), "got {err}");
}

#[test]
fn sessions_stats_session_with_unknown_id_returns_zeros() {
    let db = fresh_session_db();
    // Other sessions exist, but the requested one does not.
    db.record_message("other", "user", "x").unwrap();
    let v = sessions_stats_session_with(&db, "ghost", 1_000_000).expect("stats ok");
    assert_eq!(v["scope"], json!("session"));
    assert_eq!(v["session_id"], json!("ghost"));
    assert_eq!(v["title"], json!(null));
    assert_eq!(v["total_messages"], json!(0u64));
    assert_eq!(v["by_role"], json!([]));
    // No total_sessions / titled_sessions in per-session shape.
    assert!(v.get("total_sessions").is_none());
    assert!(v.get("titled_sessions").is_none());
}

#[test]
fn sessions_stats_session_with_isolates_one_session() {
    let db = fresh_session_db();
    let now: i64 = 100 * 86_400_000;
    for _ in 0..3 {
        db.record_message_at("alpha", "user", "a", now - 3_600_000)
            .unwrap();
    }
    for _ in 0..7 {
        db.record_message_at("beta", "user", "b", now).unwrap();
    }
    db.set_title("alpha", "Alpha").unwrap();
    let v = sessions_stats_session_with(&db, "alpha", now).expect("stats ok");
    assert_eq!(v["session_id"], json!("alpha"));
    assert_eq!(v["title"], json!("Alpha"));
    assert_eq!(v["total_messages"], json!(3u64));
    assert_eq!(v["messages_last_1d"], json!(3u64));
    assert_eq!(v["by_role"], json!([{"role": "user", "count": 3u64}]));
}

#[test]
fn sessions_stats_dispatched_with_session_flag() {
    let v = sessions_cmd(&["stats".into(), "--session".into(), "no-such-id".into()])
        .expect("dispatch ok");
    assert_eq!(v["scope"], json!("session"));
    assert_eq!(v["session_id"], json!("no-such-id"));
}

#[test]
fn sessions_stats_with_empty_db_is_all_zeros() {
    let db = fresh_session_db();
    let v = sessions_stats_with(&db, 1_000_000).expect("stats ok");
    assert_eq!(v["ok"], json!(true));
    assert_eq!(v["total_messages"], json!(0u64));
    assert_eq!(v["total_sessions"], json!(0u64));
    assert_eq!(v["titled_sessions"], json!(0u64));
    assert_eq!(v["messages_last_7d"], json!(0u64));
    assert_eq!(v["by_role"], json!([]));
    assert_eq!(v["oldest_ts_ms"], json!(null));
    assert_eq!(v["newest_ts_ms"], json!(null));
}

#[test]
fn sessions_stats_with_buckets_recency_and_role() {
    let db = fresh_session_db();
    let now: i64 = 100 * 86_400_000;
    db.record_message_at("s", "user", "fresh", now - 3_600_000)
        .unwrap();
    db.record_message_at("s", "assistant", "old", now - 10 * 86_400_000)
        .unwrap();
    db.record_message_at("t", "user", "ancient", now - 60 * 86_400_000)
        .unwrap();
    db.set_title("s", "Hello").unwrap();
    let v = sessions_stats_with(&db, now).expect("stats ok");
    assert_eq!(v["total_messages"], json!(3u64));
    assert_eq!(v["total_sessions"], json!(2u64));
    assert_eq!(v["titled_sessions"], json!(1u64));
    assert_eq!(v["messages_last_1d"], json!(1u64));
    assert_eq!(v["messages_last_7d"], json!(1u64));
    assert_eq!(v["messages_last_30d"], json!(2u64));
    // by_role: "user" leads with 2, "assistant" trails with 1.
    let roles = v["by_role"].as_array().expect("array");
    assert_eq!(roles.len(), 2);
    assert_eq!(roles[0]["role"], json!("user"));
    assert_eq!(roles[0]["count"], json!(2u64));
    assert_eq!(v["oldest_ts_ms"], json!(now - 60 * 86_400_000));
    assert_eq!(v["newest_ts_ms"], json!(now - 3_600_000));
}

#[test]
fn sessions_stats_dispatched_via_sessions_cmd() {
    let v = sessions_cmd(&["stats".into()]).expect("dispatch ok");
    assert!(v.get("total_messages").is_some());
    assert!(v.get("by_role").is_some());
}

// ---- sessions_top ----

#[test]
fn sessions_top_with_empty_db_returns_empty_array() {
    let db = fresh_session_db();
    let v = sessions_top_with(&db, 10).expect("top ok");
    assert_eq!(v["ok"], json!(true));
    assert_eq!(v["limit"], json!(10u64));
    assert_eq!(v["n"], json!(0u64));
    assert_eq!(v["ordered_by"], json!("message_count_desc"));
    assert_eq!(v["sessions"], json!([]));
}

#[test]
fn sessions_top_with_orders_by_count_desc() {
    let db = fresh_session_db();
    // "fat" 3 msgs, "mid" 2, "thin" 1.
    for _ in 0..3 {
        db.record_message("fat", "user", "x").unwrap();
    }
    for _ in 0..2 {
        db.record_message("mid", "user", "x").unwrap();
    }
    db.record_message("thin", "user", "x").unwrap();
    let v = sessions_top_with(&db, 10).expect("top ok");
    assert_eq!(v["n"], json!(3u64));
    let arr = v["sessions"].as_array().unwrap();
    assert_eq!(arr[0]["session_id"], json!("fat"));
    assert_eq!(arr[0]["message_count"], json!(3));
    assert_eq!(arr[1]["session_id"], json!("mid"));
    assert_eq!(arr[2]["session_id"], json!("thin"));
}

#[test]
fn sessions_top_with_carries_titles() {
    let db = fresh_session_db();
    db.record_message("s", "user", "x").unwrap();
    db.set_title("s", "Greeting").unwrap();
    let v = sessions_top_with(&db, 10).expect("top ok");
    let arr = v["sessions"].as_array().unwrap();
    assert_eq!(arr[0]["title"], json!("Greeting"));
}

#[test]
fn sessions_top_default_limit_is_20() {
    // Just make sure no parse errors; with no rows the array is
    // empty but the limit echoes 20.
    let v = sessions_top(&[]).expect("dispatch ok");
    assert_eq!(v["limit"], json!(20u64));
}

#[test]
fn sessions_top_dispatched_via_sessions_cmd() {
    let v = sessions_cmd(&["top".into(), "5".into()]).expect("dispatch ok");
    assert_eq!(v["limit"], json!(5u64));
    assert_eq!(v["ordered_by"], json!("message_count_desc"));
}
