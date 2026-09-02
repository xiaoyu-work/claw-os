use super::*;

#[test]
fn mcp_status_returns_catalogue() {
    let v = mcp_cmd(&["status".into()]).expect("mcp status ok");
    assert_eq!(v.get("status").and_then(|x| x.as_str()), Some("ready"));
    assert_eq!(v.get("transport").and_then(|x| x.as_str()), Some("stdio"));
    assert!(v.get("tools_registered").is_some());
    assert!(v.get("tools_permitted").is_some());
    assert!(v.get("tools").and_then(|x| x.as_array()).is_some());
}

#[test]
fn mcp_default_returns_status() {
    let v = mcp_cmd(&[]).expect("mcp default = status");
    assert_eq!(v.get("status").and_then(|x| x.as_str()), Some("ready"));
}

#[test]
fn mcp_status_includes_external_servers_section() {
    let v = mcp_cmd(&["status".into()]).expect("mcp status ok");
    // Always present even when no external servers are configured.
    assert!(v.get("external_servers_configured").is_some());
    assert!(v.get("external_servers_enabled").is_some());
    assert!(
        v.get("external_servers")
            .and_then(|x| x.as_array())
            .is_some(),
        "external_servers must be a JSON array (possibly empty)"
    );
}

#[test]
fn mcp_servers_without_probe_does_not_spawn_anything() {
    // Default test config has no mcp_servers, so this is a pure
    // shape assertion. It's still useful because a regression
    // that turned off the !probe early-return would either spawn
    // nothing (passes) or panic on attach (we'd see the failure).
    let v = mcp_cmd(&["servers".into()]).expect("mcp servers ok");
    assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
    assert_eq!(v.get("probed").and_then(|x| x.as_bool()), Some(false));
    assert!(v.get("servers").and_then(|x| x.as_array()).is_some());
}

#[test]
fn merge_mcp_overrides_preserves_base_and_denies_attended_tools() {
    let mut base = crate::config::AgentConfig::default();
    base.tool_allow = Some(vec!["echo".into()]);
    base.tool_deny = vec!["cos_sandbox".into()];
    let merged = merge_mcp_overrides(&base, None, Vec::new());
    assert_eq!(merged.tool_allow, base.tool_allow);
    assert_eq!(
        merged.tool_deny,
        vec!["cos_sandbox".to_string(), "cos_oauth_login".to_string()]
    );
}

#[test]
fn merge_mcp_overrides_allow_replaces_base_allow() {
    let mut base = crate::config::AgentConfig::default();
    base.tool_allow = Some(vec!["echo".into()]);
    let merged = merge_mcp_overrides(&base, Some(vec!["now".into()]), Vec::new());
    assert_eq!(merged.tool_allow, Some(vec!["now".into()]));
}

#[test]
fn merge_mcp_overrides_deny_appends_to_base() {
    let mut base = crate::config::AgentConfig::default();
    base.tool_deny = vec!["cos_sandbox".into()];
    let merged = merge_mcp_overrides(&base, None, vec!["cos_proc".into()]);
    assert_eq!(
        merged.tool_deny,
        vec![
            "cos_sandbox".to_string(),
            "cos_proc".to_string(),
            "cos_oauth_login".to_string()
        ]
    );
}

#[test]
fn merge_mcp_overrides_cannot_allow_attended_oauth_tool() {
    let base = crate::config::AgentConfig::default();
    let merged = merge_mcp_overrides(&base, Some(vec!["cos_oauth_login".into()]), Vec::new());

    assert_eq!(merged.tool_allow, Some(vec!["cos_oauth_login".to_string()]));
    assert!(merged
        .tool_deny
        .iter()
        .any(|name| name == "cos_oauth_login"));
}

#[test]
fn merge_mcp_overrides_does_not_mutate_base() {
    let mut base = crate::config::AgentConfig::default();
    base.tool_allow = Some(vec!["a".into()]);
    let _ = merge_mcp_overrides(&base, Some(vec!["b".into()]), vec!["c".into()]);
    // Base unchanged.
    assert_eq!(base.tool_allow, Some(vec!["a".into()]));
    assert!(base.tool_deny.is_empty());
}

// ---- mcp_cmd probe/call argument parsing ----

