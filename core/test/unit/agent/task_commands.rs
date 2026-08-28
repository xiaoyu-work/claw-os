use super::*;

#[test]
fn nudge_path_returns_string() {
    let v = nudge_cmd(&["path".into()]).expect("nudge path ok");
    assert!(v.get("path").and_then(|x| x.as_str()).is_some());
}

#[test]
fn nudge_list_shape_correct() {
    let v = nudge_cmd(&[]).expect("nudge list ok");
    assert!(v.get("path").is_some());
    assert!(v.get("n").is_some());
    assert!(v.get("nudges").and_then(|x| x.as_array()).is_some());
}

#[test]
fn nudge_add_rejects_non_integer_due() {
    let err = nudge_cmd(&["add".into(), "not-a-number".into(), "msg".into()]).unwrap_err();
    assert!(err.contains("integer"));
}

// ---- todo_cmd ----

fn temp_todo_store() -> (tempfile::TempDir, crate::agent::tools::todo::TodoStore) {
    let dir = tempfile::tempdir().expect("tmp");
    let store = crate::agent::tools::todo::TodoStore::new(dir.path().to_path_buf());
    (dir, store)
}

#[test]
fn todo_cmd_path_returns_dir() {
    let v = todo_cmd(&["path".into()]).expect("path ok");
    assert!(v.get("path").and_then(|p| p.as_str()).is_some());
}

#[test]
fn todo_cmd_list_empty_session_returns_empty() {
    let (_dir, store) = temp_todo_store();
    let v = todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
    assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(0));
    let items = v
        .get("items")
        .and_then(|i| i.as_array())
        .expect("items array");
    assert!(items.is_empty());
}

#[test]
fn todo_cmd_list_requires_session() {
    let (_dir, store) = temp_todo_store();
    let err = todo_cmd_at(&["list".into()], &store).unwrap_err();
    assert!(err.contains("list"));
}

#[test]
fn todo_cmd_add_appends_and_persists() {
    let (_dir, store) = temp_todo_store();
    let v = todo_cmd_at(
        &[
            "add".into(),
            "session-1".into(),
            "t1".into(),
            "first".into(),
            "todo".into(),
            "item".into(),
        ],
        &store,
    )
    .expect("add ok");
    assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(1));

    // Re-read confirms persistence + multi-word title joined.
    let listed = todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
    let items = listed
        .get("items")
        .and_then(|i| i.as_array())
        .expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].get("title").and_then(|t| t.as_str()),
        Some("first todo item")
    );
    assert_eq!(
        items[0].get("status").and_then(|s| s.as_str()),
        Some("pending")
    );
}

#[test]
fn todo_cmd_add_with_note_flag() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &[
            "add".into(),
            "session-1".into(),
            "t1".into(),
            "title".into(),
            "--note".into(),
            "explanatory note".into(),
        ],
        &store,
    )
    .expect("add ok");
    let listed = todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
    let items = listed
        .get("items")
        .and_then(|i| i.as_array())
        .expect("items");
    assert_eq!(
        items[0].get("note").and_then(|n| n.as_str()),
        Some("explanatory note")
    );
}

#[test]
fn todo_cmd_add_rejects_duplicate_id() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "first".into()],
        &store,
    )
    .expect("first add ok");
    let err = todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "second".into()],
        &store,
    )
    .unwrap_err();
    assert!(err.contains("t1"));
}

#[test]
fn todo_cmd_add_requires_title() {
    let (_dir, store) = temp_todo_store();
    let err = todo_cmd_at(&["add".into(), "s1".into(), "t1".into()], &store).unwrap_err();
    assert!(err.contains("title"));
}

#[test]
fn todo_cmd_add_note_flag_requires_value() {
    let (_dir, store) = temp_todo_store();
    let err = todo_cmd_at(
        &[
            "add".into(),
            "s1".into(),
            "t1".into(),
            "title".into(),
            "--note".into(),
        ],
        &store,
    )
    .unwrap_err();
    assert!(err.contains("--note"));
}

