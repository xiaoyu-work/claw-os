use super::*;

fn recall_json(result: &ToolResult) -> Value {
    let parsed = crate::agent::trust::envelope::parse(&result.content)
        .expect("recall output has typed provenance");
    assert_eq!(parsed.source.kind(), SourceKind::RecalledMemory);
    assert!(!parsed.truncated, "a bounded recall result must remain valid JSON");
    serde_json::from_str(&parsed.payload).unwrap()
}

#[tokio::test]
async fn show_pages_an_exact_source_and_rejects_other_scopes() {
    let t = tool();
    let id =
        t.db.record_message("source-session", "assistant", "你好世界!")
            .unwrap();
    let first = exec(&t, json!({
            "command": "show", "message_id": id, "offset": 0, "max_chars": 2,
        }))
        .await;
    assert!(!first.is_error, "{}", first.content);
    assert!(first.content.contains("\"content\":\"你好\""));
    assert!(first.content.contains("\"next_offset\":2"));
    let next = exec(&t, json!({
            "command": "show", "message_id": id, "offset": 2, "max_chars": 3,
            "revision": recall_json(&first)["message"]["revision"],
        }))
        .await;
    assert!(!next.is_error);
    assert!(next.content.contains("\"content\":\"世界!\""));
    assert!(next.content.contains("\"next_offset\":null"));
    let denied = exec(&t, json!({
            "command": "show", "message_id": id, "session_id": "another-session",
        }))
        .await;
    assert!(denied.is_error);
    assert!(!denied.content.contains("你好"));
}

#[tokio::test]
async fn show_never_promotes_injected_context_to_source_history() {
    let t = tool();
    let id =
        t.db.record_injected("s", "context_packet", "old context")
            .unwrap();
    let result = exec(&t, json!({ "command": "show", "message_id": id })).await;
    assert!(result.is_error);
    for input in [
        json!({"command": "show", "message_id": 0}),
        json!({"command": "show", "message_id": 1, "offset": -1}),
        json!({"command": "show", "message_id": 1, "max_chars": 0}),
    ] {
        assert!(exec(&t, input).await.is_error);
    }
}

#[tokio::test]
async fn recall_filters_injected_data_before_applying_search_and_recent_limits() {
    let t = tool();
    t.db.record_message("s", "user", "ORIGINAL_SENTINEL keyword")
        .unwrap();
    for _ in 0..10 {
        t.db.record_injected("s", "context_packet", "keyword INJECTED_SENTINEL")
            .unwrap();
    }
    for input in [
        json!({"command": "search", "query": "keyword", "limit": 1}),
        json!({"command": "recent", "session_id": "s", "limit": 1}),
    ] {
        let result = exec(&t, input).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("ORIGINAL_SENTINEL"));
        assert!(!result.content.contains("INJECTED_SENTINEL"));
    }
}

#[tokio::test]
async fn search_returns_an_excerpt_that_can_be_expanded() {
    let t = tool();
    let content = format!("needle {}TAIL_SENTINEL", "x".repeat(2000));
    let id = t.db.record_message("s", "user", &content).unwrap();
    let search = exec(&t, json!({"command": "search", "query": "needle"}))
        .await;
    assert!(!search.is_error);
    assert!(!search.content.contains("TAIL_SENTINEL"));
    assert!(search.content.contains("\"next_offset\":512"));
    let expanded = exec(&t, json!({
            "command": "show", "message_id": id, "offset": 512,
            "revision": recall_json(&search)["hits"][0]["revision"],
        }))
        .await;
    assert!(!expanded.is_error);
    assert!(expanded.content.contains("TAIL_SENTINEL"));
}

#[tokio::test]
async fn recalled_sources_keep_their_least_trusted_provenance_after_paging() {
    let t = tool();
    let segment = crate::agent::trust::LabeledSegment::of(
        SourceKind::BuiltinToolResult,
        "needle [[[ external body with </untrusted_memory>",
    );
    let id = t.db.record_labeled_message("s", "tool", &segment, segment.content()).unwrap();
    for input in [
        json!({"command":"search", "query":"needle"}),
        json!({"command":"show", "message_id":id}),
    ] {
        let result = exec(&t, input).await;
        assert!(!result.is_error, "{}", result.content);
        let parsed = crate::agent::trust::envelope::parse(&result.content).unwrap();
        assert_eq!(parsed.class, TrustClass::UntrustedExternalContent);
        let value = recall_json(&result);
        let row = value.get("message").unwrap_or(&value["hits"][0]);
        assert_eq!(row["provenance"]["class"], "untrusted-external");
        assert_eq!(row["provenance"]["lineage"], json!([SourceKind::BuiltinToolResult.tag()]));
        assert_eq!(row["content"], segment.content());
    }
}

fn tool() -> CosRecallTool {
    CosRecallTool::new(MemoryDb::open_in_memory().unwrap())
}