#[test]
fn mcp_probe_requires_cmd() {
    let err = mcp_probe(&[]).unwrap_err();
    assert!(err.contains("--cmd"));
}

#[test]
fn mcp_call_requires_cmd() {
    let err = mcp_call(&[]).unwrap_err();
    assert!(err.contains("--cmd"));
}

#[test]
fn mcp_call_requires_tool_positional() {
    let err = mcp_call(&["--cmd".into(), "nonexistent-binary-xyz-zyx".into()]).unwrap_err();
    assert!(err.contains("tool name"));
}

#[test]
fn parse_mcp_spawn_spec_collects_args_env_cwd_timeout() {
    let raw: Vec<String> = vec![
        "--cmd".into(),
        "python".into(),
        "--arg".into(),
        "-u".into(),
        "--arg".into(),
        "server.py".into(),
        "--env".into(),
        "API_KEY=secret".into(),
        "--env".into(),
        "DEBUG=1".into(),
        "--cwd".into(),
        "/tmp".into(),
        "--timeout".into(),
        "60".into(),
        "leftover-positional".into(),
    ];
    let (spec, leftover) = parse_mcp_spawn_spec(&raw).expect("parse ok");
    assert_eq!(spec.cmd, "python");
    assert_eq!(spec.args, vec!["-u", "server.py"]);
    assert_eq!(
        spec.env,
        vec![
            ("API_KEY".to_string(), "secret".to_string()),
            ("DEBUG".to_string(), "1".to_string()),
        ]
    );
    assert_eq!(spec.cwd.as_deref(), Some("/tmp"));
    assert_eq!(spec.timeout_secs, 60);
    assert_eq!(leftover, vec!["leftover-positional".to_string()]);
}

#[test]
fn parse_mcp_spawn_spec_rejects_malformed_env() {
    let raw: Vec<String> = vec![
        "--cmd".into(),
        "x".into(),
        "--env".into(),
        "noequalshere".into(),
    ];
    let err = parse_mcp_spawn_spec(&raw).unwrap_err();
    assert!(err.contains("KEY=VALUE"));
}

#[test]
fn parse_mcp_spawn_spec_default_timeout_is_30() {
    let raw: Vec<String> = vec!["--cmd".into(), "x".into()];
    let (spec, leftover) = parse_mcp_spawn_spec(&raw).expect("parse ok");
    assert_eq!(spec.timeout_secs, 30);
    assert!(leftover.is_empty());
}

#[test]
fn parse_mcp_spawn_spec_timeout_invalid_errs() {
    let raw: Vec<String> = vec![
        "--cmd".into(),
        "x".into(),
        "--timeout".into(),
        "fast".into(),
    ];
    let err = parse_mcp_spawn_spec(&raw).unwrap_err();
    assert!(err.contains("--timeout"));
}

#[test]
fn mcp_probe_propagates_spawn_failure() {
    // A binary that almost certainly doesn't exist on PATH.
    let raw: Vec<String> = vec![
        "--cmd".into(),
        "definitely-not-a-real-binary-zzz-9999".into(),
        "--timeout".into(),
        "2".into(),
    ];
    let err = mcp_probe(&raw).unwrap_err();
    // tokio::process::Command::spawn returns the underlying OS
    // error; both Windows ("program not found") and Unix ("No such
    // file") flavours are acceptable, so we only assert the binary
    // name is mentioned.
    assert!(err.contains("definitely-not-a-real-binary-zzz-9999"));
}

#[test]
fn mcp_probe_rejects_extra_positional() {
    let err = mcp_probe(&["--cmd".into(), "python".into(), "extra".into()]).unwrap_err();
    assert!(err.contains("positional"));
}

#[test]
fn mcp_call_rejects_invalid_input_json() {
    let err = mcp_call(&[
        "--cmd".into(),
        "python".into(),
        "echo".into(),
        "--input".into(),
        "not json{".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--input"));
}

#[test]
fn mcp_call_rejects_extra_positional() {
    let err = mcp_call(&[
        "--cmd".into(),
        "python".into(),
        "echo".into(),
        "another".into(),
    ])
    .unwrap_err();
    assert!(err.contains("positional"));
}