#[test]
fn todo_cmd_set_status_updates_one_item() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "first".into()],
        &store,
    )
    .expect("add ok");
    let v = todo_cmd_at(
        &[
            "set-status".into(),
            "s1".into(),
            "t1".into(),
            "in_progress".into(),
        ],
        &store,
    )
    .expect("set-status ok");
    assert_eq!(
        v.get("status").and_then(|s| s.as_str()),
        Some("in_progress")
    );
}

#[test]
fn todo_cmd_set_status_accepts_dash_alias() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "first".into()],
        &store,
    )
    .expect("add ok");
    // Both `in_progress` and `in-progress` should work.
    todo_cmd_at(
        &[
            "set-status".into(),
            "s1".into(),
            "t1".into(),
            "in-progress".into(),
        ],
        &store,
    )
    .expect("dash alias accepted");
}

#[test]
fn todo_cmd_set_status_rejects_unknown_status() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "first".into()],
        &store,
    )
    .expect("add ok");
    let err = todo_cmd_at(
        &[
            "set-status".into(),
            "s1".into(),
            "t1".into(),
            "bogus".into(),
        ],
        &store,
    )
    .unwrap_err();
    assert!(err.contains("bogus"));
}

#[test]
fn todo_cmd_remove_drops_item() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "a".into()],
        &store,
    )
    .expect("add ok");
    todo_cmd_at(
        &["add".into(), "s1".into(), "t2".into(), "b".into()],
        &store,
    )
    .expect("add ok");
    let v = todo_cmd_at(&["remove".into(), "s1".into(), "t1".into()], &store).expect("remove ok");
    assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(1));
    let listed = todo_cmd_at(&["list".into(), "s1".into()], &store).expect("list ok");
    let items = listed
        .get("items")
        .and_then(|i| i.as_array())
        .expect("items");
    assert_eq!(items[0].get("id").and_then(|i| i.as_str()), Some("t2"));
}

#[test]
fn todo_cmd_remove_unknown_id_errs() {
    let (_dir, store) = temp_todo_store();
    let err = todo_cmd_at(&["remove".into(), "s1".into(), "ghost".into()], &store).unwrap_err();
    assert!(err.contains("ghost"));
}

#[test]
fn todo_cmd_clear_requires_yes_flag() {
    let (_dir, store) = temp_todo_store();
    let err = todo_cmd_at(&["clear".into(), "s1".into()], &store).unwrap_err();
    assert!(err.contains("--yes"));
}

#[test]
fn todo_cmd_clear_with_yes_wipes_session() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "a".into()],
        &store,
    )
    .expect("add ok");
    let v = todo_cmd_at(&["clear".into(), "s1".into(), "--yes".into()], &store).expect("clear ok");
    assert_eq!(v.get("cleared").and_then(|c| c.as_bool()), Some(true));
    let listed = todo_cmd_at(&["list".into(), "s1".into()], &store).expect("list ok");
    assert_eq!(listed.get("count").and_then(|c| c.as_u64()), Some(0));
}

#[test]
fn todo_cmd_list_includes_status_breakdown() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "a".into()],
        &store,
    )
    .expect("add ok");
    todo_cmd_at(
        &["add".into(), "s1".into(), "t2".into(), "b".into()],
        &store,
    )
    .expect("add ok");
    todo_cmd_at(
        &[
            "set-status".into(),
            "s1".into(),
            "t2".into(),
            "completed".into(),
        ],
        &store,
    )
    .expect("status ok");
    let listed = todo_cmd_at(&["list".into(), "s1".into()], &store).expect("list ok");
    let counts = listed.get("by_status").expect("by_status");
    assert_eq!(counts.get("pending").and_then(|n| n.as_u64()), Some(1));
    assert_eq!(counts.get("completed").and_then(|n| n.as_u64()), Some(1));
    assert_eq!(counts.get("in_progress").and_then(|n| n.as_u64()), Some(0));
    assert_eq!(counts.get("cancelled").and_then(|n| n.as_u64()), Some(0));
}
