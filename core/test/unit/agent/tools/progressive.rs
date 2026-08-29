use super::*;

fn entry(name: &str, server: &str, remote_name: &str, description: &str) -> CatalogEntry {
    CatalogEntry {
        descriptor: LlmTool {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
                "additionalProperties": false,
            }),
        },
        disclosure: ToolDisclosure::extension(
            "mcp",
            Some(server.to_string()),
            Some(remote_name.to_string()),
            ["mcp".to_string(), "search".to_string()],
        ),
    }
}

#[test]
fn bridge_schemas_are_fixed_across_catalog_changes() {
    assert_eq!(
        serde_json::to_value(bridge_tools()).unwrap(),
        serde_json::to_value(bridge_tools()).unwrap()
    );
    let names = bridge_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec![TOOL_SEARCH, TOOL_DESCRIBE, TOOL_CALL]);
}

#[test]
fn search_distinguishes_duplicate_remote_names_by_server() {
    let catalog = vec![
        entry(
            "mcp_alpha_lookup",
            "alpha",
            "lookup",
            "Look up an alpha record.",
        ),
        entry(
            "mcp_beta_lookup",
            "beta",
            "lookup",
            "Look up a beta record.",
        ),
    ];
    let result = search_tools(
        &catalog,
        7,
        &serde_json::json!({"query": "lookup", "server": "beta"}),
    );
    assert!(!result.is_error);
    assert!(result.content.contains("<untrusted_tool_result>"));
    assert!(result.content.contains("mcp_beta_lookup"));
    assert!(!result.content.contains("mcp_alpha_lookup"));
}

#[test]
fn describe_returns_the_exact_oversized_schema_on_demand() {
    let mut catalog = vec![entry(
        "mcp_large_query",
        "large",
        "query",
        "Large query schema.",
    )];
    catalog[0].descriptor.input_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "payload": {"type": "string", "description": "x".repeat(32_000)}
        }
    });

    let result = describe_tool(
        &catalog,
        9,
        &serde_json::json!({"name": "mcp_large_query"}),
    );
    assert!(!result.is_error);
    assert!(result.content.contains(&"x".repeat(32_000)));
    assert!(result.content.contains("\"catalog_generation\":9"));
}

#[test]
fn call_envelope_is_strict_and_cannot_recurse() {
    assert!(resolve_call_envelope(&serde_json::json!({
        "name": TOOL_SEARCH,
        "arguments": {},
    }))
    .is_err());
    assert!(resolve_call_envelope(&serde_json::json!({
        "name": "mcp_alpha_lookup",
        "arguments": "{}",
    }))
    .is_err());
    assert!(resolve_call_envelope(&serde_json::json!({
        "name": "mcp_alpha_lookup",
        "arguments": {},
        "unexpected": true,
    }))
    .is_err());
}

#[test]
fn search_rejects_oversized_queries() {
    let result = search_tools(
        &[entry(
            "mcp_alpha_lookup",
            "alpha",
            "lookup",
            "Look up an alpha record.",
        )],
        1,
        &serde_json::json!({"query": "x".repeat(MAX_QUERY_CHARS + 1)}),
    );
    assert!(result.is_error);
}
