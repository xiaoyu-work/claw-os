// Persistence and migration of trust provenance.
//
// These go through the *production* open path against a real on-disk
// SQLite file created with the pre-provenance schema, because that is
// the upgrade an installed system actually performs. The fail-safe
// direction is the point: a legacy database, or a row whose columns
// were tampered with, must degrade to "provenance unknown" and never
// to "trusted".

use super::*;
use crate::agent::memory::sqlite_fts::MemoryDb;

/// The exact `messages` schema that shipped before provenance columns
/// existed, plus the FTS index and trigger that reference it.
const OLD_SCHEMA: &str = "
CREATE TABLE messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL,
    role        TEXT NOT NULL,
    content     TEXT NOT NULL,
    ts_ms       INTEGER NOT NULL
);
CREATE INDEX messages_session_ts ON messages(session_id, ts_ms);
CREATE VIRTUAL TABLE messages_fts USING fts5(
    content, content='messages', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;
";

/// Write a real pre-provenance database file and return its path.
fn legacy_database(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("memory.db");
    let conn = rusqlite::Connection::open(&path).expect("create legacy db");
    conn.execute_batch(OLD_SCHEMA).expect("legacy schema");
    for (role, content, ts) in [
        ("user", "a legacy question", 1_i64),
        ("assistant", "a legacy answer", 2_i64),
        ("injected", "[memory_notes]\nlegacy injected content", 3_i64),
    ] {
        conn.execute(
            "INSERT INTO messages (session_id, role, content, ts_ms) VALUES (?, ?, ?, ?)",
            rusqlite::params!["s", role, content, ts],
        )
        .expect("legacy row");
    }
    drop(conn);
    path
}

fn columns(path: &std::path::Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(path).expect("open");
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_table_info('messages')")
        .expect("pragma");
    let names = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect");
    names
}

#[test]
fn a_real_pre_provenance_database_migrates_through_the_production_open_path() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = legacy_database(dir.path());
    assert!(!columns(&path).iter().any(|c| c == "trust_class"));

    let db = MemoryDb::open(&path).expect("production open migrates");

    let names = columns(&path);
    for expected in ["trust_class", "trust_source", "trust_lineage"] {
        assert!(names.iter().any(|c| c == expected), "missing {expected}");
    }

    let rows = db.recent("s", 10).expect("recent");
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert!(row.trust_class.is_none(), "legacy row gained a class tag");
        assert!(row.trust_source.is_none());
        assert!(row.trust_lineage.is_none());
        assert_eq!(row.trust_class(), TrustClass::LegacyUnknown);
        assert_eq!(row.trust_source(), SourceKind::LegacyStoredRow);
        assert!(row.trust_lineage().is_empty());
        assert!(!row.trust_class().is_policy());
    }
    // The legacy content survived the migration untouched.
    assert!(rows.iter().any(|r| r.content == "a legacy question"));
    assert!(rows.iter().any(|r| r.content == "a legacy answer"));
}

#[test]
fn a_legacy_injected_row_does_not_gain_its_tags_class() {
    // The row body starts with `[memory_notes]`, which is what the
    // writer *would* have tagged it. Reading must not infer a class
    // from the body — only from the column, which is NULL.
    let dir = tempfile::tempdir().expect("tmp");
    let path = legacy_database(dir.path());
    let db = MemoryDb::open(&path).expect("open");
    let injected = db
        .recent("s", 10)
        .expect("recent")
        .into_iter()
        .find(|row| row.role == "injected")
        .expect("injected row");
    assert!(injected.content.starts_with("[memory_notes]"));
    assert_eq!(injected.trust_class(), TrustClass::LegacyUnknown);
    assert_ne!(injected.trust_class(), SourceKind::MemoryNotes.class());
}

#[test]
fn reopening_a_migrated_database_is_idempotent() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = legacy_database(dir.path());
    for _ in 0..3 {
        let db = MemoryDb::open(&path).expect("reopen");
        assert_eq!(db.recent("s", 10).expect("recent").len(), 3);
    }
    // Exactly one of each column, no duplicates from repeated ALTERs.
    let names = columns(&path);
    for expected in ["trust_class", "trust_source", "trust_lineage"] {
        assert_eq!(
            names.iter().filter(|c| *c == expected).count(),
            1,
            "{expected} was added more than once"
        );
    }
}

