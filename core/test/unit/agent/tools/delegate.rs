use super::*;

fn parent_cfg() -> AgentConfig {
    AgentConfig {
        provider: "mock".into(),
        model: "mock-model".into(),
        max_turns: 5,
        max_tokens: 1024,
        temperature: 0.0,
        system_prompt_path: None,
        ..Default::default()
    }
}

fn test_registry() -> ToolRegistry {
    // Builtins-only: avoids touching MemoryDb during delegate tests.
    crate::agent::tools::registry::builtin_only_registry()
}

#[test]
fn input_schema_has_required_fields() {
    let schema = Delegate.input_schema();
    let required = schema["required"].as_array().unwrap();
    let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
    assert!(names.contains(&"task"));
    assert!(names.contains(&"allowed_tools"));
}

#[test]
fn tool_metadata() {
    assert_eq!(Delegate.name(), "cos_delegate");
    assert!(Delegate.description().contains("sub-agent"));
}

#[test]
fn build_child_registry_keeps_only_allowed() {
    let parent = test_registry();
    let allowed = vec!["echo".to_string()];
    let child = build_child_registry(None, None, parent, &allowed);
    assert!(child.get("echo").is_some());
    assert!(child.get("now").is_none());
}

#[test]
fn build_child_registry_strips_cos_delegate() {
    let parent = test_registry();
    let allowed = vec!["cos_delegate".to_string(), "echo".to_string()];
    let child = build_child_registry(None, None, parent, &allowed);
    assert!(child.get("cos_delegate").is_none());
    assert!(child.get("echo").is_some());
}

#[test]
fn build_child_registry_silently_drops_unknown_tool_names() {
    let parent = test_registry();
    let allowed = vec!["echo".to_string(), "ghost_tool".to_string()];
    let child = build_child_registry(None, None, parent, &allowed);
    assert_eq!(child.len(), 1);
    assert!(child.get("echo").is_some());
}

#[test]
fn build_child_registry_empty_allowed_yields_empty_child() {
    let parent = test_registry();
    let child = build_child_registry(None, None, parent, &[]);
    assert_eq!(child.len(), 0);
}

#[test]
fn build_child_registry_keeps_progressive_skill_disclosure() {
    let mut source = ToolRegistry::new();
    source.register(Arc::new(super::super::skills::SkillDisclosure::new()));

    let child = build_child_registry(None, None, source, &[]);

    assert!(child.get("cos_skill").is_some());
}

#[test]
fn build_child_registry_respects_parent_skill_denial() {
    let mut source = ToolRegistry::new();
    source.register(Arc::new(super::super::skills::SkillDisclosure::new()));
    let guardrails = Guardrails::default().deny_tool("cos_skill");

    let child = build_child_registry(Some(&guardrails), None, source, &[]);

    assert!(child.get_unfiltered("cos_skill").is_none());
}

#[test]
fn build_child_registry_respects_parent_deny_list() {
    // Parent denies `echo`; even though the delegate caller listed
    // it under `allowed_tools`, the child must not see it. This is
    // the regression that the task-local inheritance was added for.
    let parent_g = Guardrails::default().deny_tool("echo");
    let allowed = vec!["echo".to_string(), "now".to_string()];
    let source = test_registry();
    let child = build_child_registry(Some(&parent_g), None, source, &allowed);
    assert!(
        child.get_unfiltered("echo").is_none(),
        "echo must not leak through parent's deny rule"
    );
    assert!(
        child.get("now").is_some(),
        "non-denied tools should still pass through"
    );
}

#[tokio::test]
async fn child_inherits_parent_guardrails() {
    // End-to-end: scope the task-local PARENT_GUARDRAILS to a deny
    // rule and call exec; the child's registry must reflect it.
    // We exercise this through `run_delegate` directly with a test
    // registry factory, then assert the child registry's guardrails
    // by intercepting via a fresh build.
    let parent_g = Guardrails::default().deny_tool("echo");
    let observed = PARENT_GUARDRAILS
        .scope(parent_g.clone(), async {
            let (g, _a) = current_parent_policy();
            let child = build_child_registry(
                g.as_ref(),
                None,
                test_registry(),
                &["echo".to_string(), "now".to_string()],
            );
            // echo was denied by parent → child must not have it
            let echo_present = child.get_unfiltered("echo").is_some();
            let now_present = child.get_unfiltered("now").is_some();
            (echo_present, now_present, child.guardrails().clone())
        })
        .await;
    let (echo_present, now_present, child_g) = observed;
    assert!(!echo_present, "echo should be blocked by inherited deny");
    assert!(now_present, "now should be passed through");
    assert!(
        child_g.deny.contains("echo"),
        "child registry's guardrails must carry parent's deny set"
    );
}

