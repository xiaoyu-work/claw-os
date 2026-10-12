use super::*;

#[test]
fn inspection_reads_existing_notes_and_checked_edits_preserve_newer_work() {
    let root = tempfile::tempdir().unwrap();
    let notes = NotesStore::at(root.path().join("notes"));
    notes.write("USER.md", "Prefer concise replies.").unwrap();
    let note = read_at(
        root.path(),
        MemoryReference::Note {
            name: "USER.md".into(),
        },
    )
    .unwrap();
    assert!(note.editable);
    update_at(
        root.path(),
        UpdateRequest {
            name: "USER.md".into(),
            revision: note.revision.clone(),
            content: "Prefer short replies.".into(),
        },
    )
    .unwrap();
    assert!(update_at(
        root.path(),
        UpdateRequest {
            name: "USER.md".into(),
            revision: note.revision,
            content: "stale edit".into(),
        }
    )
    .is_err());
    let current = read_at(
        root.path(),
        MemoryReference::Note {
            name: "USER.md".into(),
        },
    )
    .unwrap();
    assert_eq!(current.content, "Prefer short replies.");
    assert!(forget_at(
        root.path(),
        ForgetRequest {
            reference: current.reference.clone(),
            revision: current.revision.clone(),
            confirm: false,
        }
    )
    .is_err());
    forget_at(
        root.path(),
        ForgetRequest {
            reference: current.reference,
            revision: current.revision,
            confirm: true,
        },
    )
    .unwrap();
    assert!(notes.read("USER.md").unwrap().is_none());
}

#[test]
fn inspection_never_creates_missing_databases_or_exposes_private_rows() {
    let root = tempfile::tempdir().unwrap();
    inspect_at(
        root.path(),
        InspectRequest {
            kind: MemoryKind::Apps,
            query: String::new(),
            limit: 50,
        },
    )
    .unwrap();
    assert!(!root.path().join("memory.db").exists());
    let database = MemoryDb::open(root.path().join("memory.db")).unwrap();
    let id = database
        .record_message("conversation", "user", "Remember this interaction")
        .unwrap();
    let message = read_at(root.path(), MemoryReference::Message { id: id.to_string() }).unwrap();
    assert!(!message.editable);
    assert!(!message.deletable);
    assert!(forget_at(
        root.path(),
        ForgetRequest {
            reference: message.reference,
            revision: message.revision,
            confirm: true,
        }
    )
    .is_err());
    assert!(database.message(id).unwrap().is_some());
}

#[test]
fn memory_entry_references_reject_paths_and_hidden_authority() {
    let root = tempfile::tempdir().unwrap();
    assert!(read_at(
        root.path(),
        MemoryReference::Note {
            name: "../private.md".into()
        }
    )
    .is_err());
    assert!(serde_json::from_value::<UpdateRequest>(serde_json::json!({
        "name":"USER.md","revision":"new","content":"note","owner_uid":0
    }))
    .is_err());
}
