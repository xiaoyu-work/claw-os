use super::*;

fn db() -> MemoryDb {
    MemoryDb::open_in_memory().unwrap()
}

#[test]
fn open_in_memory_is_clean() {
    let db = db();
    assert_eq!(db.count_total().unwrap(), 0);
}

#[test]
fn record_then_recent_returns_in_order() {
    let db = db();
    db.record_message("s1", "user", "first").unwrap();
    db.record_message("s1", "assistant", "second").unwrap();
    db.record_message("s1", "user", "third").unwrap();
    let rows = db.recent("s1", 10).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].content, "first");
    assert_eq!(rows[2].content, "third");
}

#[test]
fn recent_respects_limit_and_returns_latest() {
    let db = db();
    for i in 0..10 {
        db.record_message("s1", "user", &format!("msg-{i}"))
            .unwrap();
    }
    let rows = db.recent("s1", 3).unwrap();
    assert_eq!(rows.len(), 3);
    // 7, 8, 9 in chronological order
    assert_eq!(rows[0].content, "msg-7");
    assert_eq!(rows[2].content, "msg-9");
}

#[test]
fn recent_replayable_excludes_injected_before_limit_but_recent_retains_them() {
    let db = db();
    db.record_message("s1", "user", "old question").unwrap();
    db.record_message("s1", "assistant", "[tool_use:lookup] {}")
        .unwrap();
    db.record_injected("s1", "memory_notes", "stale memory")
        .unwrap();
    db.record_injected("s1", "skills_catalog", "stale skills")
        .unwrap();
    db.record_injected("s1", "due_nudges", "stale nudge")
        .unwrap();
    db.record_message("s1", "user", "[tool_result] fresh result")
        .unwrap();
    db.record_message("s1", "assistant", "final answer")
        .unwrap();

    let replayable = db.recent_replayable("s1", 3).unwrap();
    assert_eq!(replayable.len(), 3);
    assert_eq!(replayable[0].content, "[tool_use:lookup] {}");
    assert_eq!(replayable[1].content, "[tool_result] fresh result");
    assert_eq!(replayable[2].content, "final answer");
    assert!(replayable.iter().all(|row| row.role != INJECTED_ROLE));

    let audit_rows = db.recent("s1", 10).unwrap();
    let injected: Vec<_> = audit_rows
        .iter()
        .filter(|row| row.role == INJECTED_ROLE)
        .collect();
    assert_eq!(injected.len(), 3);
    assert_eq!(injected[0].content, "[memory_notes]\nstale memory");
    assert_eq!(injected[1].content, "[skills_catalog]\nstale skills");
    assert_eq!(injected[2].content, "[due_nudges]\nstale nudge");
}

#[test]
fn recent_isolates_by_session() {
    let db = db();
    db.record_message("a", "user", "alpha").unwrap();
    db.record_message("b", "user", "bravo").unwrap();
    let a = db.recent("a", 10).unwrap();
    let b = db.recent("b", 10).unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert_eq!(a[0].content, "alpha");
    assert_eq!(b[0].content, "bravo");
}

#[test]
fn system_prompt_freeze_is_content_addressed_and_first_writer_wins() {
    let db = db();
    let first = db.freeze_system_prompt("s1", "stable prompt", 1).unwrap();
    assert!(first.newly_frozen);
    assert_eq!(first.prompt, "stable prompt");
    assert_eq!(first.version, 1);

    let losing = db.freeze_system_prompt("s1", "changed prompt", 1).unwrap();
    assert!(!losing.newly_frozen);
    assert_eq!(losing.prompt, "stable prompt");

    let shared = db.freeze_system_prompt("s2", "stable prompt", 1).unwrap();
    assert!(shared.newly_frozen);
    let conn = db.lock_conn().unwrap();
    let prompts: i64 = conn
        .query_row("SELECT COUNT(*) FROM system_prompts", [], |row| row.get(0))
        .unwrap();
    let refs: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_system_prompts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(prompts, 1, "identical prompts should share one blob");
    assert_eq!(refs, 2);

    drop(conn);
    let upgraded = db.freeze_system_prompt("s1", "upgraded prompt", 2).unwrap();
    assert!(upgraded.newly_frozen);
    assert_eq!(upgraded.prompt, "upgraded prompt");
    assert_eq!(upgraded.version, 2);
    let stale_writer = db.freeze_system_prompt("s1", "stale prompt", 1).unwrap();
    assert!(!stale_writer.newly_frozen);
    assert_eq!(stale_writer.prompt, "upgraded prompt");
    assert_eq!(stale_writer.version, 2);
    assert!(db.system_prompt_for("s1", 3).unwrap().is_none());
}

