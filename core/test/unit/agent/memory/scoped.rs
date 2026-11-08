use super::*;

#[test]
fn workspace_selection_precedes_fts_limit_source_windows_and_counts() {
    let db = MemoryDb::open_in_memory().unwrap();
    let private = db
        .record_message(
            "private",
            "user",
            "needle needle private Workspace transcript",
        )
        .unwrap();
    for _ in 0..20 {
        db.record_message("private", "assistant", "needle private detail")
            .unwrap();
    }
    let visible = db
        .record_message("allowed", "user", "needle shared context")
        .unwrap();
    let scoped = db
        .clone()
        .with_session_scope(&["allowed".into()], false)
        .unwrap();
    let hits = scoped.search_history("needle", None, 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].row.id, visible);
    assert!(scoped.replayable_message(private).unwrap().is_none());
    assert_eq!(scoped.count_session("private").unwrap(), 0);
    assert_eq!(scoped.count_total().unwrap(), 1);
    assert_eq!(scoped.sessions(1).unwrap()[0].session_id, "allowed");
    assert_eq!(
        db.count_total().unwrap(),
        22,
        "owner inspection preserves canonical evidence"
    );
}

#[test]
fn legacy_unassigned_reads_exclude_new_private_workspace_history() {
    let db = MemoryDb::open_in_memory().unwrap();
    let private = db
        .record_message("private", "user", "private needle")
        .unwrap();
    let legacy = db
        .record_message("legacy", "user", "legacy needle")
        .unwrap();
    let scoped = db.with_session_scope(&["private".into()], true).unwrap();
    let hits = scoped.search_history("needle", None, 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].row.id, legacy);
    assert!(scoped.message(private).unwrap().is_none());
    assert_eq!(scoped.sessions(10).unwrap().len(), 1);
}
