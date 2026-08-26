use super::*;
use crate::agent::curator::SkillConfidence;

fn sample_draft(suggested_id: &str) -> SkillDraft {
    SkillDraft {
        suggested_id: suggested_id.to_string(),
        title: "demo".into(),
        description: "test draft".into(),
        allowed_tools: vec!["echo".into(), "now".into()],
        turns_used: 4,
        confidence: SkillConfidence::Medium,
    }
}

fn tmp_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "cos-curator-drafts-{label}-{}.json",
        uuid::Uuid::new_v4().simple()
    ))
}

#[test]
fn open_at_nonexistent_returns_empty_store() {
    let p = tmp_path("nonexistent");
    let store = DraftStore::open_at(p.clone()).expect("open");
    assert!(store.list().is_empty());
    // open should NOT create the file until the first write.
    assert!(!p.exists());
}

#[test]
fn add_persists_and_reload_roundtrips() {
    let p = tmp_path("roundtrip");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let id = store
        .add("sess-1".into(), sample_draft("first"))
        .expect("add");
    assert_eq!(store.list().len(), 1);

    let reopened = DraftStore::open_at(p.clone()).unwrap();
    assert_eq!(reopened.list().len(), 1);
    let got = reopened.get(&id).expect("present");
    assert_eq!(got.session_id, "sess-1");
    assert_eq!(got.draft.suggested_id, "first");
    assert_eq!(got.status, DraftStatus::Proposed);
    assert!(got.note.is_none());

    std::fs::remove_file(&p).ok();
}

#[test]
fn add_assigns_unique_ids() {
    let p = tmp_path("unique-ids");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let a = store.add("s".into(), sample_draft("a")).unwrap();
    let b = store.add("s".into(), sample_draft("b")).unwrap();
    let c = store.add("s".into(), sample_draft("c")).unwrap();
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);
    std::fs::remove_file(&p).ok();
}

#[test]
fn set_status_transitions_and_persists() {
    let p = tmp_path("status");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let id = store.add("s".into(), sample_draft("x")).unwrap();
    store
        .set_status(&id, DraftStatus::Accepted, Some("looks good".into()))
        .expect("ok");
    let reopened = DraftStore::open_at(p.clone()).unwrap();
    let r = reopened.get(&id).unwrap();
    assert_eq!(r.status, DraftStatus::Accepted);
    assert_eq!(r.note.as_deref(), Some("looks good"));
    std::fs::remove_file(&p).ok();
}

#[test]
fn set_status_unknown_id_errors() {
    let p = tmp_path("unknown-status");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let err = store
        .set_status("nope", DraftStatus::Accepted, None)
        .unwrap_err();
    assert!(err.contains("no draft"));
    std::fs::remove_file(&p).ok();
}

#[test]
fn delete_removes_and_persists() {
    let p = tmp_path("delete");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let id = store.add("s".into(), sample_draft("d")).unwrap();
    store.delete(&id).expect("ok");
    assert!(store.list().is_empty());
    let reopened = DraftStore::open_at(p.clone()).unwrap();
    assert!(reopened.list().is_empty());
    std::fs::remove_file(&p).ok();
}

#[test]
fn delete_unknown_id_errors() {
    let p = tmp_path("unknown-delete");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let err = store.delete("nope").unwrap_err();
    assert!(err.contains("no draft"));
    std::fs::remove_file(&p).ok();
}

#[test]
fn empty_file_is_treated_as_empty_store() {
    let p = tmp_path("empty");
    std::fs::write(&p, b"").unwrap();
    let store = DraftStore::open_at(p.clone()).expect("open");
    assert!(store.list().is_empty());
    std::fs::remove_file(&p).ok();
}

#[test]
fn malformed_file_returns_error() {
    let p = tmp_path("malformed");
    std::fs::write(&p, b"{this is not json").unwrap();
    let err = DraftStore::open_at(p.clone()).unwrap_err();
    assert!(err.contains("parse"));
    std::fs::remove_file(&p).ok();
}

#[test]
fn schema_mismatch_returns_error() {
    let p = tmp_path("schema-bad");
    std::fs::write(&p, br#"{"schema":99,"drafts":[]}"#).unwrap();
    let err = DraftStore::open_at(p.clone()).unwrap_err();
    assert!(err.contains("schema"));
    std::fs::remove_file(&p).ok();
}

#[test]
fn note_is_optional_and_skipped_when_none_passed() {
    let p = tmp_path("note-none");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let id = store.add("s".into(), sample_draft("n")).unwrap();
    store
        .set_status(&id, DraftStatus::Accepted, Some("first".into()))
        .unwrap();
    // Second transition with note=None should keep the prior note.
    store.set_status(&id, DraftStatus::Rejected, None).unwrap();
    let r = store.get(&id).unwrap();
    assert_eq!(r.status, DraftStatus::Rejected);
    assert_eq!(r.note.as_deref(), Some("first"));
    std::fs::remove_file(&p).ok();
}

#[test]
fn save_atomic_uses_tmp_file_suffix() {
    let p = tmp_path("atomic");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    store.add("s".into(), sample_draft("a")).unwrap();
    // No leftover tmp file *for this path*. The shared helper uses
    // hidden per-process `.<name>.<pid>.<nonce>.tmp` siblings and
    // renames them into place — none should remain. Restrict the
    // scan to this test's stem so we don't accidentally pick up
    // .tmp files left by other tests that share `/tmp`.
    let stem = p.file_name().unwrap().to_string_lossy().into_owned();
    if let Some(parent) = p.parent() {
        for entry in std::fs::read_dir(parent).unwrap().flatten() {
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
    std::fs::remove_file(&p).ok();
}

#[test]
fn set_title_updates_embedded_skill_draft_title() {
    let p = tmp_path("retitle-ok");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let id = store.add("s".into(), sample_draft("a")).unwrap();
    store.set_title(&id, "Brand-New Title").unwrap();
    let r = store.get(&id).unwrap();
    assert_eq!(r.draft.title, "Brand-New Title");
    // Reload from disk to confirm it persisted.
    let store2 = DraftStore::open_at(p.clone()).unwrap();
    assert_eq!(store2.get(&id).unwrap().draft.title, "Brand-New Title");
    std::fs::remove_file(&p).ok();
}

#[test]
fn set_title_trims_whitespace() {
    let p = tmp_path("retitle-trim");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let id = store.add("s".into(), sample_draft("b")).unwrap();
    store.set_title(&id, "   Padded Title   ").unwrap();
    assert_eq!(store.get(&id).unwrap().draft.title, "Padded Title");
    std::fs::remove_file(&p).ok();
}

#[test]
fn set_title_rejects_empty_or_whitespace() {
    let p = tmp_path("retitle-empty");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let id = store.add("s".into(), sample_draft("c")).unwrap();
    let err = store.set_title(&id, "").unwrap_err();
    assert!(err.contains("must not be empty"));
    let err = store.set_title(&id, "   \t\n").unwrap_err();
    assert!(err.contains("must not be empty"));
    // Original title preserved.
    assert_eq!(store.get(&id).unwrap().draft.title, "demo");
    std::fs::remove_file(&p).ok();
}

#[test]
fn set_title_unknown_id_errors() {
    let p = tmp_path("retitle-unknown");
    let mut store = DraftStore::open_at(p.clone()).unwrap();
    let err = store.set_title("does-not-exist", "anything").unwrap_err();
    assert!(err.contains("no draft with id"));
    std::fs::remove_file(&p).ok();
}