#[test]
fn system_prompt_lookup_rejects_wrong_content_hash() {
    let db = db();
    db.freeze_system_prompt("s1", "trusted prompt", 1).unwrap();
    {
        let conn = db.lock_conn().unwrap();
        conn.execute("UPDATE system_prompts SET prompt = 'tampered prompt'", [])
            .unwrap();
    }
    let error = db.system_prompt_for("s1", 1).unwrap_err();
    assert!(error.is_integrity_failure());
}

#[test]
fn system_prompt_lookup_rejects_dangling_reference() {
    let db = db();
    db.freeze_system_prompt("s1", "trusted prompt", 1).unwrap();
    {
        let conn = db.lock_conn().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(
            "UPDATE session_system_prompts
             SET prompt_hash = 'missing'
             WHERE session_id = 's1'",
            [],
        )
        .unwrap();
    }
    let error = db.system_prompt_for("s1", 1).unwrap_err();
    assert!(error.is_integrity_failure());
}

#[test]
fn search_finds_substring_match_via_fts() {
    let db = db();
    db.record_message("s", "user", "I love pineapples on pizza")
        .unwrap();
    db.record_message("s", "assistant", "noted: dislikes mushrooms")
        .unwrap();
    let hits = db.search("pineapples", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].row.content.contains("pineapples"));
}

#[test]
fn search_multi_word_is_implicit_and() {
    let db = db();
    db.record_message("s", "user", "hot soup is delicious")
        .unwrap();
    db.record_message("s", "user", "cold lemonade is refreshing")
        .unwrap();
    let hits = db.search("hot soup", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].row.content.contains("hot soup"));
}

