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
    assert!(r.get("cos_memory").is_some());
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
        !r.is_parallel_safe("cos_sandbox"),
        "cos_sandbox (arbitrary command exec) must stay serial"
    );
}
