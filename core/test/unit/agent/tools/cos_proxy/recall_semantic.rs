use super::*;
use crate::agent::memory::semantic::SemanticStore;

fn tool_no_embedder() -> CosRecallSemanticTool {
    // Store without an embedder — search will return SemanticError::Disabled.
    let store = SemanticStore::open_in_memory(None).unwrap();
    CosRecallSemanticTool::new(Arc::new(store))
}

fn memory_read_session(scope: crate::caps::Scope) -> crate::proc::SessionInfo {
    crate::proc::SessionInfo {
        session_id: "semantic-session".to_string(),
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
        caps: Some(crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::MEMORY_READ,
            scope,
        )])),
        transient_caps: None,
        role: None,
        app_id: None,
        pending_bind: false,
        start_time_ticks: None,
        client: crate::session::SessionClient::new(
            crate::session::SessionSource::BrokerTask,
            false,
            true,
        ),
    }
}

async fn exec(tool: &CosRecallSemanticTool, input: Value) -> ToolResult {
    crate::proc::with_trusted_session_override(
        memory_read_session(crate::caps::Scope::Wild),
        tool.exec(input),
    )
    .await
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
    let tool = tool_no_embedder();
    let r = exec(&tool, json!({ "command": "search", "query": "anything" })).await;
    assert!(r.is_error);
    assert!(r.content.contains("disabled"), "{}", r.content);
}

#[tokio::test]
async fn count_on_empty_store_returns_zero() {
    let tool = tool_no_embedder();
    let r = exec(&tool, json!({ "command": "count" })).await;
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

#[tokio::test]
async fn session_scoped_grant_cannot_count_another_or_all_namespaces() {
    let _lock = crate::test_env::lock_env();
    let caps_dir = tempfile::tempdir().unwrap();
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    let _caps =
        crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", caps_dir.path());
    let mut registry = crate::agent::tools::registry::ToolRegistry::new();
    registry.register(Arc::new(tool_no_embedder()));
    let session = memory_read_session(crate::caps::Scope::self_ref("alpha"));
    let context = crate::agent::tools::exposure::ToolExposureContext::from_trusted_session(
        &session,
        Some("alpha"),
        None,
        1000,
        crate::agent::tools::exposure::ExecutionHost::AgentWorker,
        crate::agent::tools::guardrails::Guardrails::permissive(),
    );

    assert!(registry
        .get_for(&context, "cos_recall_semantic")
        .is_some());
    let (own, other, global) = crate::proc::with_trusted_session_override(session, async {
        let own = registry
            .execute(
                &context,
                "cos_recall_semantic",
                json!({"command": "count", "session_id": "alpha"}),
                "test",
            )
            .await;
        let other = registry
            .execute(
                &context,
                "cos_recall_semantic",
                json!({"command": "count", "session_id": "bravo"}),
                "test",
            )
            .await;
        let global = registry
            .execute(
                &context,
                "cos_recall_semantic",
                json!({"command": "count"}),
                "test",
            )
            .await;
        (own, other, global)
    })
    .await;
    assert!(!own.is_error, "{}", own.content);
    assert!(other.is_error);
    assert!(global.is_error);
}