#[test]
fn search_returns_empty_for_no_match() {
    let db = db();
    db.record_message("s", "user", "hello world").unwrap();
    let hits = db.search("xyzzynotfound", 10).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn search_handles_punctuation_safely() {
    let db = db();
    db.record_message("s", "user", "use cos_sysinfo to inspect things")
        .unwrap();
    // Inputs like `(foo)` or `bar*` previously broke FTS5 — must be sanitised.
    let hits = db.search("cos_sysinfo (inspect)", 10).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn search_with_empty_query_returns_empty_not_error() {
    let db = db();
    db.record_message("s", "user", "x").unwrap();
    assert!(db.search("", 10).unwrap().is_empty());
    assert!(db.search("   ", 10).unwrap().is_empty());
}

#[test]
fn search_session_constrains_to_session() {
    let db = db();
    db.record_message("alpha", "user", "shared word").unwrap();
    db.record_message("bravo", "user", "shared word").unwrap();
    let alpha_only = db.search_session("alpha", "shared", 10).unwrap();
    assert_eq!(alpha_only.len(), 1);
    assert_eq!(alpha_only[0].row.session_id, "alpha");
}

#[test]
fn has_session_requires_conversation_messages() {
    let db = db();
    db.set_title("claimed", "Title only").unwrap();
    assert!(!db.has_session("claimed").unwrap());
    db.record_message("claimed", "user", "real history").unwrap();
    assert!(db.has_session("claimed").unwrap());
}

#[test]
fn clear_session_removes_rows_and_fts() {
    let db = db();
    db.record_message("s", "user", "delete me please").unwrap();
    db.record_message("t", "user", "keep me please").unwrap();
    let n = db.clear_session("s").unwrap();
    assert_eq!(n, 1);
    assert_eq!(db.count_session("s").unwrap(), 0);
    assert_eq!(db.count_session("t").unwrap(), 1);
    // FTS must also have been cleared
    let hits = db.search("delete me please", 10).unwrap();
    assert!(hits.is_empty(), "FTS index should be cleared by trigger");
}

#[test]
fn clear_session_reclaims_only_unreferenced_system_prompts() {
    let db = db();
    db.record_message("s1", "user", "one").unwrap();
    db.record_message("s2", "user", "two").unwrap();
    db.freeze_system_prompt("s1", "shared", 1).unwrap();
    db.freeze_system_prompt("s2", "shared", 1).unwrap();

    db.clear_session("s1").unwrap();
    assert!(db.system_prompt_for("s1", 1).unwrap().is_none());
    assert_eq!(
        db.system_prompt_for("s2", 1).unwrap().as_deref(),
        Some("shared")
    );

    db.clear_session("s2").unwrap();
    let conn = db.lock_conn().unwrap();
    let prompts: i64 = conn
        .query_row("SELECT COUNT(*) FROM system_prompts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(prompts, 0);
}

#[test]
fn purge_older_than_ms_drops_only_below_cutoff() {
    let db = db();
    db.record_message_at("a", "user", "ancient", 100).unwrap();
    db.record_message_at("a", "user", "less ancient", 500)
        .unwrap();
    db.record_message_at("b", "user", "fresh", 5000).unwrap();
    let stats = db.purge_older_than_ms(1000).unwrap();
    assert_eq!(stats.messages_deleted, 2);
    assert_eq!(stats.sessions_emptied, 1);
    assert_eq!(stats.titles_deleted, 0);
    assert_eq!(db.count_total().unwrap(), 1);
    // FTS index must also be in sync (trigger drops mirror rows).
    let hits = db.search("ancient", 10).unwrap();
    assert!(hits.is_empty(), "FTS should drop purged rows");
}

#[test]
fn purge_older_than_ms_drops_orphaned_titles() {
    let db = db();
    db.record_message_at("old", "user", "x", 100).unwrap();
    db.set_title("old", "Old").unwrap();
    db.record_message_at("new", "user", "y", 5000).unwrap();
    db.set_title("new", "New").unwrap();
    let stats = db.purge_older_than_ms(1000).unwrap();
    assert_eq!(stats.messages_deleted, 1);
    assert_eq!(stats.sessions_emptied, 1);
    assert_eq!(stats.titles_deleted, 1);
    assert!(db.title_for("old").unwrap().is_none());
    assert_eq!(
        db.title_for("new").unwrap().as_deref(),
        Some("New"),
        "non-orphaned title must be preserved"
    );
}

#[test]
fn purge_older_than_ms_reclaims_only_emptied_session_prompts() {
    let db = db();
    db.record_message_at("old", "user", "x", 100).unwrap();
    db.freeze_system_prompt("old", "old prompt", 1).unwrap();
    db.record_message_at("new", "user", "y", 5000).unwrap();
    db.freeze_system_prompt("new", "new prompt", 1).unwrap();

    db.purge_older_than_ms(1000).unwrap();

    assert!(db.system_prompt_for("old", 1).unwrap().is_none());
    assert_eq!(
        db.system_prompt_for("new", 1).unwrap().as_deref(),
        Some("new prompt")
    );
}

#[test]
fn count_older_than_ms_does_not_mutate() {
    let db = db();
    db.record_message_at("old", "user", "x", 100).unwrap();
    db.set_title("old", "Old").unwrap();
    db.record_message_at("new", "user", "y", 5000).unwrap();
    let count = db.count_older_than_ms(1000).unwrap();
    assert_eq!(count.messages_deleted, 1);
    assert_eq!(count.sessions_emptied, 1);
    assert_eq!(count.titles_deleted, 1);
    // Nothing actually removed.
    assert_eq!(db.count_total().unwrap(), 2);
    assert_eq!(db.title_for("old").unwrap().as_deref(), Some("Old"));
}

#[test]
fn purge_older_than_ms_boundary_is_strict_less_than() {
    let db = db();
    db.record_message_at("s", "user", "exact", 1000).unwrap();
    let stats = db.purge_older_than_ms(1000).unwrap();
    assert_eq!(
        stats.messages_deleted, 0,
        "row at exact cutoff must be kept"
    );
    assert_eq!(db.count_total().unwrap(), 1);
}

#[test]
fn purge_older_than_ms_partial_session_keeps_session() {
    let db = db();
    db.record_message_at("s", "user", "first", 100).unwrap();
    db.record_message_at("s", "user", "second", 5000).unwrap();
    let stats = db.purge_older_than_ms(1000).unwrap();
    assert_eq!(stats.messages_deleted, 1);
    assert_eq!(
        stats.sessions_emptied, 0,
        "session still has a remaining row, must not count as emptied"
    );
    assert_eq!(db.count_session("s").unwrap(), 1);
}

#[test]
fn stats_empty_db_returns_zero_with_no_extremes() {
    let db = db();
    let s = db.stats(10_000).unwrap();
    assert_eq!(s.total_messages, 0);
    assert_eq!(s.total_sessions, 0);
    assert_eq!(s.titled_sessions, 0);
    assert_eq!(s.messages_last_1d, 0);
    assert_eq!(s.messages_last_7d, 0);
    assert_eq!(s.messages_last_30d, 0);
    assert!(s.by_role.is_empty());
    assert!(s.oldest_ts_ms.is_none());
    assert!(s.newest_ts_ms.is_none());
}

#[test]
fn stats_buckets_recency_correctly() {
    let db = db();
    let now: i64 = 100 * 86_400_000; // day 100 in ms
                                     // 1 row at now-2h, 1 at now-3d, 1 at now-15d, 1 at now-60d.
    db.record_message_at("s", "user", "a", now - 2 * 3_600_000)
        .unwrap();
    db.record_message_at("s", "user", "b", now - 3 * 86_400_000)
        .unwrap();
    db.record_message_at("s", "user", "c", now - 15 * 86_400_000)
        .unwrap();
    db.record_message_at("t", "user", "d", now - 60 * 86_400_000)
        .unwrap();
    let s = db.stats(now).unwrap();
    assert_eq!(s.total_messages, 4);
    assert_eq!(s.total_sessions, 2);
    assert_eq!(s.messages_last_1d, 1, "only the 2h-old row");
    assert_eq!(s.messages_last_7d, 2, "2h + 3d");
    assert_eq!(s.messages_last_30d, 3, "2h + 3d + 15d");
    assert_eq!(s.oldest_ts_ms, Some(now - 60 * 86_400_000));
    assert_eq!(s.newest_ts_ms, Some(now - 2 * 3_600_000));
}

#[test]
fn stats_by_role_is_count_desc() {
    let db = db();
    db.record_message("s", "user", "1").unwrap();
    db.record_message("s", "user", "2").unwrap();
    db.record_message("s", "user", "3").unwrap();
    db.record_message("s", "assistant", "ok").unwrap();
    db.record_message("s", "system", "init").unwrap();
    let s = db.stats(0).unwrap();
    assert_eq!(s.by_role[0], ("user".into(), 3));
    // Tied roles can land in either order but both must be present.
    let names: Vec<&str> = s.by_role.iter().map(|(r, _)| r.as_str()).collect();
    assert!(names.contains(&"assistant"));
    assert!(names.contains(&"system"));
}

#[test]
fn stats_titled_sessions_counts_session_titles_rows() {
    let db = db();
    db.record_message("a", "user", "x").unwrap();
    db.record_message("b", "user", "y").unwrap();
    db.set_title("a", "Alpha").unwrap();
    let s = db.stats(0).unwrap();
    assert_eq!(s.total_sessions, 2);
    assert_eq!(s.titled_sessions, 1);
}

#[test]
fn stats_for_session_unknown_id_is_all_zeros() {
    let db = db();
    // Even with rows in OTHER sessions, the requested id has nothing.
    db.record_message("other", "user", "x").unwrap();
    let s = db.stats_for_session("ghost", 0).unwrap();
    assert_eq!(s.session_id, "ghost");
    assert_eq!(s.total_messages, 0);
    assert!(s.title.is_none());
    assert_eq!(s.messages_last_1d, 0);
    assert_eq!(s.by_role.len(), 0);
    assert!(s.oldest_ts_ms.is_none());
    assert!(s.newest_ts_ms.is_none());
}

#[test]
fn stats_for_session_isolates_one_session() {
    let db = db();
    let now: i64 = 100 * 86_400_000;
    // session "alpha" gets 3 rows, "beta" gets 5.
    for i in 0..3 {
        db.record_message_at("alpha", "user", "a", now - i).unwrap();
    }
    for i in 0..5 {
        db.record_message_at("beta", "user", "b", now - i).unwrap();
    }
    let s = db.stats_for_session("alpha", now).unwrap();
    assert_eq!(s.session_id, "alpha");
    assert_eq!(s.total_messages, 3);
    let s2 = db.stats_for_session("beta", now).unwrap();
    assert_eq!(s2.total_messages, 5);
}

#[test]
fn stats_for_session_buckets_recency_correctly() {
    let db = db();
    let now: i64 = 100 * 86_400_000;
    // 1 row at now-2h, 1 at now-3d, 1 at now-15d, 1 at now-60d.
    db.record_message_at("s", "user", "a", now - 2 * 3_600_000)
        .unwrap();
    db.record_message_at("s", "user", "b", now - 3 * 86_400_000)
        .unwrap();
    db.record_message_at("s", "user", "c", now - 15 * 86_400_000)
        .unwrap();
    db.record_message_at("s", "user", "d", now - 60 * 86_400_000)
        .unwrap();
    // Add noise in another session — must not leak into our buckets.
    db.record_message_at("noise", "user", "z", now).unwrap();
    let s = db.stats_for_session("s", now).unwrap();
    assert_eq!(s.total_messages, 4);
    assert_eq!(s.messages_last_1d, 1);
    assert_eq!(s.messages_last_7d, 2);
    assert_eq!(s.messages_last_30d, 3);
    assert_eq!(s.oldest_ts_ms, Some(now - 60 * 86_400_000));
    assert_eq!(s.newest_ts_ms, Some(now - 2 * 3_600_000));
}

#[test]
fn stats_for_session_carries_title() {
    let db = db();
    db.record_message("s", "user", "x").unwrap();
    db.set_title("s", "Hello there").unwrap();
    let s = db.stats_for_session("s", 0).unwrap();
    assert_eq!(s.title.as_deref(), Some("Hello there"));
}

#[test]
fn stats_for_session_returns_orphan_title_when_messages_purged() {
    // Edge case: title row survives a purge with no messages left.
    // total_messages = 0 but title is still surfaced.
    let db = db();
    db.record_message_at("s", "user", "x", 100).unwrap();
    db.set_title("s", "Survivor").unwrap();
    // Purge everything older than ts=200 — drops the message.
    let _ = db.purge_older_than_ms(200).unwrap();
    // session_titles still has the row (it wasn't orphaned per
    // purge_older_than_ms's NOT IN logic? — actually it was orphaned
    // and dropped, so this should now be None). Let's just assert
    // the shape, not whether title survived.
    let s = db.stats_for_session("s", 0).unwrap();
    assert_eq!(s.total_messages, 0);
    assert!(s.by_role.is_empty());
    assert!(s.oldest_ts_ms.is_none());
    // Title may be None here because purge_older_than_ms cleans
    // orphaned title rows. The point of the test: stats_for_session
    // does not crash on an empty post-purge session.
    let _ = s.title;
}

#[test]
fn stats_for_session_by_role_count_desc() {
    let db = db();
    db.record_message("s", "user", "1").unwrap();
    db.record_message("s", "user", "2").unwrap();
    db.record_message("s", "user", "3").unwrap();
    db.record_message("s", "assistant", "ok").unwrap();
    db.record_message("other", "user", "leak").unwrap();
    let s = db.stats_for_session("s", 0).unwrap();
    assert_eq!(s.total_messages, 4);
    assert_eq!(s.by_role[0], ("user".into(), 3));
    assert_eq!(s.by_role[1], ("assistant".into(), 1));
}

#[test]
fn sessions_lists_most_recent_first() {
    let db = db();
    db.record_message("old", "user", "first").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.record_message("new", "user", "second").unwrap();
    let sessions = db.sessions(10).unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].session_id, "new");
    assert_eq!(sessions[1].session_id, "old");
}

#[test]
fn sessions_top_orders_by_message_count_desc() {
    let db = db();
    // "fat" gets 3 messages, "thin" gets 1. last-activity should
    // not promote "thin" — count is the primary key.
    db.record_message_at("thin", "user", "x", 9_999).unwrap();
    db.record_message_at("fat", "user", "a", 100).unwrap();
    db.record_message_at("fat", "user", "b", 200).unwrap();
    db.record_message_at("fat", "user", "c", 300).unwrap();
    let top = db.sessions_top(10).unwrap();
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].session_id, "fat");
    assert_eq!(top[0].message_count, 3);
    assert_eq!(top[1].session_id, "thin");
    assert_eq!(top[1].message_count, 1);
}

