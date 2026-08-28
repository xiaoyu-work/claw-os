use super::*;

fn parse(result: ToolResult) -> Value {
    assert!(!result.is_error, "unexpected discovery error: {result:?}");
    serde_json::from_str(&result.content).expect("discovery result must be JSON")
}

#[tokio::test]
async fn walks_root_to_agent_usage_without_executing_a_command() {
    let tool = CosHelp;

    let root = parse(tool.exec(json!({"path": []})).await);
    assert!(root["primitives"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"] == "agent"));

    let agent = parse(tool.exec(json!({"path": ["agent"]})).await);
    assert_eq!(agent["kind"], "namespace");
    assert!(agent["commands"].get("usage").is_some());

    let usage = parse(tool.exec(json!({"path": ["agent", "usage"]})).await);
    assert_eq!(usage["kind"], "namespace");
    assert_eq!(usage["command"], "cos agent usage");
    assert_eq!(usage["model_tool"], "cos_usage");
}

#[tokio::test]
async fn leaf_discovery_includes_the_shared_cli_schema() {
    let result = parse(
        CosHelp
            .exec(json!({"path": ["checkpoint", "rollback"]}))
            .await,
    );
    assert_eq!(result["schema_available"], true);
    assert!(result["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .any(|parameter| parameter["name"] == "checkpoint_id"));
}

#[tokio::test]
async fn rejects_flags_operands_and_hidden_routes() {
    let tool = CosHelp;
    for input in [
        json!({"path": ["--help"]}),
        json!({"path": ["__policy"]}),
        json!({"path": ["agent", "usage", "overall", "extra", "too-deep"]}),
        json!({"path": [], "command": "agent"}),
    ] {
        let result = tool.exec(input).await;
        assert!(result.is_error, "expected structural rejection: {result:?}");
    }
}

#[test]
fn accepts_namespaced_app_operation_names_without_allowing_traversal() {
    assert!(valid_segment("notes.create"));
    assert!(!valid_segment(".hidden"));
    assert!(!valid_segment("notes..create"));
}

#[tokio::test(flavor = "current_thread")]
async fn discovers_namespaced_app_operation() {
    let _lock = crate::test_env::lock_env();
    let apps = tempfile::tempdir().unwrap();
    let app = apps.path().join("notes");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("app.json"),
        r#"{
            "id":"notes",
            "version":"0.1",
            "name":"Notes",
            "operations":{
                "notes.create":{"label":"Create note"}
            }
        }"#,
    )
    .unwrap();
    let _apps_dir = crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", apps.path());

    let result = parse(
        CosHelp
            .exec(json!({"path": ["app", "notes", "notes.create"]}))
            .await,
    );
    assert_eq!(result["found"], true);
    assert_eq!(result["command"], "cos app notes notes.create");
    assert_eq!(result["model_tool"], "cos_app_run");
}

#[tokio::test]
async fn recursively_discovers_nested_agent_commands() {
    let tool = CosHelp;
    let dev = parse(tool.exec(json!({"path": ["agent", "dev"]})).await);
    assert_eq!(dev["kind"], "namespace");
    assert!(dev["subcommands"]
        .as_array()
        .unwrap()
        .contains(&json!("usage")));

    let legacy_usage = parse(tool.exec(json!({"path": ["agent", "dev", "usage"]})).await);
    assert_eq!(legacy_usage["canonical_command"], "cos agent usage");
    assert_eq!(legacy_usage["model_tool"], "cos_usage");

    let setup = parse(
        tool.exec(json!({"path": ["agent", "setup", "text", "providers"]}))
            .await,
    );
    assert_eq!(setup["found"], true);
    assert_eq!(setup["command"], "cos agent setup text providers");

    let budget = parse(tool.exec(json!({"path": ["agent", "budget"]})).await);
    assert_eq!(budget["kind"], "namespace");
    let user = parse(
        tool.exec(json!({"path": ["agent", "budget", "user"]}))
            .await,
    );
    assert_eq!(user["kind"], "namespace");
    let show = parse(
        tool.exec(json!({"path": ["agent", "budget", "user", "show"]}))
            .await,
    );
    assert_eq!(show["command"], "cos agent budget user show");

    let provider = parse(
        tool.exec(json!({"path": ["agent", "usage", "provider"]}))
            .await,
    );
    assert_eq!(provider["model_tool"], "cos_usage");

    let skill_stats = parse(
        tool.exec(json!({"path": ["agent", "skills", "usage", "stats"]}))
            .await,
    );
    assert_eq!(skill_stats["command"], "cos agent skills usage stats");
}

#[tokio::test]
async fn distinguishes_app_management_from_installed_apps() {
    let tool = CosHelp;
    let app = parse(tool.exec(json!({"path": ["app"]})).await);
    assert!(app["management"].get("install").is_some());

    let consent = parse(tool.exec(json!({"path": ["app", "consent"]})).await);
    assert_eq!(consent["kind"], "namespace");
    assert!(consent["subcommands"]
        .as_array()
        .unwrap()
        .contains(&json!("grant")));

    let grant = parse(
        tool.exec(json!({"path": ["app", "consent", "grant"]}))
            .await,
    );
    assert_eq!(grant["command"], "cos app consent grant");
    assert_eq!(grant["model_callable"], false);
}

#[tokio::test]
async fn unknown_paths_return_bounded_navigation_help() {
    let tool = CosHelp;
    let result = parse(tool.exec(json!({"path": ["agent", "not-real"]})).await);
    assert_eq!(result["found"], false);
    assert!(result["available"]
        .as_array()
        .unwrap()
        .contains(&json!("usage")));
}
