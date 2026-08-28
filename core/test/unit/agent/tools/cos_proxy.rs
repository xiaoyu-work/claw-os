use super::*;

#[test]
fn every_primitive_has_at_least_one_command() {
    for spec in PRIMITIVES {
        assert!(
            !spec.commands.is_empty(),
            "primitive {} has empty command list",
            spec.name
        );
    }
}

#[test]
fn names_are_unique_and_snake_cased() {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for spec in PRIMITIVES {
        assert!(seen.insert(spec.name), "duplicate name {}", spec.name);
        assert!(
            spec.name.starts_with("cos_"),
            "name {} should start with cos_",
            spec.name
        );
        assert!(
            spec.name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_'),
            "name {} not snake_case",
            spec.name
        );
    }
}

#[test]
fn register_all_adds_all_primitives() {
    let mut r = ToolRegistry::new();
    register_all(&mut r);
    assert_eq!(r.len(), total_count());
    assert!(r.get("cos_sandbox").is_some());
    assert!(r.get("cos_proc").is_some());
    assert!(r.get("cos_sysinfo").is_some());
    assert!(r.get("cos_usage").is_some());
    assert!(r.get("cos_memory").is_some());
    assert!(r.get("cos_oauth_login").is_some());
}

#[test]
fn schema_includes_command_enum() {
    let mut r = ToolRegistry::new();
    register_all(&mut r);
    let tool = r.get("cos_sandbox").unwrap();
    let schema = tool.input_schema();
    let enum_vals = schema
        .pointer("/properties/command/enum")
        .and_then(Value::as_array)
        .expect("enum must be present");
    assert!(enum_vals.iter().any(|v| v.as_str() == Some("exec")));
}

#[test]
fn sysinfo_schema_exposes_arbitrary_process_inspection() {
    let mut registry = ToolRegistry::new();
    register_all(&mut registry);
    let tool = registry.get("cos_sysinfo").unwrap();
    let schema = tool.input_schema();
    let commands = schema
        .pointer("/properties/command/enum")
        .and_then(Value::as_array)
        .expect("command enum must be present");
    assert!(commands.iter().any(|value| value.as_str() == Some("process")));
}

#[tokio::test]
async fn unknown_command_is_returned_as_tool_error() {
    let tool = CosPrimitiveTool::new(
        "cos_sandbox",
        "test",
        crate::sandbox::run,
        &["exec"],
    );
    let result = tool
        .exec(json!({ "command": "definitely-not-a-command", "args": [] }))
        .await;
    assert!(result.is_error, "expected is_error=true, got {result:?}");
}

#[tokio::test]
async fn missing_command_field_is_returned_as_tool_error() {
    let tool = CosPrimitiveTool::new("cos_sandbox", "test", crate::sandbox::run, &["exec"]);
    let result = tool.exec(json!({ "args": ["whatever"] })).await;
    assert!(result.is_error);
    assert!(result.content.contains("missing 'command'"));
}

#[tokio::test]
async fn args_default_to_empty() {
    // sysinfo "info" works with zero args on every platform.
    let _perms = crate::test_env::PermissiveModeGuard::new();
    let tool = CosPrimitiveTool::new("cos_sysinfo", "test", crate::sysinfo::run, &["info"]);
    let result = tool.exec(json!({ "command": "info" })).await;
    assert!(
        !result.is_error,
        "sysinfo info unexpectedly failed: {}",
        result.content
    );
}

#[tokio::test]
async fn sandbox_nonzero_exit_is_a_tool_error() {
    fn failed_command(_command: &str, _args: &[String]) -> Result<Value, String> {
        Ok(json!({
            "exit_code": 1,
            "stderr": "permission denied",
        }))
    }

    let tool = CosPrimitiveTool::new("cos_sandbox", "test", failed_command, &["exec"]);
    let result = tool.exec(json!({"command": "exec", "args": ["false"]})).await;
    assert!(result.is_error);
    assert!(result.content.contains("\"exit_code\":1"));
    assert!(result.content.contains("permission denied"));
}

#[test]
fn new_default_is_not_parallel_safe() {
    let t = CosPrimitiveTool::new("cos_x", "desc", crate::sysinfo::run, &["info"]);
    assert!(!t.parallel_safe(), "new() must default to serial");
}

#[test]
fn new_readonly_opts_into_parallel_safe() {
    let t = CosPrimitiveTool::new_readonly("cos_x", "desc", crate::sysinfo::run, &["info"]);
    assert!(t.parallel_safe(), "new_readonly() must opt into parallel");
}

#[test]
fn registered_sysinfo_is_parallel_safe() {
    let mut r = ToolRegistry::new();
    register_all(&mut r);
    assert!(
        r.is_parallel_safe("cos_sysinfo"),
        "cos_sysinfo (read-only telemetry) should opt into parallel dispatch"
    );
    assert!(
        r.is_parallel_safe("cos_usage"),
        "cos_usage (read-only aggregation) should opt into parallel dispatch"
    );
    assert!(
        !r.is_parallel_safe("cos_sandbox"),
        "cos_sandbox (arbitrary command exec) must stay serial"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn registered_usage_tool_reads_token_totals() {
    let _lock = crate::test_env::lock_env();
    let dir = tempfile::tempdir().unwrap();
    let _log_dir = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", dir.path());
    std::fs::write(
        dir.path().join("ai.jsonl"),
        format!(
            "{}\n",
            json!({
                "timestamp": "2026-08-27T12:00:00Z",
                "provider": "anthropic",
                "model": "claude-sonnet",
                "duration_ms": 12,
                "input_tokens": 120,
                "output_tokens": 30,
                "finish_reason": "stop",
                "status": "ok"
            })
        ),
    )
    .unwrap();

    let mut registry = ToolRegistry::new();
    register_all(&mut registry);
    let result = registry
        .get("cos_usage")
        .unwrap()
        .exec(json!({"command": "overall"}))
        .await;
    assert!(!result.is_error, "usage tool failed: {result:?}");
    let output: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(output["total"]["input_tokens"], 120);
    assert_eq!(output["total"]["output_tokens"], 30);
}

#[tokio::test(flavor = "current_thread")]
async fn registered_usage_tool_rejects_oversized_logs() {
    let _lock = crate::test_env::lock_env();
    let dir = tempfile::tempdir().unwrap();
    let _log_dir = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", dir.path());
    std::fs::File::create(dir.path().join("ai.jsonl"))
        .unwrap()
        .set_len(crate::agent::llm::usage::MAX_QUERY_BYTES + 1)
        .unwrap();

    let mut registry = ToolRegistry::new();
    register_all(&mut registry);
    let result = registry
        .get("cos_usage")
        .unwrap()
        .exec(json!({"command": "overall"}))
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("query limit"));
}
