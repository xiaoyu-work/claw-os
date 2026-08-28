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

async fn with_memory_caps<F, T>(verbs: &[crate::caps::Verb], future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let caps = crate::caps::CapSet::from_caps(verbs.iter().map(|verb| {
        crate::caps::Cap::new(
            *verb,
            crate::caps::Scope::self_ref(crate::agent::tools::SYSTEM_AGENT_MEMORY_SCOPE),
        )
    }));
    let session = crate::proc::SessionInfo {
        session_id: format!("memory-tool-{}", uuid::Uuid::new_v4()),
        pid: std::process::id(),
        command: vec!["test".to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: None,
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: None,
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: None,
        app_id: None,
        pending_bind: false,
        start_time_ticks: None,
        client: crate::session::SessionClient::default(),
    };
    crate::proc::with_trusted_session_override(session, future).await
}

#[tokio::test]
async fn write_then_read_roundtrips_via_tool() {
    let t = CosMemoryTool::with_store(tmp_store("rw"));
    with_memory_caps(
        &[crate::caps::Verb::MEMORY_READ, crate::caps::Verb::MEMORY_WRITE],
        async {
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
        },
    )
    .await;
}

#[tokio::test]
async fn list_returns_known_files() {
    let t = CosMemoryTool::with_store(tmp_store("list"));
    with_memory_caps(
        &[crate::caps::Verb::MEMORY_READ, crate::caps::Verb::MEMORY_WRITE],
        async {
            t.exec(json!({"command":"write","name":"MEMORY.md","content":"x"}))
                .await;
            t.exec(json!({"command":"write","name":"USER.md","content":"y"}))
                .await;
            let r = t.exec(json!({"command":"list"})).await;
            assert!(!r.is_error, "{}", r.content);
            assert!(r.content.contains("MEMORY.md"), "content: {}", r.content);
            assert!(r.content.contains("USER.md"), "content: {}", r.content);
        },
    )
    .await;
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
    with_memory_caps(
        &[crate::caps::Verb::MEMORY_READ, crate::caps::Verb::MEMORY_WRITE],
        async {
            t.exec(json!({"command":"append","name":"MEMORY.md","content":"line1"}))
                .await;
            t.exec(json!({"command":"append","name":"MEMORY.md","content":"line2"}))
                .await;
            let r = t.exec(json!({"command":"read","name":"MEMORY.md"})).await;
            assert!(r.content.contains("line1") && r.content.contains("line2"));
        },
    )
    .await;
}

#[tokio::test]
async fn missing_command_is_tool_error() {
    let t = CosMemoryTool::with_store(tmp_store("no-cmd"));
    let r = t.exec(json!({})).await;
    assert!(r.is_error);
    assert!(r.content.contains("missing 'command'"));
}

#[tokio::test]
async fn read_only_and_write_only_sessions_enforce_each_command() {
    let _lock = crate::test_env::lock_env();
    let caps_dir = tempfile::tempdir().unwrap();
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    let _caps =
        crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", caps_dir.path());
    let store = tmp_store("split-caps");
    store.write("MEMORY.md", "existing").unwrap();
    let tool = CosMemoryTool::with_store(store);

    with_memory_caps(&[crate::caps::Verb::MEMORY_READ], async {
        let read = tool.exec(json!({"command":"read","name":"MEMORY.md"})).await;
        assert!(!read.is_error, "{}", read.content);
        let write = tool
            .exec(json!({"command":"write","name":"MEMORY.md","content":"changed"}))
            .await;
        assert!(write.is_error);
        assert!(write.content.contains("memory.write"));
    })
    .await;

    with_memory_caps(&[crate::caps::Verb::MEMORY_WRITE], async {
        let write = tool
            .exec(json!({"command":"append","name":"MEMORY.md","content":"changed"}))
            .await;
        assert!(!write.is_error, "{}", write.content);
        let read = tool.exec(json!({"command":"read","name":"MEMORY.md"})).await;
        assert!(read.is_error);
        assert!(read.content.contains("memory.read"));
    })
    .await;
}