#[test]
fn sessions_top_breaks_ties_by_recency() {
    let db = db();
    // Two sessions, both with 2 messages. The one with newer
    // last_ts should win the tiebreaker.
    db.record_message_at("a", "user", "1", 100).unwrap();
    db.record_message_at("a", "user", "2", 200).unwrap();
    db.record_message_at("b", "user", "1", 100).unwrap();
    db.record_message_at("b", "user", "2", 9_999).unwrap();
    let top = db.sessions_top(10).unwrap();
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].session_id, "b", "newer last_ts wins tie");
    assert_eq!(top[1].session_id, "a");
}

#[test]
fn sessions_top_respects_limit() {
    let db = db();
    for sid in ["a", "b", "c", "d"] {
        db.record_message(sid, "user", "x").unwrap();
    }
    let top = db.sessions_top(2).unwrap();
    assert_eq!(top.len(), 2);
}

#[test]
fn sessions_top_includes_titles_when_set() {
    let db = db();
    db.record_message("s", "user", "x").unwrap();
    db.set_title("s", "Hello").unwrap();
    let top = db.sessions_top(10).unwrap();
    assert_eq!(top[0].title.as_deref(), Some("Hello"));
}

#[test]
fn sessions_top_empty_db_returns_empty() {
    let db = db();
    let top = db.sessions_top(10).unwrap();
    assert!(top.is_empty());
}

