use super::*;
use crate::agent::memory::semantic::SemanticStore;

fn tool_no_embedder() -> CosRecallSemanticTool {
    // Store without an embedder — search will return SemanticError::Disabled.
    let store = SemanticStore::open_in_memory(None).unwrap();
    CosRecallSemanticTool::new(Arc::new(store))
}

#[tokio::test]
async fn missing_command_is_tool_error() {
    let r = tool_no_embedder().exec(json!({})).await;
    assert!(r.is_error);
    assert!(r.content.contains("missing 'command'"));
}

#[tokio::test]
async fn search_without_query_errors() {
    let r = tool_no_embedder()
        .exec(json!({ "command": "search" }))
        .await;
    assert!(r.is_error);
    assert!(r.content.contains("non-empty 'query'"));
}

#[tokio::test]
async fn search_with_no_embedder_returns_disabled_error() {
    let r = tool_no_embedder()
        .exec(json!({ "command": "search", "query": "anything" }))
        .await;
    assert!(r.is_error);
    assert!(r.content.contains("disabled"), "{}", r.content);
}

#[tokio::test]
async fn count_on_empty_store_returns_zero() {
    let r = tool_no_embedder().exec(json!({ "command": "count" })).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("\"count\":0"));
}

#[test]
fn normalise_namespace_prepends_when_missing() {
    assert_eq!(normalise_namespace("abc-123"), "session/abc-123");
    assert_eq!(
        normalise_namespace("session/abc-123"),
        "session/abc-123"
    );
}

#[tokio::test]
async fn unknown_command_is_tool_error() {
    let r = tool_no_embedder()
        .exec(json!({ "command": "nope" }))
        .await;
    assert!(r.is_error);
    assert!(r.content.contains("unknown command"));
}