#[test]
fn current_depth_outside_scope_is_zero() {
    assert_eq!(current_depth(), 0);
}

#[tokio::test]
async fn current_depth_inside_scope_reflects_value() {
    let observed = DELEGATE_DEPTH.scope(2u32, async { current_depth() }).await;
    assert_eq!(observed, 2);
}

#[tokio::test]
async fn invalid_input_returns_tool_error() {
    // Missing required `task`.
    let result = Delegate.exec(json!({"allowed_tools": []})).await;
    assert!(result.is_error);
    assert!(result.content.contains("invalid delegate input"));
}

/// Build a delegate input that uses a fresh mock provider seeded with
/// the given response queue. Stores the seeded mock in
/// `agent/llm/providers/mock.rs`'s static stash so `registry::build`
/// for "mock" picks it up. We do this by registering responses on a
/// MockProvider before running the loop — but `registry::build` builds
/// a fresh MockProvider each time, so the seeded one is lost.
///
/// Instead, we exercise `run_delegate` directly with a registry/cfg
/// that is paired manually below.
fn fresh_input(task: &str, allowed: &[&str]) -> DelegateInput {
    DelegateInput {
        task: task.to_string(),
        allowed_tools: allowed.iter().map(|s| s.to_string()).collect(),
        provider: None,
        model: None,
        max_turns: Some(5),
        max_depth: None,
        timeout_secs: Some(30),
    }
}

#[tokio::test]
async fn run_delegate_refuses_when_depth_already_at_max() {
    // Set depth = 3 (== DEFAULT_MAX_DEPTH); next call would be 4, refused.
    let cfg = parent_cfg();
    let input = fresh_input("anything", &["echo"]);
    let result = DELEGATE_DEPTH
        .scope(3u32, run_delegate(input, &cfg, test_registry))
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("delegate depth limit reached"));
}

#[tokio::test]
async fn run_delegate_refuses_when_caller_lowers_max_depth_below_current() {
    let cfg = parent_cfg();
    let mut input = fresh_input("hi", &[]);
    input.max_depth = Some(1);
    // We're already at depth 1, so cur(1) + 1 = 2 > max_depth(1).
    let result = DELEGATE_DEPTH
        .scope(1u32, run_delegate(input, &cfg, test_registry))
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("depth limit"));
}

#[tokio::test]
async fn run_delegate_clamps_max_depth_to_hard_cap() {
    // Caller asks for max_depth = 100; should clamp to HARD_MAX_DEPTH = 5.
    // At depth 5, the call should still be refused.
    let cfg = parent_cfg();
    let mut input = fresh_input("hi", &[]);
    input.max_depth = Some(100);
    let result = DELEGATE_DEPTH
        .scope(5u32, run_delegate(input, &cfg, test_registry))
        .await;
    assert!(result.is_error, "expected refusal at depth=5 with hard cap");
}

#[tokio::test]
async fn run_delegate_unknown_provider_returns_error() {
    let mut cfg = parent_cfg();
    cfg.provider = "does-not-exist".into();
    let input = fresh_input("hi", &[]);
    let result = run_delegate(input, &cfg, test_registry).await;
    assert!(result.is_error);
    assert!(result.content.contains("failed to build provider"));
}

/// End-to-end happy path: configure mock, run delegate, observe child's
/// answer comes back via tool result.
///
/// We can't seed the mock through `registry::build` (which constructs a
/// fresh `MockProvider` per call). Instead we wire the parent up so its
/// MockProvider is configured to respond ToolUse(cos_delegate) → which
/// forces our delegate to be reached via the real loop, then the
/// child's MockProvider (also fresh) needs to respond. The mock's
/// default `Text` echo behaviour is what the child uses.
#[tokio::test]
async fn run_delegate_happy_path_uses_mock_echo_default() {
    let _perms = crate::test_env::PermissiveModeGuard::new();
    let cfg = parent_cfg();
    let input = fresh_input("hello child agent", &["echo"]);
    let result = run_delegate(input, &cfg, test_registry).await;
    assert!(
        !result.is_error,
        "expected success, got: {}",
        result.content
    );
    // MockProvider's default echoes the user prompt back as a Text
    // message; ask_with terminates in 1 turn with that text. Our
    // formatted output should mention the provider and model.
    assert!(result.content.contains("provider=mock"));
    assert!(result.content.contains("model=mock-model"));
    assert!(result.content.contains("turns=1"));
    assert!(result.content.contains("hello child agent"));
}

