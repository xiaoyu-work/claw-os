use super::*;
use crate::agent::memory::semantic::SemanticStore;
use crate::model::tasks::embed::{EmbedError, EmbedRequest, EmbedResponse, Embedder};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};

fn open_db() -> MemoryDb {
    MemoryDb::open_in_memory().unwrap()
}

fn open_store() -> Arc<SemanticStore> {
    Arc::new(SemanticStore::open_in_memory(None).unwrap())
}

struct FailingEmbedder {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Embedder for FailingEmbedder {
    fn name(&self) -> &str {
        "failing-test"
    }

    fn model(&self) -> &str {
        "failing-test/v1"
    }

    fn is_configured(&self) -> bool {
        true
    }

    async fn embed(&self, _request: EmbedRequest) -> Result<EmbedResponse, EmbedError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(EmbedError::Transport("temporary outage".into()))
    }
}

fn entry(source: &str, text: &str) -> AppMemoryEntry {
    AppMemoryEntry {
        source: source.into(),
        text: text.into(),
        kind: None,
        entity_id: None,
        tags: Vec::new(),
        link: None,
    }
}

#[tokio::test]
async fn remember_writes_fts_row_and_returns_outcome() {
    let db = open_db();
    let out = remember(&db, None, entry("expense-tracker", "Lunch at Eatsa"), false)
        .await
        .unwrap();
    assert!(out.row_id > 0);
    assert_eq!(out.session_id, "app:expense-tracker");
    assert!(out.stored_bytes > 0);
    assert!(!out.indexed_semantic, "no store given");
}

#[tokio::test]
async fn remember_rejects_empty_text() {
    let db = open_db();
    let err = remember(&db, None, entry("a", "   \n  "), false)
        .await
        .unwrap_err();
    assert!(matches!(err, RememberError::Invalid(_)));
}

#[tokio::test]
async fn remember_rejects_invalid_source() {
    let db = open_db();
    let err = remember(&db, None, entry("BadCaps", "hi"), false)
        .await
        .unwrap_err();
    assert!(matches!(err, RememberError::Invalid(_)));
}

#[tokio::test]
async fn list_returns_only_app_rows_newest_first() {
    let db = open_db();
    remember(&db, None, entry("a", "first"), false).await.unwrap();
    remember(&db, None, entry("a", "second"), false).await.unwrap();
    remember(&db, None, entry("b", "third"), false).await.unwrap();
    // Also stuff a regular session message — must not appear.
    db.record_message("ses_xx", "user", "private prompt").unwrap();
    let rows = list(&db, None, 10).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].text, "third");
    assert_eq!(rows[1].text, "second");
    assert_eq!(rows[2].text, "first");
}

#[tokio::test]
async fn list_filtered_by_source() {
    let db = open_db();
    remember(&db, None, entry("a", "alpha"), false).await.unwrap();
    remember(&db, None, entry("b", "beta"), false).await.unwrap();
    let rows = list(&db, Some("a"), 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text, "alpha");
    assert_eq!(rows[0].source, "a");
}

#[tokio::test]
async fn show_returns_structured_fields() {
    let db = open_db();
    let mut e = entry("expense-tracker", "Marriott NYC $487.50");
    e.kind = Some("event".into());
    e.entity_id = Some("expense-42".into());
    e.tags = vec!["expense".into(), "hotel".into(), "Hotel".into()]; // dedup case-insensitively
    e.link = Some("cos app expense-tracker show 42".into());
    let out = remember(&db, None, e, false).await.unwrap();
    let row = show(&db, out.row_id).unwrap().expect("row exists");
    assert_eq!(row.text, "Marriott NYC $487.50");
    assert_eq!(row.kind.as_deref(), Some("event"));
    assert_eq!(row.entity_id.as_deref(), Some("expense-42"));
    assert_eq!(row.tags, vec!["expense", "hotel"]);
    assert_eq!(row.link.as_deref(), Some("cos app expense-tracker show 42"));
    assert_eq!(row.source, "expense-tracker");
}

#[tokio::test]
async fn search_finds_by_natural_text_and_by_tag() {
    let db = open_db();
    let mut e = entry("expense-tracker", "Lunch at Eatsa with the team");
    e.tags = vec!["lunch".into(), "team".into()];
    remember(&db, None, e, false).await.unwrap();
    remember(&db, None, entry("calendar", "Dinner at Eatsa solo"), false)
        .await
        .unwrap();

    let hits = search(&db, "Eatsa", None, 10).unwrap();
    assert_eq!(hits.len(), 2);

    let hits = search(&db, "team", None, 10).unwrap();
    assert_eq!(hits.len(), 1, "tags are FTS-indexed");
    assert_eq!(hits[0].source, "expense-tracker");
}

