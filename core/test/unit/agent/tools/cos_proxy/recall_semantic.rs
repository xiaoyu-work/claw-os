use super::*;
use crate::agent::memory::semantic::SemanticStore;

fn semantic_json(result: &ToolResult) -> Value {
    let start = result.content.find('{').unwrap();
    let end = result.content.rfind('}').unwrap();
    serde_json::from_str(&result.content[start..=end]).unwrap()
}

#[tokio::test]
async fn semantic_search_exposes_bounded_versioned_reads_and_original_sources() {
    let store =
        Arc::new(SemanticStore::open_in_memory(Some(Arc::new(claw_embed::StubEmbedder))).unwrap());
    let text = format!("{}TAIL_SENTINEL", "semantic source ".repeat(100));
    store.index("session/one", "user-7", &text).await.unwrap();
    let tool = CosRecallSemanticTool::new(store.clone());
    let searched = tool
        .exec(json!({
            "command": "search", "query": text, "namespace": "session/one", "limit": 1,
        }))
        .await;
    assert!(!searched.is_error, "{}", searched.content);
    let value = semantic_json(&searched);
    let hit = &value["hits"][0];
    assert!(!hit["text"].as_str().unwrap().contains("TAIL_SENTINEL"));
    assert_eq!(hit["next_offset"], 512);
    assert_eq!(hit["source_complete"], false);
    assert!(hit.get("indexed_at_ms").is_some());
    assert_eq!(hit["original_source"]["message_id"], 7);
    assert_eq!(value["score_kind"], "cosine_similarity_not_confidence");
    let mut read = hit["read"].clone();
    read.as_object_mut().unwrap().remove("tool");
    let read_result = tool.exec(read.clone()).await;
    assert!(!read_result.is_error, "{}", read_result.content);
    assert!(read_result.content.contains("TAIL_SENTINEL"));
    store
        .index("session/one", "user-7", "changed source")
        .await
        .unwrap();
    let stale = tool.exec(read).await;
    assert!(stale.is_error);
    assert!(stale.content.contains("source changed"));
}

#[tokio::test]
async fn namespace_filters_and_missing_sources_are_explicit() {
    let store =
        Arc::new(SemanticStore::open_in_memory(Some(Arc::new(claw_embed::StubEmbedder))).unwrap());
    store
        .index("app/calendar", "12", "calendar event")
        .await
        .unwrap();
    store
        .index("session/one", "user-7", "conversation event")
        .await
        .unwrap();
    let tool = CosRecallSemanticTool::new(store);
    let result = tool
        .exec(json!({
            "command": "search", "query": "event", "namespace": "app/calendar",
        }))
        .await;
    let value = semantic_json(&result);
    assert_eq!(value["hits"].as_array().unwrap().len(), 1);
    assert_eq!(value["hits"][0]["original_source"]["source"], "calendar");
    assert!(
        tool.exec(json!({"command":"count", "namespace":"x", "session_id":"y"}))
            .await
            .is_error
    );
    assert!(
        tool.exec(json!({"command":"count", "session_id":null}))
            .await
            .is_error
    );
    assert!(
        tool.exec(json!({"command":"read", "namespace":"app/calendar", "key":"missing"}))
            .await
            .is_error
    );
}

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
    assert_eq!(normalise_namespace("session/abc-123"), "session/abc-123");
}

#[tokio::test]
async fn unknown_command_is_tool_error() {
    let r = tool_no_embedder().exec(json!({ "command": "nope" })).await;
    assert!(r.is_error);
    assert!(r.content.contains("unknown command"));
}