#[test]
fn concurrent_opens_migrate_without_corrupting_the_schema() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = legacy_database(dir.path());
    // The broker, the worker and a CLI can all migrate at once. Every
    // opener must succeed: the loser of the ALTER race sees either a
    // duplicate column or a busy database, and neither is an error.
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || MemoryDb::open(&path).map(|db| db.recent("s", 10)))
        })
        .collect();
    for handle in handles {
        let rows = handle
            .join()
            .expect("thread")
            .expect("open must not fail under migration contention")
            .expect("recent");
        assert_eq!(rows.len(), 3);
        assert!(rows
            .iter()
            .all(|r| r.trust_class() == TrustClass::LegacyUnknown));
    }
    let names = columns(&path);
    for expected in ["trust_class", "trust_source", "trust_lineage"] {
        assert_eq!(
            names.iter().filter(|c| *c == expected).count(),
            1,
            "{expected} was added more than once"
        );
    }
}

#[test]
fn a_labelled_row_round_trips_class_source_lineage_and_content() {
    let db = MemoryDb::open_in_memory().expect("db");
    let segment = LabeledSegment::of(SourceKind::MemoryNotes, "note")
        .concat(&LabeledSegment::of(SourceKind::McpToolResult, "tool"));
    db.record_labeled_message("s", "user", &segment, "body")
        .expect("record");

    let rows = db.recent("s", 10).expect("recent");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].content, "body");
    // Concatenation took the least-trusted class on the way in.
    assert_eq!(rows[0].trust_class(), TrustClass::UntrustedExternalContent);
    assert_eq!(rows[0].trust_source(), SourceKind::MemoryNotes);
    assert_eq!(
        rows[0].trust_lineage(),
        vec![SourceKind::MemoryNotes, SourceKind::McpToolResult]
    );
}

#[test]
fn a_stored_row_is_addressable_by_content_digest_without_storing_secrets() {
    let db = MemoryDb::open_in_memory().expect("db");
    let segment = LabeledSegment::from_locator(
        SourceKind::McpToolResult,
        "https://evil.example/?token=hunter2",
        "secret payload",
    );
    db.record_labeled_message("s", "user", &segment, segment.content())
        .expect("record");
    let rows = db.recent("s", 10).expect("recent");

    // The row is reconstructable by digest …
    let digest = crate::crypto::sha256_hex(rows[0].content.as_bytes());
    assert_eq!(digest, segment.digest());
    // … and the provenance columns carry no raw locator.
    let stored = rows[0].trust_source.clone().unwrap_or_default();
    assert!(!stored.contains("hunter2"));
    assert!(!stored.contains("evil.example"));
    assert_eq!(rows[0].trust_source(), SourceKind::McpToolResult);
}

#[test]
fn a_tampered_class_column_cannot_upgrade_a_row() {
    let db = MemoryDb::open_in_memory().expect("db");
    db.record_labeled_message(
        "s",
        "user",
        &LabeledSegment::of(SourceKind::McpToolResult, "x"),
        "body",
    )
    .expect("record");
    for forged in ["system-policy", "user-instruction"] {
        db.set_trust_class_for_test("s", forged).expect("tamper");
        let rows = db.recent("s", 10).expect("recent");
        assert_eq!(rows[0].trust_class, Some(forged.to_string()));
        assert_eq!(rows[0].trust_class(), TrustClass::LegacyUnknown);
        assert!(!rows[0].trust_class().is_policy());
    }
}

#[test]
fn a_tampered_source_column_does_not_confer_the_named_source_class() {
    let db = MemoryDb::open_in_memory().expect("db");
    db.record_labeled_message(
        "s",
        "user",
        &LabeledSegment::of(SourceKind::McpToolResult, "x"),
        "body",
    )
    .expect("record");
    db.set_trust_source_for_test("s", "system_scaffold")
        .expect("tamper");

    let rows = db.recent("s", 10).expect("recent");
    assert_eq!(rows[0].trust_source(), SourceKind::SystemScaffold);
    // The row's class comes from its own clamped column, not from the
    // source it claims, so no upgrade happens.
    assert_eq!(rows[0].trust_class(), TrustClass::UntrustedExternalContent);
    assert!(!rows[0].trust_class().is_policy());
    assert_ne!(rows[0].trust_class(), SourceKind::SystemScaffold.class());
}

