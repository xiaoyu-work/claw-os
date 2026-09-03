use super::*;
use crate::agent::tools::exposure::ToolTransport;
use crate::agent::tools::guardrails::Guardrails;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct OwnedDescriptorTool {
    name: String,
    description: String,
    drops: Arc<AtomicUsize>,
}

struct ContextTool {
    name: &'static str,
    exposure: ToolExposure,
    schema_calls: Arc<AtomicUsize>,
    executions: Arc<AtomicUsize>,
}

struct ExtensionTestTool {
    name: String,
    server: String,
    remote_name: String,
    description: String,
    schema: serde_json::Value,
    exposure: ToolExposure,
    attachment: Option<ToolAttachment>,
    executions: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::agent::tools::Tool for ExtensionTestTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    fn exposure(&self) -> ToolExposure {
        self.exposure.clone()
    }

    fn disclosure(&self) -> crate::agent::tools::progressive::ToolDisclosure {
        crate::agent::tools::progressive::ToolDisclosure::extension(
            "mcp",
            Some(self.server.clone()),
            Some(self.remote_name.clone()),
            ["mcp".to_string(), "test".to_string()],
        )
    }

    fn is_available(&self) -> bool {
        self.attachment
            .as_ref()
            .is_none_or(ToolAttachment::is_active)
    }

    async fn exec(&self, input: serde_json::Value) -> crate::agent::tools::ToolResult {
        let Some(path) = input.get("path").and_then(serde_json::Value::as_str) else {
            return crate::agent::tools::ToolResult::err("path is required");
        };
        let Some(context) = crate::agent::tools::exposure::current() else {
            return crate::agent::tools::ToolResult::err("missing exposure context");
        };
        let requested =
            crate::caps::Cap::new(crate::caps::Verb::FS_READ, crate::caps::Scope::path(path));
        if !context.capabilities().covers(&requested) {
            return crate::agent::tools::ToolResult::err("exact path is not authorized");
        }
        self.executions.fetch_add(1, Ordering::SeqCst);
        crate::agent::tools::ToolResult::ok("ok")
    }

    fn parallel_safe(&self) -> bool {
        true
    }
}

fn extension_tool(
    name: &str,
    server: &str,
    description: &str,
    schema: serde_json::Value,
    exposure: ToolExposure,
    attachment: Option<ToolAttachment>,
    executions: Arc<AtomicUsize>,
) -> Arc<dyn crate::agent::tools::Tool> {
    Arc::new(ExtensionTestTool {
        name: name.to_string(),
        server: server.to_string(),
        remote_name: name
            .strip_prefix(&format!("mcp_{server}_"))
            .unwrap_or(name)
            .to_string(),
        description: description.to_string(),
        schema,
        exposure,
        attachment,
        executions,
    })
}

#[async_trait::async_trait]
impl crate::agent::tools::Tool for ContextTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "context-sensitive test tool"
    }

    fn input_schema(&self) -> serde_json::Value {
        self.schema_calls.fetch_add(1, Ordering::SeqCst);
        serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "additionalProperties": false
        })
    }

    fn exposure(&self) -> ToolExposure {
        self.exposure.clone()
    }

    async fn exec(&self, input: serde_json::Value) -> crate::agent::tools::ToolResult {
        let Some(path) = input.get("path").and_then(serde_json::Value::as_str) else {
            return crate::agent::tools::ToolResult::err("path is required");
        };
        let Some(context) = crate::agent::tools::exposure::current() else {
            return crate::agent::tools::ToolResult::err("missing exposure context");
        };
        let requested =
            crate::caps::Cap::new(crate::caps::Verb::FS_READ, crate::caps::Scope::path(path));
        if !context.capabilities().covers(&requested) {
            return crate::agent::tools::ToolResult::err("exact path is not authorized");
        }
        self.executions.fetch_add(1, Ordering::SeqCst);
        crate::agent::tools::ToolResult::ok("ok")
    }
}

