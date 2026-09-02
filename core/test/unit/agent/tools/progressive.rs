use super::*;
use std::sync::Arc;

fn entry(name: &str, server: &str, remote_name: &str, description: &str) -> CatalogEntry {
    CatalogEntry {
        descriptor: Arc::new(LlmTool {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
                "additionalProperties": false,
            }),
        }),
        disclosure: Arc::new(ToolDisclosure::extension(
            "mcp",
            Some(server.to_string()),
            Some(remote_name.to_string()),
            ["mcp".to_string(), "search".to_string()],
        )),
    }
}

fn result_json(result: &ToolResult) -> Value {
    let parsed = crate::agent::trust::envelope::parse(&result.content)
        .expect("progressive result is trust-labelled");
    assert_eq!(
        parsed.source.kind(),
        crate::agent::trust::SourceKind::McpToolMetadata
    );
    serde_json::from_str(&parsed.payload).expect("valid result JSON")
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
    assert!(crate::agent::trust::envelope::looks_enveloped(
        &result.content
    ));
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
    Arc::make_mut(&mut catalog[0].descriptor).input_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "payload": {"type": "string", "description": "x".repeat(32_000)}
        }
    });

    let result = describe_tool(&catalog, 9, &serde_json::json!({"name": "mcp_large_query"}));
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

#[test]
fn search_bounds_sixteen_mib_required_metadata() {
    let huge_required = "x".repeat(16 * 1024 * 1024);
    let catalog = vec![CatalogEntry {
        descriptor: Arc::new(LlmTool {
            name: "mcp_alpha_huge_required".to_string(),
            description: "Adversarial schema metadata.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": [huge_required],
            }),
        }),
        disclosure: Arc::new(ToolDisclosure::extension(
            "mcp",
            Some("alpha".to_string()),
            Some("huge_required".to_string()),
            ["mcp".to_string()],
        )),
    }];

    let result = search_tools(&catalog, 11, &serde_json::json!({"query": "*"}));
    assert!(!result.is_error);
    assert!(result.content.len() <= MAX_SEARCH_RESPONSE_BYTES);
    assert!(!result.content.contains("input_schema"));
    let value = result_json(&result);
    let metadata = &value["matches"][0];
    assert_eq!(metadata["required_total"], 1);
    assert_eq!(metadata["required_value_truncated_count"], 1);
    assert_eq!(metadata["metadata_truncated"], true);
    assert!(metadata["required"][0].as_str().unwrap().chars().count() <= MAX_REQUIRED_FIELD_CHARS);
}

#[test]
fn search_bounds_utf8_tool_and_server_names_deterministically() {
    let huge_name = format!("mcp_{}", "工具".repeat(100_000));
    let huge_server = "服务器".repeat(100_000);
    let catalog = vec![entry(
        &huge_name,
        &huge_server,
        &"远程工具".repeat(100_000),
        "Unicode metadata.",
    )];

    let first = search_tools(&catalog, 12, &serde_json::json!({"query": "mcp"}));
    let second = search_tools(&catalog, 12, &serde_json::json!({"query": "mcp"}));
    assert_eq!(first.content, second.content);
    assert!(first.content.len() <= MAX_SEARCH_RESPONSE_BYTES);
    let value = result_json(&first);
    let metadata = &value["matches"][0];
    assert_eq!(metadata["name_truncated"], true);
    assert_eq!(metadata["server_truncated"], true);
    assert_eq!(metadata["remote_name_truncated"], true);
    assert!(metadata["name"].as_str().unwrap().chars().count() <= MAX_RESULT_TOOL_NAME_CHARS);
    assert!(metadata["server"].as_str().unwrap().chars().count() <= MAX_RESULT_SERVER_CHARS);
    assert!(
        metadata["remote_name"].as_str().unwrap().chars().count() <= MAX_RESULT_REMOTE_NAME_CHARS
    );
}

#[test]
fn search_caps_twenty_five_adversarial_results_and_reports_truncation() {
    let catalog = (0..25)
        .map(|index| {
            let mut item = entry(
                &format!("mcp_server_tool_{index:02}"),
                &format!("server-{index:02}"),
                &format!("tool-{index:02}"),
                &"description".repeat(100),
            );
            Arc::make_mut(&mut item.descriptor).input_schema = serde_json::json!({
                "type": "object",
                "required": (0..64)
                    .map(|field| format!("field-{index:02}-{field:02}-{}", "界".repeat(256)))
                    .collect::<Vec<_>>(),
            });
            item
        })
        .collect::<Vec<_>>();

    let result = search_tools(
        &catalog,
        13,
        &serde_json::json!({"query": "*", "limit": MAX_SEARCH_LIMIT}),
    );
    assert!(!result.is_error);
    assert!(result.content.len() <= MAX_SEARCH_RESPONSE_BYTES);
    let value = result_json(&result);
    assert_eq!(value["total_matches"], 25);
    let returned = value["returned_count"].as_u64().unwrap();
    let truncated = value["truncated_count"].as_u64().unwrap();
    assert!(returned <= MAX_SEARCH_LIMIT as u64);
    assert_eq!(returned + truncated, 25);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["truncation"]["result_limit_reached"], true);
    let first = &value["matches"][0];
    assert_eq!(first["required_total"], 64);
    assert_eq!(
        first["required"].as_array().unwrap().len(),
        MAX_REQUIRED_FIELDS
    );
    assert_eq!(
        first["required_truncated_count"],
        (64 - MAX_REQUIRED_FIELDS) as u64
    );
    assert!(first["required"]
        .as_array()
        .unwrap()
        .iter()
        .all(|field| { field.as_str().unwrap().chars().count() <= MAX_REQUIRED_FIELD_CHARS }));
    assert!(
        value["truncation"]["metadata_truncated_count"]
            .as_u64()
            .unwrap()
            > 0
    );
}
