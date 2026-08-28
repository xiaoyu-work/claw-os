use super::*;

fn tool(name: &str, description: &str, required: &[&str]) -> LlmTool {
    LlmTool {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
            },
            "required": required,
            "additionalProperties": false,
        }),
    }
}

#[test]
fn partition_keeps_core_and_defers_extensible_tools() {
    let (visible, deferred) = partition_tools(vec![
        tool("cos_sysinfo", "system telemetry", &[]),
        tool(
            "mcp_linear_create_issue",
            "create a Linear issue",
            &["query"],
        ),
        tool("cos_app_mail", "mail application", &["query"]),
    ]);
    assert_eq!(
        visible
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["cos_sysinfo"]
    );
    assert_eq!(
        deferred
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["mcp_linear_create_issue", "cos_app_mail"]
    );
}

#[test]
fn bridge_surface_is_fixed_and_lists_deferred_names() {
    let bridges = bridge_tools(&[tool(
        "mcp_linear_create_issue",
        "Create a Linear issue.",
        &["query"],
    )]);
    assert_eq!(bridges.len(), 3);
    assert_eq!(bridges[0].name, TOOL_SEARCH);
    assert_eq!(bridges[1].name, TOOL_DESCRIBE);
    assert_eq!(bridges[2].name, TOOL_CALL);
    assert!(bridges[0].description.contains("mcp_linear_create_issue"));
}

#[test]
fn search_bridge_description_stays_within_provider_limit() {
    let deferred = (0..100)
        .map(|index| {
            let name = format!("mcp_service_tool_{index:03}");
            tool(
                &name,
                "A deliberately verbose deferred capability description that would overflow the provider limit.",
                &[],
            )
        })
        .collect::<Vec<_>>();
    let bridges = bridge_tools(&deferred);
    assert!(bridges[0].description.chars().count() <= 1024);
}

#[test]
fn search_returns_matching_tool_and_required_fields() {
    let result = search_tools(
        &[
            tool(
                "mcp_linear_create_issue",
                "Create a Linear issue.",
                &["query"],
            ),
            tool("mcp_slack_send", "Send a Slack message.", &["query"]),
        ],
        &serde_json::json!({"queries": ["linear issue"]}),
    );
    assert!(!result.is_error);
    let value: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(
        value["results"][0]["matches"][0]["name"],
        "mcp_linear_create_issue"
    );
    assert_eq!(value["results"][0]["matches"][0]["required"][0], "query");
}

#[test]
fn call_envelope_accepts_object_or_json_string() {
    let (_, object) = resolve_call_envelope(&serde_json::json!({
        "name": "mcp_linear_create_issue",
        "arguments": {"query": "bug"},
    }))
    .unwrap();
    let (_, string) = resolve_call_envelope(&serde_json::json!({
        "name": "mcp_linear_create_issue",
        "arguments": "{\"query\":\"bug\"}",
    }))
    .unwrap();
    assert_eq!(object, string);
}

#[test]
fn required_validation_returns_schema_guidance() {
    let tool = tool(
        "mcp_linear_create_issue",
        "Create a Linear issue.",
        &["query"],
    );
    let error = validate_required(&tool, &serde_json::json!({})).unwrap_err();
    assert!(error.contains("query"));
    assert!(error.contains("Expected schema"));
}
