use super::*;

fn tmp_store() -> (TodoStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = TodoStore::new(dir.path().to_path_buf());
    (store, dir)
}

#[test]
fn read_missing_session_returns_empty() {
    let (store, _g) = tmp_store();
    let list = store.read("nope").unwrap();
    assert!(list.items.is_empty());
}

#[test]
fn write_then_read_round_trips() {
    let (store, _g) = tmp_store();
    let list = TodoList {
        items: vec![
            TodoItem {
                id: "a".into(),
                title: "do A".into(),
                status: TodoStatus::Pending,
                note: None,
            },
            TodoItem {
                id: "b".into(),
                title: "do B".into(),
                status: TodoStatus::InProgress,
                note: Some("reason".into()),
            },
        ],
    };
    store.write("s1", &list).unwrap();
    let back = store.read("s1").unwrap();
    assert_eq!(back.items, list.items);
}

#[test]
fn write_rejects_duplicate_ids() {
    let (store, _g) = tmp_store();
    let list = TodoList {
        items: vec![
            TodoItem {
                id: "a".into(),
                title: "x".into(),
                status: TodoStatus::Pending,
                note: None,
            },
            TodoItem {
                id: "a".into(),
                title: "y".into(),
                status: TodoStatus::Pending,
                note: None,
            },
        ],
    };
    let err = store.write("s1", &list).unwrap_err();
    assert!(err.contains("duplicate"));
}

#[test]
fn write_rejects_empty_id_and_title() {
    let (store, _g) = tmp_store();
    let list = TodoList {
        items: vec![TodoItem {
            id: " ".into(),
            title: "x".into(),
            status: TodoStatus::Pending,
            note: None,
        }],
    };
    assert!(store.write("s1", &list).is_err());
    let list = TodoList {
        items: vec![TodoItem {
            id: "a".into(),
            title: "  ".into(),
            status: TodoStatus::Pending,
            note: None,
        }],
    };
    assert!(store.write("s1", &list).is_err());
}

#[test]
fn set_status_updates_one_item() {
    let (store, _g) = tmp_store();
    store
        .write(
            "s1",
            &TodoList {
                items: vec![TodoItem {
                    id: "a".into(),
                    title: "x".into(),
                    status: TodoStatus::Pending,
                    note: None,
                }],
            },
        )
        .unwrap();
    let updated = store.set_status("s1", "a", TodoStatus::Completed).unwrap();
    assert_eq!(updated.items[0].status, TodoStatus::Completed);
}

#[test]
fn set_status_unknown_id_errors() {
    let (store, _g) = tmp_store();
    store.write("s1", &TodoList::default()).unwrap();
    let err = store
        .set_status("s1", "ghost", TodoStatus::Completed)
        .unwrap_err();
    assert!(err.contains("not found"));
}

#[test]
fn clear_removes_session_file() {
    let (store, _g) = tmp_store();
    store
        .write(
            "s1",
            &TodoList {
                items: vec![TodoItem {
                    id: "a".into(),
                    title: "x".into(),
                    status: TodoStatus::Pending,
                    note: None,
                }],
            },
        )
        .unwrap();
    store.clear("s1").unwrap();
    let after = store.read("s1").unwrap();
    assert!(after.items.is_empty());
}

#[test]
fn clear_missing_session_is_noop() {
    let (store, _g) = tmp_store();
    store.clear("never-existed").unwrap();
}

#[test]
fn session_id_path_traversal_rejected() {
    let (store, _g) = tmp_store();
    assert!(store.read("../escape").is_err());
    assert!(store.read("a/b").is_err());
    assert!(store.read("a\\b").is_err());
    assert!(store.read("").is_err());
    assert!(store.read("  ").is_err());
    assert!(store.read("with\0null").is_err());
}

/// Windows treats these names as character devices regardless of
/// case or suffix. Reject them up-front so cross-OS deployments
/// can't end up with an unwriteable file (or worse, one that
/// blocks on `fs::write` because it's bound to the console).
#[test]
fn session_id_windows_reserved_names_rejected() {
    let (store, _g) = tmp_store();
    for s in [
        "CON", "con", "Con", "PRN", "AUX", "NUL", "COM1", "lpt9",
        // Reserved stem with an extension is still reserved on
        // Windows ("CON.json" resolves to the console).
        "CON.json", "nul.foo",
    ] {
        assert!(
            store.read(s).is_err(),
            "{s:?} should be rejected as reserved"
        );
    }
    // Sanity: 'concrete' is NOT reserved.
    assert!(store.read("concrete").is_ok());
}

