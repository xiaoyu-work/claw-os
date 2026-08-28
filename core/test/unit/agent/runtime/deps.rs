use super::*;

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
    };

    let deps = RuntimeDeps::load(&paths, None);

    assert_eq!(deps.hooks().names(), vec!["logging", "audit"]);
    assert_eq!(deps.paths().unwrap().audit_log, audit_log);
    assert!(
        !audit_log.exists(),
        "constructing dependencies must not write audit"
    );
}