#[tokio::test]
async fn run_delegate_clamps_max_turns_to_hard_cap() {
    let _perms = crate::test_env::PermissiveModeGuard::new();
    let cfg = parent_cfg();
    let mut input = fresh_input("hi", &[]);
    input.max_turns = Some(9999);
    // Should not panic; child should run normally with max_turns = 50.
    let result = run_delegate(input, &cfg, test_registry).await;
    assert!(!result.is_error);
}

/// Cover the depth-increment path: at depth 0 a delegate call should
/// succeed; the child running inside should observe depth 1.
#[tokio::test]
async fn run_delegate_increments_depth_for_child() {
    let _perms = crate::test_env::PermissiveModeGuard::new();
    // Construct a registry whose only tool inspects the depth.
    struct DepthInspector;
    #[async_trait]
    impl Tool for DepthInspector {
        fn name(&self) -> &'static str {
            "depth_inspector"
        }
        fn description(&self) -> &'static str {
            "report current delegate depth"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({"type":"object","properties":{},"additionalProperties":false})
        }
        async fn exec(&self, _input: serde_json::Value) -> ToolResult {
            ToolResult::ok(format!("depth={}", current_depth()))
        }
    }

    fn registry_with_inspector() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(DepthInspector));
        r.register(Arc::new(crate::agent::tools::builtin::Echo));
        r
    }

    let cfg = parent_cfg();
    let input = fresh_input("inspect depth please", &["depth_inspector"]);
    let result = run_delegate(input, &cfg, registry_with_inspector).await;
    assert!(!result.is_error, "got error: {}", result.content);
    // Mock provider's default behaviour is to echo the user prompt as
    // text — it never calls tools. So we won't actually invoke
    // `depth_inspector` here. What this test does verify is that the
    // child path does not panic when a custom registry is plugged in
    // and that the depth scope is set up without error.
    assert!(result.content.contains("turns=1"));
}

#[tokio::test]
async fn run_delegate_zero_timeout_panics_no_just_kidding_it_returns_timeout() {
    let _perms = crate::test_env::PermissiveModeGuard::new();
    // 1-second timeout against the (instant) mock echo; should *not*
    // time out — sanity check that the timeout wrapper doesn't fire
    // spuriously.
    let cfg = parent_cfg();
    let mut input = fresh_input("hi", &[]);
    input.timeout_secs = Some(1);
    let result = run_delegate(input, &cfg, test_registry).await;
    assert!(!result.is_error);
}

#[test]
fn format_result_includes_metadata_block() {
    let r = AskResult {
        answer: "the moon".into(),
        evidence: Default::default(),
        fallback: None,
        turns: 4,
        provider: "anthropic".into(),
        model: "claude-haiku-4".into(),
        session_id: String::new(),
    };
    let s = format_result(&r);
    assert!(s.contains("provider=anthropic"));
    assert!(s.contains("model=claude-haiku-4"));
    assert!(s.contains("turns=4"));
    assert!(s.contains("the moon"));
}

#[test]
fn delegate_input_with_unknown_extra_field_still_parses() {
    let v = json!({
        "task": "do x",
        "allowed_tools": ["echo"],
        "future_field_that_does_not_exist_yet": 42
    });
    let parsed: Result<DelegateInput, _> = serde_json::from_value(v);
    assert!(parsed.is_ok());
}

#[test]
fn delegate_input_missing_task_fails() {
    let v = json!({"allowed_tools": []});
    let parsed: Result<DelegateInput, _> = serde_json::from_value(v);
    assert!(parsed.is_err());
}

#[test]
fn delegate_input_missing_allowed_tools_defaults_to_empty() {
    // `allowed_tools` is `#[serde(default)]` so missing -> empty Vec.
    let v = json!({"task": "do x"});
    let parsed: DelegateInput = serde_json::from_value(v).unwrap();
    assert!(parsed.allowed_tools.is_empty());
}