impl Drop for OwnedDescriptorTool {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl crate::agent::tools::Tool for OwnedDescriptorTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn exec(&self, _input: serde_json::Value) -> crate::agent::tools::ToolResult {
        crate::agent::tools::ToolResult::ok("ok")
    }
}

#[test]
fn default_registry_has_builtins_and_cos_proxy() {
    let r = default_registry();
    assert!(r.get("echo").is_some());
    assert!(r.get("now").is_some());
    assert!(r.get("cos_delegate").is_some());
    assert!(r.get("cos_todo").is_some());
    assert!(r.get("cos_clarify").is_some());
    assert!(r.get("cos_skill").is_some());
    assert!(r.get("cos_sandbox").is_some());
    assert!(r.get("cos_sysinfo").is_some());
    assert!(r.get("cos_memory").is_some());
    assert!(r.get("cos_tts").is_some());
    assert!(r.get("cos_stt").is_some());
    assert!(r.get("cos_imagegen").is_some());
    assert!(r.get("cos_doctor").is_some());
    // Generic catalog + run are always registered, regardless of
    // whether any typed cos_app_<id> proxies were picked up from
    // $COS_APPS_DIR (which is environment-dependent at test time).
    assert!(r.get("cos_app_catalog").is_some());
    assert!(r.get("cos_app_run").is_some());

    // Lower bound: 2 builtins + cos_delegate + cos_todo + cos_clarify
    // + cos_skill
    // + every cos_proxy tool (primitives + cos_memory) + cos_app_catalog
    // + cos_app_run + 3 media tools, plus optionally cos_recall and active
    // stateful App-session tools.
    let expected_min = 6 + super::super::cos_proxy::total_count() + 2 + 3;
    assert!(
        r.len() >= expected_min,
        "expected at least {} tools, got {}",
        expected_min,
        r.len()
    );
}

#[test]
fn builtin_only_registry_has_just_builtins() {
    let r = builtin_only_registry();
    assert_eq!(r.len(), 2);
    assert!(r.get("cos_sandbox").is_none());
}

#[test]
fn names_are_sorted() {
    let r = default_registry();
    let names = r.names();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[test]
fn as_llm_tools_round_trips_schema() {
    let r = default_registry();
    let tools = r.as_llm_tools();
    assert!(tools.iter().any(|t| t.name == "echo"));
}

#[test]
fn repeated_registries_release_owned_dynamic_descriptors() {
    const BUILDS: usize = 64;
    let drops = Arc::new(AtomicUsize::new(0));

    for build in 0..BUILDS {
        let name = format!("dynamic_{build}");
        let description = format!("descriptor for build {build}");
        {
            let mut registry = ToolRegistry::new();
            registry.register(Arc::new(OwnedDescriptorTool {
                name: name.clone(),
                description: description.clone(),
                drops: drops.clone(),
            }));

            assert_eq!(registry.names_unfiltered(), vec![name.as_str()]);
            assert_eq!(
                registry
                    .get_unfiltered(&name)
                    .expect("dynamic tool registered")
                    .description(),
                description
            );
        }

        assert_eq!(drops.load(Ordering::SeqCst), build + 1);
    }
}

#[test]
fn unique_registration_never_replaces_an_existing_descriptor() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(OwnedDescriptorTool {
        name: "dynamic".to_string(),
        description: "trusted".to_string(),
        drops: drops.clone(),
    }));
    let rejected = Arc::new(OwnedDescriptorTool {
        name: "dynamic".to_string(),
        description: "untrusted replacement".to_string(),
        drops,
    });
    assert!(registry.register_unique(rejected).is_err());
    assert_eq!(
        registry
            .descriptor_unfiltered("dynamic")
            .unwrap()
            .description,
        "trusted"
    );
}

