use super::*;

fn tool() -> CosRecallTool {
    CosRecallTool::new(MemoryDb::open_in_memory().unwrap())
}

#[tokio::test]
async fn missing_command_is_tool_error() {
    let r = tool().exec(json!({})).await;
    assert!(r.is_error);
    assert!(r.content.contains("missing 'command'"));
}

#[tokio::test]
async fn search_finds_inserted_message() {
    let t = tool();
    t.db.record_message("s", "user", "the secret password is rosebud")
        .unwrap();
    let r = t
        .exec(json!({ "command": "search", "query": "rosebud" }))
        .await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("rosebud"));
}

#[tokio::test]
async fn search_hides_legacy_assistant_evidence_markers() {
    let t = tool();
    t.db.record_message(
        "s",
        "assistant",
        "Network is idle. [evidence:call_1 confidence=0.95]",
    )
    .unwrap();
    let result = t
        .exec(json!({ "command": "search", "query": "Network" }))
        .await;
    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("Network is idle."));
    assert!(!result.content.contains("[evidence:"));
}

#[tokio::test]
async fn search_without_query_errors() {
    let t = tool();
    let r = t.exec(json!({ "command": "search" })).await;
    assert!(r.is_error);
    assert!(r.content.contains("non-empty 'query'"));
}

#[tokio::test]
async fn recent_requires_session_id() {
    let t = tool();
    let r = t.exec(json!({ "command": "recent" })).await;
    assert!(r.is_error);
    assert!(r.content.contains("requires 'session_id'"));
}

#[tokio::test]
async fn recent_returns_session_messages() {
    let t = tool();
    t.db.record_message("alpha", "user", "first").unwrap();
    t.db.record_message("alpha", "assistant", "ok").unwrap();
    t.db.record_message("bravo", "user", "elsewhere").unwrap();
    let r = t
        .exec(json!({ "command": "recent", "session_id": "alpha" }))
        .await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("first"));
    assert!(r.content.contains("ok"));
    assert!(!r.content.contains("elsewhere"));
}

#[tokio::test]
async fn sessions_lists_distinct_session_ids() {
    let t = tool();
    t.db.record_message("a", "user", "x").unwrap();
    t.db.record_message("b", "user", "y").unwrap();
    let r = t.exec(json!({ "command": "sessions" })).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("\"a\""));
    assert!(r.content.contains("\"b\""));
}

#[tokio::test]
async fn stats_returns_total_count() {
    let t = tool();
    t.db.record_message("s", "user", "one").unwrap();
    t.db.record_message("s", "user", "two").unwrap();
    let r = t.exec(json!({ "command": "stats" })).await;
    assert!(!r.is_error, "{}", r.content);
    // total_messages: 2
    assert!(r.content.contains("\"total_messages\":2"));
}

#[tokio::test]
async fn limit_is_clamped() {
    let t = tool();
    for i in 0..5 {
        t.db.record_message("s", "user", &format!("m{i}")).unwrap();
    }
    // limit > MAX_LIMIT must be silently clamped, not rejected.
    let r = t
        .exec(json!({
            "command": "recent",
            "session_id": "s",
            "limit": MAX_LIMIT as i64 + 100,
        }))
        .await;
    assert!(!r.is_error, "{}", r.content);
}

/// Hostile / unusual queries (FTS5 column filters, embedded quotes,
/// operator keywords) must not raise an FTS5 syntax error — they
/// must round-trip as literal-match phrase queries.
#[tokio::test]
async fn search_query_with_fts_meta_chars_is_safe() {
    let t = tool();
    t.db.record_message("s", "user", r#"a "quoted" phrase: with colons"#)
        .unwrap();
    // Each of these would be an FTS5 syntax error or hijack a
    // column filter if we passed it through raw.
    for hostile in [
        r#""quoted""#,
        "body: secret",
        r#"hi"; DROP TABLE foo --"#,
        "AND OR NEAR(",
        "*wildcard",
        "-negation",
    ] {
        let r = t
            .exec(json!({ "command": "search", "query": hostile }))
            .await;
        assert!(
            !r.is_error,
            "FTS5 meta query {hostile:?} must not error: {}",
            r.content
        );
    }
}

#[test]
fn escape_fts5_query_quotes_input() {
    assert_eq!(escape_fts5_query("hello world"), "\"hello world\"");
    // Double-up internal quotes.
    assert_eq!(escape_fts5_query(r#"a"b"#), "\"a\"\"b\"");
    // Column filter syntax must be inside the phrase, not at the top.
    assert_eq!(escape_fts5_query("body:foo"), "\"body:foo\"");
}