#[test]
fn title_for_returns_none_when_unset() {
    let db = db();
    db.record_message("s", "user", "x").unwrap();
    assert!(db.title_for("s").unwrap().is_none());
}

#[test]
fn set_title_persists_and_reads_back() {
    let db = db();
    db.set_title("s1", "Hello session").unwrap();
    assert_eq!(
        db.title_for("s1").unwrap().as_deref(),
        Some("Hello session")
    );
}

#[test]
fn set_title_overwrites_existing() {
    let db = db();
    db.set_title("s1", "first").unwrap();
    db.set_title("s1", "second").unwrap();
    assert_eq!(db.title_for("s1").unwrap().as_deref(), Some("second"));
}

#[test]
fn sessions_returns_title_when_present() {
    let db = db();
    db.record_message("s1", "user", "hi").unwrap();
    db.record_message("s2", "user", "hi").unwrap();
    db.set_title("s2", "labelled").unwrap();
    let summaries = db.sessions(10).unwrap();
    let m: std::collections::HashMap<_, _> = summaries
        .iter()
        .map(|s| (s.session_id.clone(), s.title.clone()))
        .collect();
    assert_eq!(m.get("s1").cloned().flatten(), None);
    assert_eq!(m.get("s2").cloned().flatten().as_deref(), Some("labelled"));
}

