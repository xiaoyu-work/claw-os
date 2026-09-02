use super::*;
use crate::agent::run;

// -----------------------------------------------------------------
// learn (memory curator CLI)
// -----------------------------------------------------------------

/// Pin the curator default log under a per-test temp dir so we
/// don't trample the real machine's `%ProgramData%\cos\` state.
/// Returns a guard that holds the crate-wide env lock for the
/// test's lifetime: each call mutates `COS_DATA_DIR`, and two
/// tests running in parallel would otherwise observe each
/// other's data directory (cargo test runs many threads).
/// The guard derefs to `&Path` so existing `dir.join(...)`
/// callers keep working without changes.
struct LearnDataDir {
    path: std::path::PathBuf,
    _env: std::sync::MutexGuard<'static, ()>,
}

impl std::ops::Deref for LearnDataDir {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl LearnDataDir {
    fn join(&self, p: impl AsRef<std::path::Path>) -> std::path::PathBuf {
        self.path.join(p)
    }
}

fn isolate_cos_data_dir(tag: &str) -> LearnDataDir {
    let env = crate::test_env::lock_env();
    let dir = std::env::temp_dir().join(format!(
        "cos-learn-cli-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("COS_DATA_DIR", &dir);
    LearnDataDir {
        path: dir,
        _env: env,
    }
}

// ---- context_cmd dispatch ----

// ---- context hints ----

#[test]
fn context_hints_invalid_cwd_errs() {
    let err =
        context_hints_cmd(&["--cwd".into(), "Z:\\definitely\\not\\there".into()]).unwrap_err();
    assert!(err.contains("not a directory"));
}

#[test]
fn context_hints_finds_real_markers_in_temp_dir() {
    let dir = std::env::temp_dir().join(format!(
        "cos-context-hints-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    let v = context_hints_cmd(&["--cwd".into(), dir.to_string_lossy().to_string()]).expect("ok");
    assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(1));
    let hints = v.get("hints").and_then(|h| h.as_array()).unwrap();
    assert!(hints
        .iter()
        .any(|h| h.get("label").and_then(|s| s.as_str()) == Some("Rust crate")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn context_hints_render_returns_summary_paragraph() {
    let dir = std::env::temp_dir().join(format!(
        "cos-context-hints-render-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("package.json"), "{}").unwrap();
    let v = context_hints_cmd(&[
        "--cwd".into(),
        dir.to_string_lossy().to_string(),
        "--render".into(),
    ])
    .expect("ok");
    let s = v.get("summary").and_then(|s| s.as_str()).unwrap_or("");
    assert!(s.contains("Project hints"));
    assert!(s.contains("Node.js project"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn context_hints_recursive_with_depth() {
    let dir = std::env::temp_dir().join(format!(
        "cos-context-hints-deep-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let nested = dir.join("apps").join("web");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("package.json"), "{}").unwrap();
    // Depth 0 → no recursion → no hits.
    let v0 = context_hints_cmd(&[
        "--cwd".into(),
        dir.to_string_lossy().to_string(),
        "--depth".into(),
        "0".into(),
    ])
    .expect("ok");
    assert_eq!(v0.get("count").and_then(|n| n.as_u64()), Some(0));
    // Depth 3 → recursive walk → finds the nested manifest.
    let v3 = context_hints_cmd(&[
        "--cwd".into(),
        dir.to_string_lossy().to_string(),
        "--depth".into(),
        "3".into(),
    ])
    .expect("ok");
    assert_eq!(v3.get("count").and_then(|n| n.as_u64()), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- context refs ----

#[test]
fn context_refs_requires_text() {
    let err = context_refs_cmd(&[]).unwrap_err();
    assert!(err.contains("--text"));
}

#[test]
fn context_refs_extracts_paths_and_urls() {
    let v = context_refs_cmd(&[
        "--text".into(),
        "see @notes.md and @https://example.com/x".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(2));
    let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
    assert_eq!(refs[0].get("kind").and_then(|s| s.as_str()), Some("Path"));
    assert_eq!(
        refs[0].get("raw").and_then(|s| s.as_str()),
        Some("notes.md")
    );
    assert_eq!(refs[1].get("kind").and_then(|s| s.as_str()), Some("Url"));
}

#[test]
fn context_refs_unique_dedupes() {
    let v = context_refs_cmd(&["--text".into(), "@a @a @a".into(), "--unique".into()]).expect("ok");
    assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(1));
    assert_eq!(v.get("unique").and_then(|b| b.as_bool()), Some(true));
}

// ---- context markers ----

#[test]
fn context_markers_dumps_table() {
    let v = context_markers_cmd(&[]).expect("ok");
    let total = v.get("total").and_then(|n| n.as_u64()).unwrap();
    assert!(total >= 30);
    let by_kind = v.get("by_kind").and_then(|x| x.as_object()).unwrap();
    let manifests = by_kind.get("Manifest").and_then(|x| x.as_array()).unwrap();
    let names: Vec<&str> = manifests.iter().filter_map(|s| s.as_str()).collect();
    assert!(names.contains(&"Cargo.toml"));
    assert!(names.contains(&"package.json"));
    assert!(names.contains(&"go.mod"));
}

// ---- context build (engine) ----

#[test]
fn context_build_no_args_returns_empty_block() {
    let v = context_cmd(&["build".into()]).expect("ok");
    assert_eq!(v.get("is_empty").and_then(|b| b.as_bool()), Some(true));
    assert!(v.get("rendered").map(|x| x.is_null()).unwrap_or(false));
}

#[test]
fn context_build_invalid_cwd_errs() {
    let err = context_cmd(&[
        "build".into(),
        "--cwd".into(),
        "Z:\\definitely\\not\\there".into(),
    ])
    .unwrap_err();
    assert!(err.contains("not a directory"));
}

#[test]
fn context_build_invalid_depth_errs() {
    let err = context_cmd(&["build".into(), "--depth".into(), "abc".into()]).unwrap_err();
    assert!(err.contains("--depth"));
}

#[test]
fn context_build_with_text_extracts_references() {
    let v =
        context_cmd(&["build".into(), "--text".into(), "look at @notes.md".into()]).expect("ok");
    assert_eq!(v.get("is_empty").and_then(|b| b.as_bool()), Some(false));
    let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
    assert_eq!(refs.len(), 1);
    let rendered = v.get("rendered").and_then(|s| s.as_str()).unwrap_or("");
    assert!(rendered.contains("PROJECT_CONTEXT"));
    assert!(rendered.contains("notes.md"));
}

#[test]
fn context_build_with_cwd_picks_up_hints() {
    let dir = std::env::temp_dir().join(format!(
        "cos-context-build-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    let v = context_cmd(&[
        "build".into(),
        "--cwd".into(),
        dir.to_string_lossy().to_string(),
    ])
    .expect("ok");
    let hints = v.get("hints").and_then(|x| x.as_array()).unwrap();
    assert_eq!(hints.len(), 1);
    assert_eq!(
        hints[0].get("label").and_then(|s| s.as_str()),
        Some("Rust crate")
    );
    let rendered = v.get("rendered").and_then(|s| s.as_str()).unwrap_or("");
    assert!(rendered.contains("Project hints"));
    assert!(rendered.contains("cwd:"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn context_build_with_notes_appends_them() {
    let v = context_cmd(&[
        "build".into(),
        "--note".into(),
        "host: Windows".into(),
        "--note".into(),
        "12 MB free".into(),
    ])
    .expect("ok");
    let notes = v.get("notes").and_then(|x| x.as_array()).unwrap();
    assert_eq!(notes.len(), 2);
    let rendered = v.get("rendered").and_then(|s| s.as_str()).unwrap_or("");
    assert!(rendered.contains("Notes:"));
    assert!(rendered.contains("host: Windows"));
    assert!(rendered.contains("12 MB free"));
}

#[test]
fn context_build_max_refs_caps_count() {
    let v = context_cmd(&[
        "build".into(),
        "--text".into(),
        "@a @b @c @d @e".into(),
        "--max-refs".into(),
        "2".into(),
    ])
    .expect("ok");
    let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
    assert_eq!(refs.len(), 2);
}

#[test]
fn context_build_no_dedup_keeps_duplicates() {
    let v = context_cmd(&[
        "build".into(),
        "--text".into(),
        "@a @a @a".into(),
        "--no-dedup".into(),
    ])
    .expect("ok");
    let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
    assert_eq!(refs.len(), 3);
}

// -----------------------------------------------------------------
// interrupt_cmd
// -----------------------------------------------------------------

#[test]
fn interrupt_cmd_default_errs_with_usage() {
    let err = interrupt_cmd(&[]).unwrap_err();
    assert!(err.contains("interrupt"), "got {err}");
    assert!(err.contains("list"), "got {err}");
    assert!(err.contains("signal"), "got {err}");
}

#[test]
fn interrupt_cmd_list_returns_active_sessions() {
    let id = format!("cli-list-{}", uuid::Uuid::new_v4().simple());
    let _h = crate::agent::runtime::interrupt::register(&id);
    let v = interrupt_cmd(&["list".into()]).expect("list ok");
    let arr = v["sessions"].as_array().expect("sessions array");
    let ids: Vec<&str> = arr.iter().filter_map(|s| s.as_str()).collect();
    assert!(ids.contains(&id.as_str()), "list missing {id}: {arr:?}");
    assert!(v["count"].as_u64().unwrap() >= 1);
}

#[test]
fn interrupt_cmd_signal_unknown_session_reports_not_registered() {
    let id = format!("cli-unknown-{}", uuid::Uuid::new_v4().simple());
    let v = interrupt_cmd(&["signal".into(), id.clone()]).expect("ok");
    assert_eq!(v["signaled"], serde_json::Value::Bool(false));
    assert_eq!(v["session_id"].as_str().unwrap(), id);
    assert!(v["reason"].as_str().unwrap().contains("not registered"));
}

#[test]
fn interrupt_cmd_signal_active_session_returns_signaled_true() {
    let id = format!("cli-signal-{}", uuid::Uuid::new_v4().simple());
    let h = crate::agent::runtime::interrupt::register(&id);
    let v = interrupt_cmd(&["signal".into(), id.clone()]).expect("ok");
    assert_eq!(v["signaled"], serde_json::Value::Bool(true));
    assert_eq!(v["session_id"].as_str().unwrap(), id);
    // Signal really took effect.
    assert!(h.check());
}

#[test]
fn interrupt_cmd_signal_requires_session_id() {
    let err = interrupt_cmd(&["signal".into()]).unwrap_err();
    assert!(err.contains("usage"), "got {err}");
}

#[test]
fn run_interrupt_routes_to_interrupt_cmd() {
    // Confirm the agent dispatcher reaches interrupt_cmd.
    let err = run("interrupt", &["frobnicate".into()]).unwrap_err();
    assert!(err.contains("unknown"), "got {err}");
}

// -----------------------------------------------------------------
// hooks (runtime hook registry CLI)
// -----------------------------------------------------------------

#[test]
fn hooks_cmd_list_default_returns_count() {
    let _dir = isolate_cos_data_dir("hooks-list-default");
    let v = hooks_cmd(&[]).expect("ok");
    assert!(v.get("hooks").is_some(), "got {v}");
    assert!(v.get("count").is_some(), "got {v}");
    assert!(v["count"].is_number(), "got {v}");
    assert!(v["persistent"].is_array(), "got {v}");
    assert!(v["config_path"].is_string(), "got {v}");
}

#[test]
fn hooks_cmd_list_after_register_includes_name() {
    use crate::agent::runtime::hooks::{global_registry, Hook, HookContext, HookOutcome};
    let _dir = isolate_cos_data_dir("hooks-list-after-register");
    struct TestHook;
    impl Hook for TestHook {
        fn name(&self) -> &str {
            "cli-test-hook"
        }
        fn pre_turn(&self, _ctx: &HookContext) -> HookOutcome {
            HookOutcome::Continue
        }
    }
    let registry = global_registry();
    registry.register(std::sync::Arc::new(TestHook));
    let v = hooks_cmd(&["list".into()]).expect("ok");
    let names = v["hooks"].as_array().unwrap();
    assert!(
        names.iter().any(|n| n.as_str() == Some("cli-test-hook")),
        "got {v}"
    );
    // Cleanup so we don't leak the registration into other tests.
    registry.unregister("cli-test-hook");
}

#[test]
fn run_hooks_routes_to_hooks_cmd() {
    let _dir = isolate_cos_data_dir("hooks-route");
    let v = run("dev", &["hooks".into(), "list".into()]).expect("ok");
    assert!(v.get("count").is_some(), "got {v}");
}

#[test]
fn hooks_cmd_enable_persists_kind_and_registers_in_process() {
    use crate::agent::runtime::hooks::global_registry;
    use crate::agent::runtime::hooks_config;
    let _dir = isolate_cos_data_dir("hooks-enable");
    // make sure no leftover registration from a prior test
    global_registry().unregister("logging");

    let v = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
    assert_eq!(v["kind"], serde_json::json!("logging"));
    assert_eq!(v["persisted"], serde_json::json!(true));
    assert_eq!(v["registered_now"], serde_json::json!(true));

    // file exists with logging in enabled list
    let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).expect("load");
    assert_eq!(cfg.enabled, vec![hooks_config::HookKind::Logging]);

    // hook actually registered
    assert!(global_registry().names().contains(&"logging".to_string()));

    // cleanup
    global_registry().unregister("logging");
}

#[test]
fn hooks_cmd_enable_idempotent_second_call_is_noop() {
    use crate::agent::runtime::hooks::global_registry;
    let _dir = isolate_cos_data_dir("hooks-enable-idempotent");
    global_registry().unregister("logging");

    let _ = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
    let v = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
    assert_eq!(v["persisted"], serde_json::json!(false));
    assert_eq!(v["registered_now"], serde_json::json!(false));

    global_registry().unregister("logging");
}

#[test]
fn hooks_cmd_enable_accepts_kind_flag_form() {
    use crate::agent::runtime::hooks::global_registry;
    let _dir = isolate_cos_data_dir("hooks-enable-flag");
    global_registry().unregister("logging");

    let v = hooks_cmd(&["enable".into(), "--kind".into(), "logging".into()]).expect("ok");
    assert_eq!(v["kind"], serde_json::json!("logging"));

    global_registry().unregister("logging");
}

#[test]
fn hooks_cmd_enable_unknown_kind_errs() {
    let _dir = isolate_cos_data_dir("hooks-enable-unknown");
    let err = hooks_cmd(&["enable".into(), "frobnicate".into()]).unwrap_err();
    assert!(err.contains("unknown hook kind"), "got {err}");
}

#[test]
fn hooks_cmd_enable_missing_kind_errs() {
    let _dir = isolate_cos_data_dir("hooks-enable-missing");
    let err = hooks_cmd(&["enable".into()]).unwrap_err();
    assert!(err.contains("missing hook kind"), "got {err}");
}

#[test]
fn hooks_cmd_enable_checkpoint_kind_persists_and_registers() {
    use crate::agent::runtime::hooks::global_registry;
    use crate::agent::runtime::hooks_config;
    let _dir = isolate_cos_data_dir("hooks-enable-checkpoint");
    global_registry().unregister("checkpoint");

    let v = hooks_cmd(&["enable".into(), "checkpoint".into()]).expect("ok");
    assert_eq!(v["kind"], serde_json::json!("checkpoint"));
    assert_eq!(v["persisted"], serde_json::json!(true));
    assert_eq!(v["registered_now"], serde_json::json!(true));

    let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).expect("load");
    assert_eq!(cfg.enabled, vec![hooks_config::HookKind::Checkpoint]);
    assert!(global_registry()
        .names()
        .contains(&"checkpoint".to_string()));

    global_registry().unregister("checkpoint");
}

#[test]
fn hooks_cmd_disable_removes_from_config_and_registry() {
    use crate::agent::runtime::hooks::global_registry;
    use crate::agent::runtime::hooks_config;
    let _dir = isolate_cos_data_dir("hooks-disable");
    global_registry().unregister("logging");

    let _ = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
    let v = hooks_cmd(&["disable".into(), "logging".into()]).expect("ok");
    assert_eq!(v["persisted"], serde_json::json!(true));
    assert_eq!(v["unregistered_now"], serde_json::json!(true));

    let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).expect("load");
    assert!(cfg.enabled.is_empty());
    assert!(!global_registry().names().contains(&"logging".to_string()));
}

#[test]
fn hooks_cmd_disable_idempotent_when_not_enabled() {
    let _dir = isolate_cos_data_dir("hooks-disable-noop");
    let v = hooks_cmd(&["disable".into(), "logging".into()]).expect("ok");
    assert_eq!(v["persisted"], serde_json::json!(false));
    assert_eq!(v["unregistered_now"], serde_json::json!(false));
}

#[test]
fn hooks_cmd_list_includes_persistent_kinds() {
    use crate::agent::runtime::hooks::global_registry;
    let _dir = isolate_cos_data_dir("hooks-list-persistent");
    global_registry().unregister("logging");

    let _ = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
    let v = hooks_cmd(&["list".into()]).expect("ok");
    let pers = v["persistent"].as_array().unwrap();
    assert!(
        pers.iter().any(|x| x.as_str() == Some("logging")),
        "got {v}"
    );

    // cleanup
    let _ = hooks_cmd(&["disable".into(), "logging".into()]).expect("ok");
}
