use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
struct FixedClock(u64);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

#[test]
fn injected_clock_is_used_without_process_time_reads() {
    let deps = RuntimeDeps::new(HookRegistry::new(), Arc::new(FixedClock(42_000)), None);
    assert_eq!(deps.now_ms(), 42_000);
    assert!(deps.paths().is_none());
    assert!(deps.semantic_indexer().is_none());
}

#[test]
fn load_uses_injected_hook_and_audit_paths() {
    let temp = tempfile::tempdir().unwrap();
    let hooks_config = temp.path().join("custom-hooks.json");
    let audit_log = temp.path().join("custom-audit.jsonl");
    std::fs::write(
        &hooks_config,
        r#"{"version":1,"enabled":["logging","audit"]}"#,
    )
    .unwrap();
    let paths = RuntimePaths {
        hooks_config,
        audit_log: audit_log.clone(),
        notes_dir: temp.path().join("notes"),
        nudges_path: temp.path().join("nudges.json"),
        system_skills_dir: temp.path().join("system-skills"),
        user_skills_dir: temp.path().join("user-skills"),
        system_skills_origin: crate::agent::skills::loader::SkillOrigin::Local,
        curation_log: temp.path().join("curation_log.json"),
    };

    let deps = RuntimeDeps::load(&paths, None);

    assert_eq!(deps.hooks().names(), vec!["logging", "audit"]);
    assert_eq!(deps.paths().unwrap().audit_log, audit_log);
    assert!(
        !audit_log.exists(),
        "constructing dependencies must not write audit"
    );
}

#[tokio::test]
async fn fixed_clock_reaches_hook_context_and_turn_latency() {
    struct ClockSpy {
        started_at: Arc<AtomicU64>,
        latency: Arc<AtomicU64>,
    }

    impl crate::agent::runtime::hooks::Hook for ClockSpy {
        fn name(&self) -> &str {
            "clock-spy"
        }

        fn pre_turn(
            &self,
            context: &crate::agent::runtime::hooks::HookContext,
        ) -> crate::agent::runtime::hooks::HookOutcome {
            self.started_at
                .store(context.started_at_ms, Ordering::SeqCst);
            crate::agent::runtime::hooks::HookOutcome::Continue
        }

        fn post_turn(
            &self,
            _context: &crate::agent::runtime::hooks::HookContext,
            summary: &crate::agent::runtime::hooks::TurnSummary,
        ) -> crate::agent::runtime::hooks::HookOutcome {
            self.latency.store(summary.latency_ms, Ordering::SeqCst);
            crate::agent::runtime::hooks::HookOutcome::Continue
        }
    }

    let started_at = Arc::new(AtomicU64::new(0));
    let latency = Arc::new(AtomicU64::new(u64::MAX));
    let hooks = HookRegistry::new();
    hooks.register(Arc::new(ClockSpy {
        started_at: Arc::clone(&started_at),
        latency: Arc::clone(&latency),
    }));
    let deps = RuntimeDeps::new(hooks, Arc::new(FixedClock(42_000)), None);
    let config = crate::config::AgentConfig {
        provider: "mock".into(),
        model: "mock-model".into(),
        ..Default::default()
    };
    let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(
        crate::agent::llm::providers::mock::MockProvider::new(&config.model, &config),
    );
    let tools = crate::agent::tools::registry::builtin_only_registry();
    let request =
        crate::agent::runtime::loop_::RuntimeRequest::buffered(provider, &config, "clock", &tools);

    crate::agent::runtime::loop_::run_with_deps(&deps, request)
        .await
        .unwrap();

    assert_eq!(started_at.load(Ordering::SeqCst), 42_000);
    assert_eq!(latency.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn request_local_registry_keeps_worker_audit_and_late_extension_observers() {
    struct TurnSpy {
        name: &'static str,
        calls: Arc<AtomicU64>,
    }

    impl crate::agent::runtime::hooks::Hook for TurnSpy {
        fn name(&self) -> &str {
            self.name
        }

        fn pre_turn(
            &self,
            _context: &crate::agent::runtime::hooks::HookContext,
        ) -> crate::agent::runtime::hooks::HookOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            crate::agent::runtime::hooks::HookOutcome::Continue
        }
    }

    let worker_audit_calls = Arc::new(AtomicU64::new(0));
    let extension_calls = Arc::new(AtomicU64::new(0));
    let hooks = HookRegistry::new();
    hooks.register(Arc::new(TurnSpy {
        name: "worker-audit",
        calls: Arc::clone(&worker_audit_calls),
    }));
    let deps = RuntimeDeps::new(hooks.clone(), Arc::new(FixedClock(42_000)), None);
    hooks.register(Arc::new(TurnSpy {
        name: "agent-extension-observer",
        calls: Arc::clone(&extension_calls),
    }));

    let config = crate::config::AgentConfig {
        provider: "mock".into(),
        model: "mock-model".into(),
        ..Default::default()
    };
    let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(
        crate::agent::llm::providers::mock::MockProvider::new(&config.model, &config),
    );
    let tools = crate::agent::tools::registry::builtin_only_registry();
    let request =
        crate::agent::runtime::loop_::RuntimeRequest::buffered(provider, &config, "hooks", &tools);

    crate::agent::runtime::loop_::run_with_deps(&deps, request)
        .await
        .unwrap();

    assert_eq!(worker_audit_calls.load(Ordering::SeqCst), 1);
    assert_eq!(extension_calls.load(Ordering::SeqCst), 1);
}