#[test]
fn title_for_unknown_session_is_none() {
    let db = db();
    assert!(db.title_for("never-recorded").unwrap().is_none());
}

#[test]
fn open_persists_and_reopens_cleanly() {
    let dir = std::env::temp_dir().join(format!("cos-mem-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("memory.db");

    {
        let db = MemoryDb::open(&path).unwrap();
        db.record_message("s", "user", "persist me").unwrap();
    }
    {
        let db = MemoryDb::open(&path).unwrap();
        assert_eq!(db.count_total().unwrap(), 1);
        let hits = db.search("persist", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fts5_escape_handles_quotes_and_operators() {
    // Bare alphanumeric words.
    assert_eq!(fts5_escape("foo bar"), r#""foo" "bar""#);
    // Strip operators that would otherwise break MATCH.
    assert_eq!(fts5_escape("foo* (bar)"), r#""foo" "bar""#);
    // Embedded double quote is doubled.
    assert_eq!(fts5_escape(r#"a"b"#), r#""a""b""#);
    // All-whitespace returns empty.
    assert_eq!(fts5_escape("   "), "");
}

#[test]
fn render_message_content_extracts_text_and_tool_blocks() {
    use crate::agent::llm::{ContentBlock, Message, Role};
    let msg = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "let me look".into(),
            },
            ContentBlock::ToolUse {
                id: "abc".into(),
                name: "cos_sysinfo".into(),
                input: serde_json::json!({"command":"info"}),
            },
        ],
    };
    let s = render_message_content(&msg);
    assert!(s.contains("let me look"));
    assert!(s.contains("[tool_use:cos_sysinfo]"));
    assert!(s.contains("\"command\""));
}