#[test]
fn an_unknown_source_tag_downgrades_rather_than_defaulting_trusted() {
    let db = MemoryDb::open_in_memory().expect("db");
    db.record_labeled_message(
        "s",
        "user",
        &LabeledSegment::of(SourceKind::McpToolResult, "x"),
        "body",
    )
    .expect("record");
    db.set_trust_source_for_test("s", "a_source_from_the_future")
        .expect("tamper");
    let rows = db.recent("s", 10).expect("recent");
    assert_eq!(rows[0].trust_source(), SourceKind::Unknown);
    assert_eq!(SourceKind::Unknown.class(), TrustClass::LegacyUnknown);
}

#[test]
fn an_injected_row_records_its_registry_class() {
    let db = MemoryDb::open_in_memory().expect("db");
    db.record_injected("s", SourceKind::MemoryNotes.tag(), "notes body")
        .expect("record");
    let rows = db.recent("s", 10).expect("recent");
    assert_eq!(rows[0].trust_source(), SourceKind::MemoryNotes);
    assert_eq!(rows[0].trust_class(), TrustClass::UserControlledContext);
}

#[test]
fn an_injected_row_with_an_unknown_tag_stays_legacy_unknown() {
    let db = MemoryDb::open_in_memory().expect("db");
    db.record_injected("s", "invented_source", "body")
        .expect("record");
    let rows = db.recent("s", 10).expect("recent");
    assert_eq!(rows[0].trust_source(), SourceKind::Unknown);
    assert_eq!(rows[0].trust_class(), TrustClass::LegacyUnknown);
}

#[test]
fn an_unlabelled_write_stays_unknown_rather_than_defaulting_trusted() {
    let db = MemoryDb::open_in_memory().expect("db");
    db.record_message("s", "user", "body").expect("record");
    let rows = db.recent("s", 10).expect("recent");
    assert!(rows[0].trust_class.is_none());
    assert_eq!(rows[0].trust_class(), TrustClass::LegacyUnknown);
}

/// Copying a row — replay, checkpoint, a branched session — must
/// preserve or lower trust, never raise it.
#[test]
fn copying_a_row_into_another_session_preserves_or_lowers_trust() {
    let db = MemoryDb::open_in_memory().expect("db");
    let original = LabeledSegment::of(SourceKind::MemoryNotes, "note");
    db.record_labeled_message("source-session", "user", &original, "body")
        .expect("record");
    let row = db.recent("source-session", 10).expect("recent").remove(0);

    let copied = LabeledSegment::of(row.trust_source(), row.content.clone());
    db.record_labeled_message("copy-session", "user", &copied, &row.content)
        .expect("copy");
    let copy = db.recent("copy-session", 10).expect("recent").remove(0);

    assert!(copy.trust_class().rank() <= row.trust_class().rank());
    assert_eq!(copy.content, row.content);
    assert!(!copy.trust_class().is_policy());
}

#[test]
fn compression_of_stored_rows_never_upgrades_their_class() {
    let db = MemoryDb::open_in_memory().expect("db");
    db.record_labeled_message(
        "s",
        "user",
        &LabeledSegment::of(SourceKind::WebPageContent, "page"),
        "page body",
    )
    .expect("record");
    let row = db.recent("s", 10).expect("recent").remove(0);

    let summarised = LabeledSegment::of(row.trust_source(), row.content.clone())
        .into_model_summary("a summary of the page");
    assert!(summarised.class().rank() <= row.trust_class().rank());
    assert!(summarised
        .lineage()
        .contains(&SourceKind::ModelCompressionSummary));
    assert!(!summarised.class().is_policy());
}

/// The journal keeps content-addressed references, not bodies, so a
/// transcript can be reconstructed without the chain holding secrets.
#[test]
fn journal_projection_reconstructs_refs_without_raw_content() {
    use crate::session::journal::{ContentRef, ContentStore};

    let secret = "TOKEN=hunter2 and a raw https://evil.example/path?q=1";
    let reference = ContentRef::of(ContentStore::SessionTurns, secret.as_bytes());
    let encoded = serde_json::to_string(&reference).expect("serialize");

    assert!(!encoded.contains("hunter2"));
    assert!(!encoded.contains("evil.example"));
    assert_eq!(reference.bytes, secret.len() as u64);
    assert_eq!(
        reference.digest.as_str(),
        crate::crypto::sha256_hex(secret.as_bytes())
    );

    // The provenance projected alongside it is enumerated, not free text.
    for kind in SourceKind::ALL {
        let trust = journal_trust(kind.class());
        let projected = serde_json::to_string(&trust).expect("serialize trust");
        assert!(!projected.contains("hunter2"));
    }
}