#[test]
fn descriptor_schema_is_cached_without_caching_session_decisions() {
    let schema_calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ContextTool {
        name: "scoped",
        exposure: ToolExposure::always().requiring_all_verbs([crate::caps::Verb::FS_READ]),
        schema_calls: schema_calls.clone(),
        executions,
    }));
    assert_eq!(schema_calls.load(Ordering::SeqCst), 1);

    let allowed = ToolExposureContext::isolated(Guardrails::permissive()).with_capabilities(
        crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::FS_READ,
            crate::caps::Scope::path("/home/alice/**"),
        )]),
    );
    let denied = ToolExposureContext::isolated(Guardrails::permissive());

    assert_eq!(registry.as_llm_tools_for(&allowed).len(), 1);
    assert!(registry.as_llm_tools_for(&denied).is_empty());
    assert_eq!(registry.as_llm_tools_for(&allowed).len(), 1);
    assert_eq!(
        schema_calls.load(Ordering::SeqCst),
        1,
        "projection must clone the immutable descriptor, not rebuild it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_sessions_cannot_leak_owner_source_grant_or_transport_schemas() {
    let schema_calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    for (name, exposure) in [
        (
            "alice_path",
            ToolExposure::always().requiring_caps([crate::caps::Cap::new(
                crate::caps::Verb::FS_READ,
                crate::caps::Scope::path("/home/alice/**"),
            )]),
        ),
        (
            "bob_path",
            ToolExposure::always().requiring_caps([crate::caps::Cap::new(
                crate::caps::Verb::FS_READ,
                crate::caps::Scope::path("/home/bob/**"),
            )]),
        ),
        (
            "attended_cli",
            ToolExposure::always()
                .from_sources([crate::session::SessionSource::LocalCli])
                .requiring_attended_local(),
        ),
        (
            "app_transport",
            ToolExposure::always().requiring_transport(ToolTransport::AppSession),
        ),
        (
            "alpha_extension",
            ToolExposure::always().requiring_extension("mcp:alpha"),
        ),
        (
            "beta_extension",
            ToolExposure::always().requiring_extension("mcp:beta"),
        ),
    ] {
        registry.register(Arc::new(ContextTool {
            name,
            exposure,
            schema_calls: schema_calls.clone(),
            executions: executions.clone(),
        }));
    }
    let registry = Arc::new(registry);

    let mut alice = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity(
            "alice-session",
            1000,
            crate::session::SessionSource::LocalCli,
        )
        .with_presence(true, true)
        .with_transport(ToolTransport::AppSession, true)
        .with_capabilities(crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::FS_READ,
            crate::caps::Scope::path("/home/alice/**"),
        )]));
    alice.enable_extension("mcp:alpha");

    let mut bob = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity(
            "bob-session",
            1001,
            crate::session::SessionSource::ExternalMcp,
        )
        .with_presence(false, true)
        .with_transport(ToolTransport::AppSession, false)
        .with_capabilities(crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::FS_READ,
            crate::caps::Scope::path("/home/bob/**"),
        )]));
    bob.enable_extension("mcp:beta");

    let alice_registry = registry.clone();
    let alice_task = tokio::spawn(async move {
        for _ in 0..100 {
            let names: Vec<String> = alice_registry
                .as_llm_tools_for(&alice)
                .into_iter()
                .map(|tool| tool.name)
                .collect();
            assert_eq!(
                names,
                vec![
                    "alice_path",
                    "app_transport",
                    "attended_cli",
                    "cos_tool_call",
                    "cos_tool_describe",
                    "cos_tool_search",
                ]
            );
        }
    });
    let bob_registry = registry.clone();
    let bob_task = tokio::spawn(async move {
        for _ in 0..100 {
            let names: Vec<String> = bob_registry
                .as_llm_tools_for(&bob)
                .into_iter()
                .map(|tool| tool.name)
                .collect();
            assert_eq!(
                names,
                vec![
                    "bob_path",
                    "cos_tool_call",
                    "cos_tool_describe",
                    "cos_tool_search",
                ]
            );
        }
    });
    let (alice_result, bob_result) = tokio::join!(alice_task, bob_task);
    alice_result.unwrap();
    bob_result.unwrap();
}