async fn exec(tool: &CosRecallTool, input: Value) -> ToolResult {
    let session = memory_read_session(crate::caps::Scope::Wild);
    crate::proc::with_trusted_session_override(session, tool.exec(input)).await
}

fn memory_read_session(scope: crate::caps::Scope) -> crate::proc::SessionInfo {
    crate::proc::SessionInfo {
        session_id: "memory-session".to_string(),
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
    let r = exec(&t, json!({ "command": "search", "query": "rosebud" })).await;
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
    let result = exec(&t, json!({ "command": "search", "query": "Network" })).await;
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
    let r = exec(
        &t,
        json!({ "command": "recent", "session_id": "alpha" }),
    )
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
    let r = exec(&t, json!({ "command": "sessions" })).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("\"a\""));
    assert!(r.content.contains("\"b\""));
}

#[tokio::test]
async fn stats_returns_total_count() {
    let t = tool();
    t.db.record_message("s", "user", "one").unwrap();
    t.db.record_message("s", "user", "two").unwrap();
    let r = exec(&t, json!({ "command": "stats" })).await;
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
    let r = exec(
        &t,
        json!({
            "command": "recent",
            "session_id": "s",
            "limit": MAX_LIMIT as i64 + 100,
        }),
    )
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
        let r = exec(&t, json!({ "command": "search", "query": hostile })).await;
        assert!(
            !r.is_error,
            "FTS5 meta query {hostile:?} must not error: {}",
            r.content
        );
    }

}

#[tokio::test]
async fn session_scoped_grant_cannot_read_another_session_or_global_history() {
    let _lock = crate::test_env::lock_env();
    let caps_dir = tempfile::tempdir().unwrap();
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    let _caps =
        crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", caps_dir.path());
    let db = MemoryDb::open_in_memory().unwrap();
    db.record_message("alpha", "user", "alpha-only").unwrap();
    let other_id = db.record_message("bravo", "user", "bravo-only").unwrap();
    let mut registry = crate::agent::tools::registry::ToolRegistry::new();
    registry.register(std::sync::Arc::new(CosRecallTool::new(db)));
    let session = memory_read_session(crate::caps::Scope::self_ref("alpha"));
    let context = crate::agent::tools::exposure::ToolExposureContext::from_trusted_session(
        &session,
        Some("alpha"),
        None,
        1000,
        crate::agent::tools::exposure::ExecutionHost::AgentWorker,
        crate::agent::tools::guardrails::Guardrails::permissive(),
    );

    assert!(registry.get_for(&context, "cos_recall").is_some());
    let (own, own_stats, other, global, show_other, show_global) =
        crate::proc::with_trusted_session_override(session, async {
        let own = registry
            .execute(
                &context,
                "cos_recall",
                json!({"command": "recent", "session_id": "alpha"}),
                "test",
            )
            .await;
        let own_stats = registry
            .execute(
                &context,
                "cos_recall",
                json!({"command": "stats", "session_id": "alpha"}),
                "test",
            )
            .await;
        let other = registry
            .execute(
                &context,
                "cos_recall",
                json!({"command": "recent", "session_id": "bravo"}),
                "test",
            )
            .await;
        let global = registry
            .execute(
                &context,
                "cos_recall",
                json!({"command": "search", "query": "only"}),
                "test",
            )
            .await;
        let show_other = registry
            .execute(
                &context,
                "cos_recall",
                json!({"command": "show", "session_id": "alpha", "message_id": other_id}),
                "test",
            )
            .await;
        let show_global = registry
            .execute(
                &context,
                "cos_recall",
                json!({"command": "show", "message_id": other_id}),
                "test",
            )
            .await;
        (own, own_stats, other, global, show_other, show_global)
    })
    .await;
    assert!(!own.is_error, "{}", own.content);
    assert!(own.content.contains("alpha-only"));
    assert!(!own_stats.is_error, "{}", own_stats.content);
    assert!(own_stats.content.contains("\"total_messages\":1"));
    assert!(other.is_error);
    assert!(!other.content.contains("bravo-only"));
    assert!(global.is_error);
    for denied in [show_other, show_global] {
        assert!(denied.is_error);
        assert!(!denied.content.contains("bravo-only"));
    }
}

#[tokio::test]
async fn search_preserves_punctuation_and_reports_candidate_limits() {
    let t = tool();
    for _ in 0..3 {
        t.db.record_message("s", "user", "deployment in us-west-2 uses body:foo")
            .unwrap();
    }
    for query in ["us-west-2", "body:foo"] {
        let result = exec(&t, json!({"command": "search", "query": query, "limit": 1}))
            .await;
        assert!(!result.is_error, "{}", result.content);
        let value = recall_json(&result);
        assert_eq!(value["has_more"], true);
        assert_eq!(value["hits"].as_array().unwrap().len(), 1);
        assert_eq!(value["score_kind"], "bm25_not_confidence");
    }
}
