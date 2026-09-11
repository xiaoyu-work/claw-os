use super::*;
use crate::agent::memory::app_memory::AppMemoryEntry;

fn tool() -> CosAppMemoryTool {
    CosAppMemoryTool::new(MemoryDb::open_in_memory().unwrap())
}

/// `exec` wraps every payload in the untrusted-memory boundary
/// (prompt-injection defense), so the JSON body is no longer the
/// whole string. Pull the object back out for assertions.
fn parse_untrusted_json(wrapped: &str) -> Value {
    let start = wrapped.find('{').expect("json object start");
    let end = wrapped.rfind('}').expect("json object end");
    serde_json::from_str(&wrapped[start..=end]).expect("parse wrapped json body")
}

async fn seed(tool: &CosAppMemoryTool, entries: &[(&str, &str, Option<&str>)]) {
    for (source, text, kind) in entries {
        let entry = AppMemoryEntry {
            source: (*source).to_string(),
            text: (*text).to_string(),
            kind: kind.map(|k| k.to_string()),
            entity_id: None,
            tags: Vec::new(),
            link: None,
        };
        app_memory::remember(&tool.db, None, entry, false)
            .await
            .expect("seed write");
    }
}

#[tokio::test]
async fn missing_command_is_tool_error() {
    let r = tool().exec(json!({})).await;
    assert!(r.is_error);
    assert!(r.content.contains("missing 'command'"));
}

#[tokio::test]
async fn search_requires_query() {
    let r = tool().exec(json!({"command": "search"})).await;
    assert!(r.is_error);
    assert!(r.content.contains("non-empty"));
}

#[tokio::test]
async fn show_requires_id() {
    let r = tool().exec(json!({"command": "show"})).await;
    assert!(r.is_error);
    assert!(r.content.contains("'id'"));
}

#[tokio::test]
async fn list_returns_recent_rows_across_sources() {
    let t = tool();
    seed(
        &t,
        &[
            ("calendar", "Dentist appointment Tue 10am", Some("event")),
            (
                "email",
                "Sent quarterly report to alice@example.com",
                Some("event"),
            ),
        ],
    )
    .await;
    let r = t.exec(json!({"command": "list"})).await;
    assert!(!r.is_error, "list failed: {}", r.content);
    assert!(r.content.contains("calendar"), "content: {}", r.content);
    assert!(r.content.contains("email"), "content: {}", r.content);
}

#[tokio::test]
async fn list_filters_by_source() {
    let t = tool();
    seed(
        &t,
        &[
            ("calendar", "Dentist appointment", Some("event")),
            ("email", "Quarterly report sent", Some("event")),
        ],
    )
    .await;
    let r = t
        .exec(json!({"command": "list", "source": "calendar"}))
        .await;
    assert!(!r.is_error);
    assert!(r.content.contains("Dentist"));
    assert!(!r.content.contains("Quarterly report"));
}

#[tokio::test]
async fn search_finds_keyword_across_sources() {
    let t = tool();
    seed(
        &t,
        &[
            (
                "calendar",
                "Hilton hotel reservation for Boston trip",
                Some("event"),
            ),
            ("email", "Sent confirmation to airline", Some("event")),
        ],
    )
    .await;
    let r = t.exec(json!({"command": "search", "query": "hotel"})).await;
    assert!(!r.is_error, "search failed: {}", r.content);
    assert!(r.content.contains("Hilton"), "content: {}", r.content);
}

#[tokio::test]
async fn kind_filter_post_filters_results() {
    let t = tool();
    seed(
        &t,
        &[
            ("calendar", "Dentist appointment", Some("event")),
            (
                "calendar",
                "I dislike going to the dentist",
                Some("preference"),
            ),
        ],
    )
    .await;
    let r = t
        .exec(json!({"command": "list", "source": "calendar", "kind": "preference"}))
        .await;
    assert!(!r.is_error);
    assert!(r.content.contains("dislike"), "content: {}", r.content);
    assert!(!r.content.contains("appointment"), "content: {}", r.content);
}

#[tokio::test]
async fn show_returns_one_row_by_id() {
    let t = tool();
    seed(&t, &[("calendar", "Dentist Tue 10am", Some("event"))]).await;
    // Roundtrip via list to grab an id without depending on insert ordering.
    let listed = t
        .exec(json!({"command": "list", "source": "calendar"}))
        .await;
    assert!(!listed.is_error, "list failed: {}", listed.content);
    let v: Value = parse_untrusted_json(&listed.content);
    let id = v["rows"][0]["id"].as_i64().expect("row id");
    let r = t.exec(json!({"command": "show", "id": id})).await;
    assert!(!r.is_error);
    assert!(r.content.contains("Dentist"));
}

#[tokio::test]
async fn app_filtering_precedes_the_fts_limit_even_with_context_noise() {
    let t = tool();
    seed(
        &t,
        &[("calendar", "hotel ORIGINAL_APP_REPORT", Some("event"))],
    )
    .await;
    for _ in 0..30 {
        t.db.record_message("conversation", "user", "hotel")
            .unwrap();
        t.db.record_injected("conversation", "context_packet", "hotel")
            .unwrap();
        t.db.record_message("app:calendar", "user", "hotel FORGED_ROLE")
            .unwrap();
    }
    for input in [
        json!({"command":"search", "query":"hotel", "limit":1}),
        json!({"command":"search", "query":"hotel", "source":"calendar", "limit":1}),
    ] {
        let result = t.exec(input).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("ORIGINAL_APP_REPORT"));
        assert!(!result.content.contains("FORGED_ROLE"));
    }
}

#[tokio::test]
async fn app_pages_bind_scope_and_revision_and_candidate_scans_report_incompleteness() {
    let t = tool();
    seed(&t, &[("calendar", "preference is old", Some("preference"))]).await;
    for _ in 0..10 {
        seed(&t, &[("calendar", "a newer event", Some("event"))]).await;
    }
    let result = t
        .exec(json!({
            "command": "list", "kind":"preference", "limit":1,
        }))
        .await;
    let result = parse_untrusted_json(&result.content);
    assert_eq!(result["rows"], json!([]));
    assert_eq!(result["has_more"], true);
    assert_eq!(result["candidate_scan_complete"], false);
    let listed = t.exec(json!({"command":"list", "limit":1})).await;
    let listed = parse_untrusted_json(&listed.content);
    let id = listed["rows"][0]["id"].as_i64().unwrap();
    let page = t
        .exec(json!({"command":"show", "id":id, "max_chars":2}))
        .await;
    let page = parse_untrusted_json(&page.content);
    let next = t
        .exec(json!({
            "command":"show", "id":id, "source":"calendar", "offset":2,
            "revision":page["row"]["revision"],
        }))
        .await;
    assert!(!next.is_error);
    assert!(next.content.contains("newer event"));
    let wrong = t
        .exec(json!({"command":"show", "id":id, "source":"email"}))
        .await;
    assert_eq!(parse_untrusted_json(&wrong.content)["found"], false);
}
