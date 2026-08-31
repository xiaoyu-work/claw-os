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
                    "alpha_extension",
                    "app_transport",
                    "attended_cli"
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
            assert_eq!(names, vec!["beta_extension", "bob_path"]);
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
fn worker_does_not_advertise_unreachable_app_session_tools() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(
        crate::agent::tools::cos_apps_session::CosAppSessionOpen,
    ));
    let caps = crate::caps::CapSet::from_caps([crate::caps::Cap::new(
        crate::caps::Verb::AGENT_INVOKE,
        crate::caps::Scope::name("**"),
    )]);
    let direct = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity("direct", 1000, crate::session::SessionSource::LocalCli)
        .with_capabilities(caps.clone())
        .with_host(crate::agent::tools::exposure::ExecutionHost::Direct);
    let worker = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity("worker", 1000, crate::session::SessionSource::BrokerTask)
        .with_capabilities(caps)
        .with_host(crate::agent::tools::exposure::ExecutionHost::AgentWorker);

    assert!(registry
        .names_for(&direct)
        .contains(&"cos_app_session_open"));
    assert!(!registry
        .names_for(&worker)
        .contains(&"cos_app_session_open"));
    assert!(registry.get_for(&worker, "cos_app_session_open").is_none());
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