#[tokio::test]
async fn execution_rechecks_exact_validated_arguments_after_exposure() {
    let schema_calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ContextTool {
        name: "scoped",
        exposure: ToolExposure::always().requiring_all_verbs([crate::caps::Verb::FS_READ]),
        schema_calls,
        executions: executions.clone(),
    }));
    let context = ToolExposureContext::isolated(Guardrails::permissive()).with_capabilities(
        crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::FS_READ,
            crate::caps::Scope::path("/home/alice/**"),
        )]),
    );

    assert_eq!(registry.as_llm_tools_for(&context).len(), 1);
    let denied = registry
        .execute(
            &context,
            "scoped",
            serde_json::json!({"path": "/home/bob/secret"}),
            "test",
        )
        .await;
    assert!(denied.is_error);
    assert_eq!(executions.load(Ordering::SeqCst), 0);

    let allowed = registry
        .execute(
            &context,
            "scoped",
            serde_json::json!({"path": "/home/alice/note"}),
            "test",
        )
        .await;
    assert!(!allowed.is_error);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[test]
fn oauth_schema_requires_trusted_attended_source_not_just_local_presence() {
    let registry = default_registry();
    let cli = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity("cli", 1000, crate::session::SessionSource::LocalCli)
        .with_presence(true, true);
    let external = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity("mcp", 1000, crate::session::SessionSource::ExternalMcp)
        .with_presence(true, true);

    assert!(registry.get_for(&cli, "cos_oauth_login").is_some());
    assert!(registry.get_for(&external, "cos_oauth_login").is_none());
}

#[test]
fn small_extension_catalog_remains_deferred_and_deterministic() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = builtin_only_registry();
    registry.register(extension_tool(
        "mcp_alpha_lookup",
        "alpha",
        "Look up a record.",
        serde_json::json!({"type": "object", "properties": {}}),
        ToolExposure::always(),
        None,
        executions,
    ));
    let context = ToolExposureContext::isolated(Guardrails::permissive())
        .with_tool_schema_budget_tokens(10_000);

    let first = registry.projection_for(&context);
    let second = registry.projection_for(&context);
    assert_eq!(
        serde_json::to_value(first.tools()).unwrap(),
        serde_json::to_value(second.tools()).unwrap()
    );
    assert_eq!(first.diagnostics(), second.diagnostics());
    assert!(first.diagnostics().progressive);
    assert_eq!(first.diagnostics().deferred_count, 1);
    assert!(!first
        .tools()
        .iter()
        .any(|tool| tool.name == "mcp_alpha_lookup"));
    assert!(first
        .tools()
        .iter()
        .any(|tool| crate::agent::tools::progressive::is_bridge_tool(&tool.name)));
}

#[test]
fn extension_exposure_is_automatically_budget_eligible() {
    let schema_calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ContextTool {
        name: "future_extension_tool",
        exposure: ToolExposure::always().requiring_extension("future:alpha"),
        schema_calls,
        executions,
    }));
    let mut context =
        ToolExposureContext::isolated(Guardrails::permissive()).with_tool_schema_budget_tokens(0);
    context.enable_extension("future:alpha");

    let projection = registry.projection_for(&context);
    assert_eq!(projection.diagnostics().deferred_count, 1);
    assert!(projection
        .tools()
        .iter()
        .any(|tool| tool.name == crate::agent::tools::progressive::TOOL_CALL));
}

#[test]
fn large_extension_catalog_defers_while_core_tools_stay_direct() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = builtin_only_registry();
    for index in 0..6 {
        registry.register(extension_tool(
            &format!("mcp_alpha_tool_{index}"),
            "alpha",
            &format!("Extension tool {index}. {}", "description ".repeat(40)),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "payload": {"type": "string", "description": "x".repeat(400)}
                }
            }),
            ToolExposure::always(),
            None,
            executions.clone(),
        ));
    }
    let context =
        ToolExposureContext::isolated(Guardrails::permissive()).with_tool_schema_budget_tokens(1);
    let projection = registry.projection_for(&context);
    let names = projection
        .tools()
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();

    assert!(projection.diagnostics().progressive);
    assert_eq!(projection.diagnostics().deferred_count, 6);
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"now"));
    assert!(names.contains(&crate::agent::tools::progressive::TOOL_SEARCH));
    assert!(names.contains(&crate::agent::tools::progressive::TOOL_DESCRIBE));
    assert!(names.contains(&crate::agent::tools::progressive::TOOL_CALL));
    assert!(!names.iter().any(|name| name.starts_with("mcp_alpha_")));
    assert!(projection.diagnostics().schema_tokens < projection.diagnostics().raw_schema_tokens);
}

