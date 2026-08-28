use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn deps() -> RegistryDeps {
    let root = std::env::temp_dir().join(format!("cos-registry-deps-{}", std::process::id()));
    RegistryDeps::without_optional_resources(
        Arc::new(crate::config::CosConfig::default()),
        RegistryPaths {
            apps_dir: root.join("apps"),
            todos_dir: root.join("todos"),
            system_skills_dir: root.join("system-skills"),
            user_skills_dir: root.join("user-skills"),
            skills_usage_path: root.join("skills-usage.jsonl"),
            media_outputs_dir: root.join("media"),
            memory_db_path: root.join("memory.db"),
            semantic_db_path: root.join("semantic.db"),
        },
    )
}

struct OwnedDescriptorTool {
    name: String,
    description: String,
    drops: Arc<AtomicUsize>,
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
    let r = default_registry(&deps());
    assert!(r.get("echo").is_some());
    assert!(r.get("now").is_some());
    assert!(r.get("cos_delegate").is_some());
    assert!(r.get("cos_todo").is_some());
    assert!(r.get("cos_clarify").is_some());
    assert!(r.get("cos_skill").is_some());
    assert!(r.get("cos_help").is_some());
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
    // + cos_skill + cos_help
    // + every cos_proxy tool (primitives + cos_memory) + cos_app_catalog
    // + cos_app_run + 3 media tools, plus optionally cos_recall and active
    // stateful App-session tools.
    let expected_min = 7 + super::super::cos_proxy::total_count() + 2 + 3;
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
fn registry_construction_does_not_open_optional_stores_or_create_paths() {
    let deps = deps();
    let paths = deps.paths.clone();
    assert!(!paths.memory_db_path.exists());
    assert!(!paths.semantic_db_path.exists());
    assert!(!paths.todos_dir.exists());
    assert!(!paths.media_outputs_dir.exists());

    let registry = default_registry(&deps);

    assert!(registry.get("cos_recall").is_none());
    assert!(registry.get("cos_recall_semantic").is_none());
    assert!(!paths.memory_db_path.exists());
    assert!(!paths.semantic_db_path.exists());
    assert!(!paths.todos_dir.exists());
    assert!(!paths.media_outputs_dir.exists());
}

#[test]
fn names_are_sorted() {
    let r = default_registry(&deps());
    let names = r.names();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[test]
fn as_llm_tools_round_trips_schema() {
    let r = default_registry(&deps());
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