/// Two threads racing `set_status` on the same session must both
/// commit — without the per-session RMW lock, the later writer
/// silently drops the earlier writer's update because both read
/// the same baseline list. With the lock, both updates are
/// observed in the final on-disk list.
#[test]
fn set_status_serialises_concurrent_writers() {
    use std::sync::Arc;
    let (store, _g) = tmp_store();
    let store = Arc::new(store);

    // Seed two items, both pending.
    store
        .write(
            "race-session",
            &TodoList {
                items: vec![
                    TodoItem {
                        id: "a".into(),
                        title: "first".into(),
                        status: TodoStatus::Pending,
                        note: None,
                    },
                    TodoItem {
                        id: "b".into(),
                        title: "second".into(),
                        status: TodoStatus::Pending,
                        note: None,
                    },
                ],
            },
        )
        .unwrap();

    let s1 = store.clone();
    let h1 = std::thread::spawn(move || {
        s1.set_status("race-session", "a", TodoStatus::Completed)
            .unwrap();
    });
    let s2 = store.clone();
    let h2 = std::thread::spawn(move || {
        s2.set_status("race-session", "b", TodoStatus::Completed)
            .unwrap();
    });
    h1.join().unwrap();
    h2.join().unwrap();

    let final_list = store.read("race-session").unwrap();
    let by_id: std::collections::HashMap<_, _> =
        final_list.items.iter().map(|i| (&i.id, i.status)).collect();
    assert_eq!(by_id[&"a".to_string()], TodoStatus::Completed);
    assert_eq!(by_id[&"b".to_string()], TodoStatus::Completed);
}

#[test]
fn render_list_marks_status_correctly() {
    let list = TodoList {
        items: vec![
            TodoItem {
                id: "a".into(),
                title: "one".into(),
                status: TodoStatus::Pending,
                note: None,
            },
            TodoItem {
                id: "b".into(),
                title: "two".into(),
                status: TodoStatus::InProgress,
                note: None,
            },
            TodoItem {
                id: "c".into(),
                title: "three".into(),
                status: TodoStatus::Completed,
                note: Some("notes".into()),
            },
            TodoItem {
                id: "d".into(),
                title: "four".into(),
                status: TodoStatus::Cancelled,
                note: None,
            },
        ],
    };
    let rendered = render_list(&list);
    assert!(rendered.contains("[ ] a one"));
    assert!(rendered.contains("[~] b two"));
    assert!(rendered.contains("[x] c three"));
    assert!(rendered.contains("(notes)"));
    assert!(rendered.contains("[-] d four"));
}

#[test]
fn render_list_empty_message() {
    assert_eq!(render_list(&TodoList::default()), "(no todos)");
}

#[tokio::test]
async fn tool_exec_read_then_write_then_read_via_json() {
    let (store, _g) = tmp_store();
    let tool = Todo::new(store);
    let r = tool.exec(json!({"command":"read","session_id":"s1"})).await;
    assert!(!r.is_error);
    assert!(r.content.contains("(no todos)"));

    let r = tool
        .exec(json!({
            "command":"write",
            "session_id":"s1",
            "items":[{"id":"a","title":"plan","status":"in_progress"}]
        }))
        .await;
    assert!(!r.is_error);
    assert!(r.content.contains("wrote 1"));

    let r = tool.exec(json!({"command":"read","session_id":"s1"})).await;
    assert!(r.content.contains("[~] a plan"));
}

#[tokio::test]
async fn tool_exec_set_status_via_json() {
    let (store, _g) = tmp_store();
    let tool = Todo::new(store);
    tool.exec(json!({
        "command":"write",
        "session_id":"s1",
        "items":[{"id":"a","title":"task","status":"pending"}]
    }))
    .await;
    let r = tool
        .exec(json!({
            "command":"set_status",
            "session_id":"s1",
            "id":"a",
            "status":"completed"
        }))
        .await;
    assert!(!r.is_error);
    assert!(r.content.contains("[x] a task"));
}

#[tokio::test]
async fn tool_exec_invalid_command() {
    let (store, _g) = tmp_store();
    let tool = Todo::new(store);
    let r = tool.exec(json!({"command":"frob","session_id":"s1"})).await;
    assert!(r.is_error);
}

#[tokio::test]
async fn tool_exec_set_status_missing_args() {
    let (store, _g) = tmp_store();
    let tool = Todo::new(store);
    let r = tool
        .exec(json!({"command":"set_status","session_id":"s1"}))
        .await;
    assert!(r.is_error);
}

#[tokio::test]
async fn tool_exec_clear_via_json() {
    let (store, _g) = tmp_store();
    let tool = Todo::new(store);
    tool.exec(json!({
        "command":"write","session_id":"s1",
        "items":[{"id":"a","title":"task"}]
    }))
    .await;
    let r = tool
        .exec(json!({"command":"clear","session_id":"s1"}))
        .await;
    assert!(!r.is_error);
    let r = tool.exec(json!({"command":"read","session_id":"s1"})).await;
    assert!(r.content.contains("(no todos)"));
}

#[test]
fn tool_metadata() {
    let tool = Todo::new(TodoStore::new(std::env::temp_dir()));
    assert_eq!(tool.name(), "cos_todo");
    assert!(tool.description().contains("task list"));
    let schema = tool.input_schema();
    assert_eq!(schema["required"][0], "command");
}