#[tokio::test]
async fn search_scoped_by_source() {
    let db = open_db();
    remember(&db, None, entry("a", "shared keyword"), false)
        .await
        .unwrap();
    remember(&db, None, entry("b", "shared keyword"), false)
        .await
        .unwrap();
    let hits = search(&db, "shared", Some("a"), 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source, "a");
}

#[tokio::test]
async fn forget_source_removes_only_that_source() {
    let db = open_db();
    remember(&db, None, entry("a", "alpha"), false).await.unwrap();
    remember(&db, None, entry("b", "beta"), false).await.unwrap();
    let n = forget_source(&db, None, "a").unwrap();
    assert_eq!(n, 1);
    let rows = list(&db, None, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "b");
}

#[tokio::test]
async fn forget_row_removes_only_that_row() {
    let db = open_db();
    let out1 = remember(&db, None, entry("a", "first"), false).await.unwrap();
    let _out2 = remember(&db, None, entry("a", "second"), false).await.unwrap();
    let ok = forget_row(&db, None, out1.row_id).unwrap();
    assert!(ok);
    let rows = list(&db, None, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text, "second");
}

#[tokio::test]
async fn forget_row_returns_false_for_unknown_id() {
    let db = open_db();
    let ok = forget_row(&db, None, 9_999_999).unwrap();
    assert!(!ok);
}

#[tokio::test]
async fn remember_with_disabled_embedder_does_not_error() {
    let db = open_db();
    let store = open_store(); // no embedder
    let out = remember(
        &db,
        Some(&store),
        entry("expense-tracker", "indexable text"),
        true,
    )
    .await
    .unwrap();
    // Should fall back gracefully when the store is configured
    // without an embedder.
    assert!(!out.indexed_semantic);
}

#[tokio::test]
async fn remember_semantic_failure_returns_fts_success_without_retry() {
    let db = open_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let embedder: Arc<dyn Embedder> = Arc::new(FailingEmbedder {
        calls: Arc::clone(&calls),
    });
    let store = Arc::new(SemanticStore::open_in_memory(Some(embedder)).unwrap());

    let out = remember(
        &db,
        Some(&store),
        entry("expense-tracker", "Lunch at Eatsa"),
        true,
    )
    .await
    .expect("the committed FTS row must be reported as success");

    assert!(!out.indexed_semantic);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "embedding is not retried");
    let rows = list(&db, Some("expense-tracker"), 10).unwrap();
    assert_eq!(rows.len(), 1, "the FTS row remains available");
    assert_eq!(rows[0].id, out.row_id);
}

#[test]
fn parse_content_recovers_full_metadata() {
    let content = "Hello world\n\nSource: my-app\nKind: event\nEntity: x-1\nTags: a, b\nLink: cos app my-app show x-1";
    let p = parse_content(content);
    assert_eq!(p.text, "Hello world");
    assert_eq!(p.source.as_deref(), Some("my-app"));
    assert_eq!(p.kind.as_deref(), Some("event"));
    assert_eq!(p.entity_id.as_deref(), Some("x-1"));
    assert_eq!(p.tags, vec!["a", "b"]);
    assert_eq!(
        p.link.as_deref(),
        Some("cos app my-app show x-1")
    );
}

#[test]
fn parse_content_with_no_suffix() {
    let content = "Just text, no metadata";
    let p = parse_content(content);
    assert_eq!(p.text, "Just text, no metadata");
    assert!(p.source.is_none());
    assert!(p.kind.is_none());
    assert!(p.tags.is_empty());
}

#[test]
fn parse_content_with_colons_in_body() {
    // A body that contains "X: y" lines should NOT be misparsed as
    // metadata because the suffix block must be separated by a
    // blank line.
    let content = "Note: I had a thought\nIt was about cats\n\nSource: my-app";
    let p = parse_content(content);
    assert_eq!(p.text, "Note: I had a thought\nIt was about cats");
    assert_eq!(p.source.as_deref(), Some("my-app"));
}

#[test]
fn validate_source_rejects_garbage() {
    assert!(validate_source("").is_err());
    assert!(validate_source("Bad").is_err());
    assert!(validate_source("9start").is_err());
    assert!(validate_source("with space").is_err());
    assert!(validate_source("a-good_one1").is_ok());
}

#[test]
fn entry_sanitize_dedups_and_lowercases_tags() {
    let e = AppMemoryEntry {
        source: "expense-tracker".into(),
        text: "x".into(),
        kind: Some("  Event  ".into()),
        entity_id: None,
        tags: vec![
            "Hotel".into(),
            "hotel".into(),
            "".into(),
            "Hotel".into(),
            "TRAVEL".into(),
        ],
        link: None,
    }
    .sanitize();
    assert_eq!(e.source, "expense-tracker");
    assert_eq!(e.kind.as_deref(), Some("event"));
    assert_eq!(e.tags, vec!["hotel", "travel"]);
}
