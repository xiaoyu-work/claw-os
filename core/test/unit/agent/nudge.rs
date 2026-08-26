use super::*;
use std::path::PathBuf;

fn tmp(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "cos-nudge-{label}-{}.json",
        Uuid::new_v4().simple()
    ))
}

fn n(message: &str, due: u64, repeat: Option<u64>) -> Nudge {
    Nudge {
        id: String::new(),
        message: message.to_string(),
        due_at_epoch_s: due,
        repeat_secs: repeat,
        tag: None,
        last_fired_epoch_s: None,
    }
}

#[test]
fn add_assigns_uuid_if_blank() {
    let p = tmp("uuid");
    let store = NudgeStore::new(&p);
    let id = store.add(n("hello", 100, None)).unwrap();
    assert!(!id.is_empty());
    let listed = store.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    fs::remove_file(&p).ok();
}

#[test]
fn add_keeps_supplied_id() {
    let p = tmp("supplied-id");
    let store = NudgeStore::new(&p);
    let nudge = Nudge {
        id: "my-nudge".to_string(),
        message: "x".to_string(),
        due_at_epoch_s: 0,
        repeat_secs: None,
        tag: None,
        last_fired_epoch_s: None,
    };
    let id = store.add(nudge).unwrap();
    assert_eq!(id, "my-nudge");
    fs::remove_file(&p).ok();
}

#[test]
fn due_filters_by_time() {
    let p = tmp("due");
    let store = NudgeStore::new(&p);
    store.add(n("past", 100, None)).unwrap();
    store.add(n("future", 9_999_999_999, None)).unwrap();
    let now = 200u64;
    let due = store.due(now);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].message, "past");
    fs::remove_file(&p).ok();
}

#[test]
fn fire_one_shot_deletes() {
    let p = tmp("oneshot");
    let store = NudgeStore::new(&p);
    let id = store.add(n("x", 100, None)).unwrap();
    let ok = store.fire(&id, 200).unwrap();
    assert!(ok);
    assert!(store.list().is_empty());
    fs::remove_file(&p).ok();
}

#[test]
fn fire_repeating_advances_due() {
    let p = tmp("repeat");
    let store = NudgeStore::new(&p);
    let id = store.add(n("ping", 100, Some(60))).unwrap();
    store.fire(&id, 200).unwrap();
    let listed = store.list();
    assert_eq!(listed.len(), 1);
    // Base = max(100, 200) = 200; new due = 260.
    assert_eq!(listed[0].due_at_epoch_s, 260);
    assert_eq!(listed[0].last_fired_epoch_s, Some(200));
    fs::remove_file(&p).ok();
}

#[test]
fn fire_unknown_returns_false() {
    let p = tmp("unknown");
    let store = NudgeStore::new(&p);
    let ok = store.fire("nope", 100).unwrap();
    assert!(!ok);
    fs::remove_file(&p).ok();
}

#[test]
fn remove_returns_true_only_if_existed() {
    let p = tmp("remove");
    let store = NudgeStore::new(&p);
    let id = store.add(n("x", 100, None)).unwrap();
    assert!(store.remove(&id).unwrap());
    assert!(!store.remove(&id).unwrap());
    fs::remove_file(&p).ok();
}

#[test]
fn list_empty_when_file_missing() {
    let p = tmp("missing");
    let store = NudgeStore::new(&p);
    assert!(store.list().is_empty());
}

#[test]
fn save_atomic_via_tmp_rename() {
    let p = tmp("atomic");
    let store = NudgeStore::new(&p);
    store.add(n("x", 100, None)).unwrap();
    // No per-process tmp file should linger after a successful save.
    // The shared atomic_write helper uses a hidden `.<name>.<pid>...tmp`
    // sibling and renames it into place. Restrict the scan to this
    // test's stem so we don't false-positive on `.tmp` files left
    // by other concurrently-running tests sharing `/tmp`.
    let stem = p.file_name().unwrap().to_string_lossy().into_owned();
    if let Some(parent) = p.parent() {
        for entry in fs::read_dir(parent).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.contains(&stem) {
                continue;
            }
            assert!(
                !name.ends_with(".tmp"),
                "no leftover tmp file expected for {stem}, got {name}"
            );
        }
    }
    assert!(p.exists());
    let _ = fs::remove_file(&p);
    let lock = p.with_file_name(format!(
        "{}.lock",
        p.file_name().unwrap().to_string_lossy()
    ));
    let _ = fs::remove_file(&lock);
}

#[test]
fn now_epoch_s_is_recent() {
    let n = now_epoch_s();
    // After 2025-01-01.
    assert!(n > 1_735_689_600);
}

#[test]
fn repeat_with_due_in_future_uses_due_as_base() {
    let p = tmp("repeat-future");
    let store = NudgeStore::new(&p);
    let id = store.add(n("ping", 1000, Some(60))).unwrap();
    // Fire when current time is BEFORE the due time.
    store.fire(&id, 500).unwrap();
    let listed = store.list();
    // base = max(1000, 500) = 1000; new due = 1060.
    assert_eq!(listed[0].due_at_epoch_s, 1060);
    fs::remove_file(&p).ok();
}
