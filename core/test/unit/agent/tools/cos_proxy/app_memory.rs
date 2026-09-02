use super::*;
use crate::agent::memory::app_memory::AppMemoryEntry;

fn tool() -> CosAppMemoryTool {
    CosAppMemoryTool::new(MemoryDb::open_in_memory().unwrap())
}

fn memory_read_session(
    scope: crate::caps::Scope,
    app_id: Option<&str>,
) -> crate::proc::SessionInfo {
    crate::proc::SessionInfo {
        session_id: "app-memory-session".to_string(),
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
        app_id: app_id.map(str::to_string),
        pending_bind: false,
        start_time_ticks: None,
        client: crate::session::SessionClient::new(
            if app_id.is_some() {
                crate::session::SessionSource::App
            } else {
                crate::session::SessionSource::BrokerTask
            },
            false,
            true,
        ),
    }
}

async fn exec(tool: &CosAppMemoryTool, input: Value) -> ToolResult {
    crate::proc::with_trusted_session_override(
        memory_read_session(crate::caps::Scope::Wild, None),
        tool.exec(input),
    )
    .await
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
    let r = exec(&t, json!({"command": "list"})).await;
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
    let r = exec(&t, json!({"command": "list", "source": "calendar"})).await;
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
    let r = exec(&t, json!({"command": "search", "query": "hotel"})).await;
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
    let r = exec(
        &t,
        json!({"command": "list", "source": "calendar", "kind": "preference"}),
    )
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
    let listed = exec(&t, json!({"command": "list", "source": "calendar"})).await;
    assert!(!listed.is_error, "list failed: {}", listed.content);
    let v: Value = parse_untrusted_json(&listed.content);
    let id = v["rows"][0]["id"].as_i64().expect("row id");
    let r = exec(&t, json!({"command": "show", "id": id})).await;
    assert!(!r.is_error);
    assert!(r.content.contains("Dentist"));
}

#[tokio::test]
async fn app_scoped_grant_cannot_read_other_sources_or_global_rows() {
    let _lock = crate::test_env::lock_env();
    let package_root = tempfile::tempdir().unwrap();
    let package = package_root.path().join("calendar");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("app.json"),
        r#"{"id":"calendar","version":"1.0.0","name":"Calendar","operations":{}}"#,
    )
    .unwrap();
    std::fs::write(package.join("main.py"), "print('calendar')\n").unwrap();
    let launch = crate::test_env::app_launch(&package, "calendar");
    let provenance_owner = crate::provenance::runtime::current_owner();
    crate::provenance::runtime::register(provenance_owner, "app-memory-session", launch.package());
    crate::provenance::runtime::bind_process(
        provenance_owner,
        "app-memory-session",
        std::process::id(),
    );
    let caps_dir = tempfile::tempdir().unwrap();
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", caps_dir.path());
    let db = MemoryDb::open_in_memory().unwrap();
    let tool = CosAppMemoryTool::new(db.clone());
    seed(
        &tool,
        &[
            ("calendar", "calendar-only", Some("event")),
            ("email", "email-only", Some("event")),
        ],
    )
    .await;
    let email_id = app_memory::list(&db, Some("email"), 10).unwrap()[0].id;
    let mut registry = crate::agent::tools::registry::ToolRegistry::new();
    registry.register(std::sync::Arc::new(tool));
    registry.register(std::sync::Arc::new(
        crate::agent::tools::cos_proxy::recall::CosRecallTool::new(db.clone()),
    ));
    registry.register(std::sync::Arc::new(
        crate::agent::tools::cos_proxy::recall_semantic::CosRecallSemanticTool::new(
            std::sync::Arc::new(
                crate::agent::memory::semantic::SemanticStore::open_in_memory(None).unwrap(),
            ),
        ),
    ));
    let session = memory_read_session(crate::caps::Scope::self_ref("calendar"), Some("calendar"));
    let context = crate::agent::tools::exposure::ToolExposureContext::from_trusted_session(
        &session,
        None,
        None,
        1000,
        crate::agent::tools::exposure::ExecutionHost::Direct,
        crate::agent::tools::guardrails::Guardrails::permissive(),
    );

    assert!(registry.get_for(&context, "cos_app_memory").is_some());
    assert!(registry.get_for(&context, "cos_recall").is_none());
    assert!(registry.get_for(&context, "cos_recall_semantic").is_none());
    let (own, other, global, show_other) =
        crate::proc::with_trusted_session_override(session, async {
            let own = registry
                .execute(
                    &context,
                    "cos_app_memory",
                    json!({"command": "list", "source": "calendar"}),
                    "test",
                )
                .await;
            let other = registry
                .execute(
                    &context,
                    "cos_app_memory",
                    json!({"command": "list", "source": "email"}),
                    "test",
                )
                .await;
            let global = registry
                .execute(
                    &context,
                    "cos_app_memory",
                    json!({"command": "list"}),
                    "test",
                )
                .await;
            let show_other = registry
                .execute(
                    &context,
                    "cos_app_memory",
                    json!({"command": "show", "id": email_id}),
                    "test",
                )
                .await;
            (own, other, global, show_other)
        })
        .await;
    assert!(!own.is_error, "{}", own.content);
    assert!(own.content.contains("calendar-only"));
    for denied in [other, global, show_other] {
        assert!(denied.is_error);
        assert!(!denied.content.contains("email-only"));
    }
    crate::provenance::runtime::deregister(provenance_owner, "app-memory-session");
}
