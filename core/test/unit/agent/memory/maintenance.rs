use super::*;

#[test]
fn learned_memory_reset_preserves_conversations_and_non_memory_files() {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    std::fs::create_dir_all(&agent).unwrap();

    let db = MemoryDb::open(agent.join("memory.db")).unwrap();
    db.record_message("ses_conversation", "user", "keep this")
        .unwrap();
    db.record_message("app:calendar", "app", "forget this")
        .unwrap();

    let notes = NotesStore::at(agent.join("notes"));
    notes.write("MEMORY.md", "learned fact").unwrap();
    notes.write("USER.md", "user preference").unwrap();
    notes.write("PROJECT.md", "project note").unwrap();
    std::fs::write(agent.join("notes").join("keep.txt"), "not a memory note").unwrap();

    let semantic_path = agent.join("semantic.db");
    drop(SemanticStore::open(&semantic_path, None).unwrap());
    let semantic = rusqlite::Connection::open(&semantic_path).unwrap();
    semantic
        .execute(
            "INSERT INTO semantic_docs(namespace, key, text, model, dim, embedding, ts_ms)
             VALUES ('notes', 'memory', 'learned fact', 'fixture', 1, X'00000000', 1)",
            [],
        )
        .unwrap();
    drop(semantic);

    let report = reset_at(&agent).unwrap();
    assert_eq!(
        report,
        LearnedMemoryReset {
            notes_deleted: 3,
            app_memories_deleted: 1,
            semantic_rows_deleted: 1,
            conversations_preserved: true,
        }
    );

    let db = MemoryDb::open(agent.join("memory.db")).unwrap();
    assert_eq!(db.count_session("ses_conversation").unwrap(), 1);
    assert_eq!(app_memory::count_all(&db).unwrap(), 0);
    assert_eq!(notes.list().unwrap(), Vec::<String>::new());
    assert!(agent.join("notes").join("keep.txt").is_file());
    let semantic = SemanticStore::open(&semantic_path, None).unwrap();
    assert_eq!(semantic.count(None).unwrap(), 0);
}

#[cfg(unix)]
#[test]
fn learned_memory_reset_refuses_a_symlinked_notes_directory() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("MEMORY.md"), "must remain").unwrap();
    symlink(&outside, agent.join("notes")).unwrap();

    let error = reset_at(&agent).unwrap_err();
    assert!(error.contains("notes directory is not a regular directory"));
    assert_eq!(
        std::fs::read_to_string(outside.join("MEMORY.md")).unwrap(),
        "must remain"
    );
}
