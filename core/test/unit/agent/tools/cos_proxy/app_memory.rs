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
            ("email", "Sent quarterly report to alice@example.com", Some("event")),
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
    let r = t.exec(json!({"command": "list", "source": "calendar"})).await;
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
            ("calendar", "Hilton hotel reservation for Boston trip", Some("event")),
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
            ("calendar", "I dislike going to the dentist", Some("preference")),
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
    let listed = t.exec(json!({"command": "list", "source": "calendar"})).await;
    assert!(!listed.is_error, "list failed: {}", listed.content);
    let v: Value = parse_untrusted_json(&listed.content);
    let id = v["rows"][0]["id"].as_i64().expect("row id");
    let r = t.exec(json!({"command": "show", "id": id})).await;
    assert!(!r.is_error);
    assert!(r.content.contains("Dentist"));
}
