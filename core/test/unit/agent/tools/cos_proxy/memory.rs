use super::*;
use std::path::PathBuf;

fn memory_json(result: &ToolResult) -> Value {
    let start = result.content.find('{').unwrap();
    let end = result.content.rfind('}').unwrap();
    serde_json::from_str(&result.content[start..=end]).unwrap()
}

#[tokio::test]
async fn note_reads_are_bounded_utf8_pages_and_untrusted_data() {
    let directory = tempfile::tempdir().unwrap();
    let store = NotesStore::at(directory.path());
    store.write("MEMORY.md", "你好世界!").unwrap();
    let tool = CosMemoryTool::with_store(store);
    let first = tool
        .exec(json!({
            "command": "read", "offset": 0, "max_chars": 2,
        }))
        .await;
    assert!(!first.is_error, "{}", first.content);
    assert!(first.content.starts_with("<untrusted_memory>"));
    assert!(first.content.contains("\"content\":\"你好\""));
    assert!(first.content.contains("\"next_offset\":2"));
    let second = tool
        .exec(json!({
            "command": "read", "offset": 2, "max_chars": 3,
            "revision": memory_json(&first)["revision"],
        }))
        .await;
    assert!(second.content.contains("\"content\":\"世界!\""));
    assert!(second.content.contains("\"next_offset\":null"));
    for input in [
        json!({"command": "read", "offset": -1}),
        json!({"command": "read", "offset": 100}),
        json!({"command": "read", "max_chars": 0}),
    ] {
        assert!(tool.exec(input).await.is_error);
    }
}

#[tokio::test]
async fn targeted_note_search_returns_versioned_handles_and_honest_coverage() {
    let directory = tempfile::tempdir().unwrap();
    let store = NotesStore::at(directory.path());
    store
        .write(
            "MEMORY.md",
            "deployment staging uses blue; deployment production uses green",
        )
        .unwrap();
    store
        .write("other.md", "deployment notes elsewhere")
        .unwrap();
    let tool = CosMemoryTool::with_store(store);
    let result = tool
        .exec(json!({"command": "search", "query": "deployment", "limit": 1}))
        .await;
    assert!(!result.is_error, "{}", result.content);
    let value = memory_json(&result);
    assert_eq!(value["match_kind"], "literal_text");
    assert_eq!(value["has_more"], true);
    assert_eq!(value["scan_complete"], false);
    assert_eq!(value["hits"].as_array().unwrap().len(), 1);
    let hit = &value["hits"][0];
    assert_eq!(hit["name"], "MEMORY.md");
    assert_eq!(hit["revision"].as_str().unwrap().len(), 64);
    let mut input = hit["read"].clone();
    input.as_object_mut().unwrap().remove("tool");
    let read = tool.exec(input).await;
    assert!(!read.is_error, "{}", read.content);
    assert!(read.content.contains("uses blue"));
    let absent = tool
        .exec(json!({"command": "search", "query": "absent", "name": "other.md"}))
        .await;
    let absent = memory_json(&absent);
    assert_eq!(absent["hits"], json!([]));
    assert_eq!(absent["searched_names"], json!(["other.md"]));
    assert_eq!(absent["scan_complete"], true);
}

#[tokio::test]
async fn changing_a_note_invalidates_paging_instead_of_mixing_versions() {
    let directory = tempfile::tempdir().unwrap();
    let store = NotesStore::at(directory.path());
    store
        .write("MEMORY.md", "old content in two pages")
        .unwrap();
    let tool = CosMemoryTool::with_store(store.clone());
    let first = tool.exec(json!({"command": "read", "max_chars": 3})).await;
    let revision = memory_json(&first)["revision"].clone();
    store
        .write("MEMORY.md", "new content in two pages")
        .unwrap();
    let stale = tool
        .exec(json!({
            "command": "read", "offset": 3, "revision": revision,
        }))
        .await;
    assert!(stale.is_error);
    assert!(stale.content.contains("source changed"));
    let missing_revision = tool.exec(json!({"command": "read", "offset": 3})).await;
    assert!(missing_revision.is_error);
    let fresh = tool.exec(json!({"command": "read"})).await;
    assert!(!fresh.is_error);
    assert!(fresh.content.contains("new content"));
}

#[tokio::test]
async fn malformed_writes_cannot_erase_notes_and_missing_reads_are_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let store = NotesStore::at(directory.path());
    store.write("MEMORY.md", "keep this").unwrap();
    let tool = CosMemoryTool::with_store(store.clone());
    for input in [
        json!({"command": "write"}),
        json!({"command": "write", "content": null}),
        json!({"command": "append", "content": 5}),
        json!({"command": "read", "name": 5}),
        json!({"command": "search", "name": null, "query": "keep"}),
    ] {
        assert!(tool.exec(input).await.is_error);
    }
    assert_eq!(
        store.read("MEMORY.md").unwrap().as_deref(),
        Some("keep this")
    );
    assert!(
        tool.exec(json!({"command": "read", "name": "missing.md"}))
            .await
            .is_error
    );
}

fn tmp_store(label: &str) -> NotesStore {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "cos-tool-mem-{}-{}-{}",
        label,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    NotesStore::at(dir)
}

#[tokio::test]
async fn write_then_read_roundtrips_via_tool() {
    let t = CosMemoryTool::with_store(tmp_store("rw"));
    let r = t
        .exec(json!({
            "command": "write",
            "name": "MEMORY.md",
            "content": "tool wrote this",
        }))
        .await;
    assert!(!r.is_error, "write failed: {}", r.content);

    let r = t
        .exec(json!({ "command": "read", "name": "MEMORY.md" }))
        .await;
    assert!(!r.is_error, "read failed: {}", r.content);
    assert!(
        r.content.contains("tool wrote this"),
        "content was: {}",
        r.content
    );
}

#[tokio::test]
async fn list_returns_known_files() {
    let t = CosMemoryTool::with_store(tmp_store("list"));
    t.exec(json!({"command":"write","name":"MEMORY.md","content":"x"}))
        .await;
    t.exec(json!({"command":"write","name":"USER.md","content":"y"}))
        .await;
    let r = t.exec(json!({"command":"list"})).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("MEMORY.md"), "content: {}", r.content);
    assert!(r.content.contains("USER.md"), "content: {}", r.content);
}

#[tokio::test]
async fn invalid_name_is_returned_as_tool_error() {
    let t = CosMemoryTool::with_store(tmp_store("bad-name"));
    let r = t
        .exec(json!({"command":"write","name":"../escape.md","content":"x"}))
        .await;
    assert!(r.is_error);
}

#[tokio::test]
async fn append_extends_existing_note() {
    let t = CosMemoryTool::with_store(tmp_store("append"));
    t.exec(json!({"command":"append","name":"MEMORY.md","content":"line1"}))
        .await;
    t.exec(json!({"command":"append","name":"MEMORY.md","content":"line2"}))
        .await;
    let r = t.exec(json!({"command":"read","name":"MEMORY.md"})).await;
    assert!(r.content.contains("line1") && r.content.contains("line2"));
}

#[tokio::test]
async fn missing_command_is_tool_error() {
    let t = CosMemoryTool::with_store(tmp_store("no-cmd"));
    let r = t.exec(json!({})).await;
    assert!(r.is_error);
    assert!(r.content.contains("missing 'command'"));
}