#[test]
fn one_oversized_schema_triggers_progressive_disclosure() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = builtin_only_registry();
    registry.register(extension_tool(
        "mcp_large_payload",
        "large",
        "Large schema.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "payload": {"type": "string", "description": "x".repeat(64_000)}
            }
        }),
        ToolExposure::always(),
        None,
        executions,
    ));
    let context =
        ToolExposureContext::isolated(Guardrails::permissive()).with_tool_schema_budget_tokens(128);
    let projection = registry.projection_for(&context);
    assert_eq!(projection.diagnostics().deferred_count, 1);
    assert!(!projection
        .tools()
        .iter()
        .any(|tool| tool.name == "mcp_large_payload"));
}

#[test]
fn denied_tools_never_enter_search_describe_or_call() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = builtin_only_registry();
    registry.register(extension_tool(
        "mcp_alpha_allowed",
        "alpha",
        "Allowed lookup.",
        serde_json::json!({"type": "object", "properties": {}}),
        ToolExposure::always(),
        None,
        executions.clone(),
    ));
    registry.register(extension_tool(
        "mcp_alpha_denied",
        "alpha",
        "Denied lookup.",
        serde_json::json!({"type": "object", "properties": {}}),
        ToolExposure::always(),
        None,
        executions,
    ));
    let context =
        ToolExposureContext::isolated(Guardrails::permissive().deny_tool("mcp_alpha_denied"))
            .with_tool_schema_budget_tokens(0);

    let search = registry.execute_catalog(
        &context,
        crate::agent::tools::progressive::TOOL_SEARCH,
        &serde_json::json!({"query": "*"}),
    );
    assert!(!search.is_error);
    assert!(search.content.contains("mcp_alpha_allowed"));
    assert!(!search.content.contains("mcp_alpha_denied"));

    let describe = registry.execute_catalog(
        &context,
        crate::agent::tools::progressive::TOOL_DESCRIBE,
        &serde_json::json!({"name": "mcp_alpha_denied"}),
    );
    assert!(describe.is_error);

    let resolved = registry.resolve_model_call(
        &context,
        &ToolCall {
            id: "denied".to_string(),
            name: crate::agent::tools::progressive::TOOL_CALL.to_string(),
            input: serde_json::json!({
                "name": "mcp_alpha_denied",
                "arguments": {"path": "/home/alice/file"}
            }),
        },
    );
    assert!(matches!(resolved.kind, ResolvedToolKind::Rejected(_)));
}

