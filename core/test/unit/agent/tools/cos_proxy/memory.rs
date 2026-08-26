use super::*;
use std::path::PathBuf;

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