#[tokio::test]
async fn bridge_reuses_exact_capability_and_approval_boundaries() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = builtin_only_registry();
    registry.register(extension_tool(
        "mcp_alpha_read",
        "alpha",
        "Read one path.",
        serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
        ToolExposure::always().requiring_all_verbs([crate::caps::Verb::FS_READ]),
        None,
        executions.clone(),
    ));
    registry.set_approval(crate::agent::runtime::approval::ApprovalGate::new(
        crate::agent::runtime::approval::ApprovalConfig::new().auto_deny("mcp_alpha_read"),
    ));
    let context = ToolExposureContext::isolated(Guardrails::permissive())
        .with_capabilities(crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::FS_READ,
            crate::caps::Scope::path("/home/alice/**"),
        )]))
        .with_tool_schema_budget_tokens(0);
    let resolved = registry.resolve_model_call(
        &context,
        &ToolCall {
            id: "call".to_string(),
            name: crate::agent::tools::progressive::TOOL_CALL.to_string(),
            input: serde_json::json!({
                "name": "mcp_alpha_read",
                "arguments": {"path": "/home/alice/file"}
            }),
        },
    );
    assert_eq!(resolved.call.name, "mcp_alpha_read");
    assert!(matches!(resolved.kind, ResolvedToolKind::Registry));
    let denied = registry
        .execute(
            &context,
            &resolved.call.name,
            resolved.call.input,
            "test bridge",
        )
        .await;
    assert!(denied.is_error);
    assert!(denied.content.contains("approval denied"));
    assert_eq!(executions.load(Ordering::SeqCst), 0);

    registry.set_approval(crate::agent::runtime::approval::ApprovalGate::default());
    let wrong_scope = registry.resolve_model_call(
        &context,
        &ToolCall {
            id: "wrong-scope".to_string(),
            name: crate::agent::tools::progressive::TOOL_CALL.to_string(),
            input: serde_json::json!({
                "name": "mcp_alpha_read",
                "arguments": {"path": "/home/bob/secret"}
            }),
        },
    );
    let denied = registry
        .execute(
            &context,
            &wrong_scope.call.name,
            wrong_scope.call.input,
            "test bridge",
        )
        .await;
    assert!(denied.is_error);
    assert!(denied.content.contains("exact path is not authorized"));
    assert_eq!(executions.load(Ordering::SeqCst), 0);

    let allowed_scope = registry.resolve_model_call(
        &context,
        &ToolCall {
            id: "allowed-scope".to_string(),
            name: crate::agent::tools::progressive::TOOL_CALL.to_string(),
            input: serde_json::json!({
                "name": "mcp_alpha_read",
                "arguments": {"path": "/home/alice/file"}
            }),
        },
    );
    let allowed = registry
        .execute(
            &context,
            &allowed_scope.call.name,
            allowed_scope.call.input,
            "test bridge",
        )
        .await;
    assert!(!allowed.is_error);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn approving_bridge_name_does_not_approve_underlying_legacy_tool() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = builtin_only_registry();
    registry.register(extension_tool(
        "mcp_alpha_read",
        "alpha",
        "Read one path.",
        serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
        ToolExposure::always().requiring_all_verbs([crate::caps::Verb::FS_READ]),
        None,
        executions.clone(),
    ));
    registry.set_approval(crate::agent::runtime::approval::ApprovalGate::new(
        crate::agent::runtime::approval::ApprovalConfig::new()
            .auto_approve(crate::agent::tools::progressive::TOOL_CALL)
            .dangerous("mcp_alpha_read"),
    ));
    let context = ToolExposureContext::isolated(Guardrails::permissive())
        .with_capabilities(crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::FS_READ,
            crate::caps::Scope::path("/home/alice/**"),
        )]))
        .with_tool_schema_budget_tokens(0);
    let resolved = registry.resolve_model_call(
        &context,
        &ToolCall {
            id: "legacy-approval".to_string(),
            name: crate::agent::tools::progressive::TOOL_CALL.to_string(),
            input: serde_json::json!({
                "name": "mcp_alpha_read",
                "arguments": {"path": "/home/alice/file"}
            }),
        },
    );
    let result = registry
        .execute(
            &context,
            &resolved.call.name,
            resolved.call.input,
            "test bridge",
        )
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("approval pending"));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

#[test]
fn bridge_cannot_invoke_core_or_bypass_direct_deferred_dispatch() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = builtin_only_registry();
    registry.register(extension_tool(
        "mcp_alpha_lookup",
        "alpha",
        "Lookup.",
        serde_json::json!({"type": "object", "properties": {}}),
        ToolExposure::always(),
        None,
        executions,
    ));
    let context =
        ToolExposureContext::isolated(Guardrails::permissive()).with_tool_schema_budget_tokens(0);

    let direct = registry.resolve_model_call(
        &context,
        &ToolCall {
            id: "direct".to_string(),
            name: "mcp_alpha_lookup".to_string(),
            input: serde_json::json!({}),
        },
    );
    assert!(matches!(direct.kind, ResolvedToolKind::Rejected(_)));

    let core = registry.resolve_model_call(
        &context,
        &ToolCall {
            id: "core".to_string(),
            name: crate::agent::tools::progressive::TOOL_CALL.to_string(),
            input: serde_json::json!({"name": "echo", "arguments": {"text": "bypass"}}),
        },
    );
    assert!(matches!(core.kind, ResolvedToolKind::Rejected(_)));

    registry.set_approval(crate::agent::runtime::approval::ApprovalGate::new(
        crate::agent::runtime::approval::ApprovalConfig::new()
            .auto_deny(crate::agent::tools::progressive::TOOL_CALL),
    ));
    let denied_bridge = registry.resolve_model_call(
        &context,
        &ToolCall {
            id: "denied-bridge".to_string(),
            name: crate::agent::tools::progressive::TOOL_CALL.to_string(),
            input: serde_json::json!({
                "name": "mcp_alpha_lookup",
                "arguments": {}
            }),
        },
    );
    assert!(matches!(
        denied_bridge.kind,
        ResolvedToolKind::Rejected(ref reason) if reason.contains("auto_deny")
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progressive_catalogs_do_not_leak_between_concurrent_sessions() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(extension_tool(
        "mcp_shared_alice",
        "shared",
        "Alice scoped.",
        serde_json::json!({"type": "object"}),
        ToolExposure::always()
            .requiring_caps([crate::caps::Cap::new(
                crate::caps::Verb::FS_READ,
                crate::caps::Scope::path("/home/alice/**"),
            )])
            .requiring_extension("mcp:alice"),
        None,
        executions.clone(),
    ));
    registry.register(extension_tool(
        "mcp_shared_bob",
        "shared",
        "Bob scoped.",
        serde_json::json!({"type": "object"}),
        ToolExposure::always()
            .requiring_caps([crate::caps::Cap::new(
                crate::caps::Verb::FS_READ,
                crate::caps::Scope::path("/home/bob/**"),
            )])
            .requiring_extension("mcp:bob"),
        None,
        executions,
    ));
    let registry = Arc::new(registry);
    let mut alice = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity(
            "alice-session",
            1000,
            crate::session::SessionSource::BrokerTask,
        )
        .with_capabilities(crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::FS_READ,
            crate::caps::Scope::path("/home/alice/**"),
        )]))
        .with_tool_schema_budget_tokens(0);
    alice.enable_extension("mcp:alice");
    let mut bob = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity(
            "bob-session",
            1001,
            crate::session::SessionSource::BrokerTask,
        )
        .with_capabilities(crate::caps::CapSet::from_caps([crate::caps::Cap::new(
            crate::caps::Verb::FS_READ,
            crate::caps::Scope::path("/home/bob/**"),
        )]))
        .with_tool_schema_budget_tokens(0);
    bob.enable_extension("mcp:bob");

    let alice_registry = registry.clone();
    let alice_task = tokio::spawn(async move {
        let result = alice_registry.execute_catalog(
            &alice,
            crate::agent::tools::progressive::TOOL_SEARCH,
            &serde_json::json!({"query": "*"}),
        );
        assert!(result.content.contains("mcp_shared_alice"));
        assert!(!result.content.contains("mcp_shared_bob"));
    });
    let bob_registry = registry.clone();
    let bob_task = tokio::spawn(async move {
        let result = bob_registry.execute_catalog(
            &bob,
            crate::agent::tools::progressive::TOOL_SEARCH,
            &serde_json::json!({"query": "*"}),
        );
        assert!(result.content.contains("mcp_shared_bob"));
        assert!(!result.content.contains("mcp_shared_alice"));
    });
    let (alice_result, bob_result) = tokio::join!(alice_task, bob_task);
    alice_result.unwrap();
    bob_result.unwrap();
}

#[test]
fn reserved_bridge_names_cannot_be_shadowed() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(OwnedDescriptorTool {
        name: crate::agent::tools::progressive::TOOL_CALL.to_string(),
        description: "shadow".to_string(),
        drops,
    }));
    assert!(registry
        .get_unfiltered(crate::agent::tools::progressive::TOOL_CALL)
        .is_none());
}
