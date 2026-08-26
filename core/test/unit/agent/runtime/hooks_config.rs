use super::*;
use crate::agent::runtime::hooks::HookRegistry;
use tempfile::TempDir;

fn tmpfile(name: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join(name);
    (dir, p)
}

// ---- HookKind ----

#[test]
fn hook_kind_parse_handles_canonical_and_aliases() {
    assert_eq!(HookKind::parse("logging"), Some(HookKind::Logging));
    assert_eq!(HookKind::parse("LOGGING"), Some(HookKind::Logging));
    assert_eq!(HookKind::parse("  log  "), Some(HookKind::Logging));
    assert_eq!(HookKind::parse("tracing"), Some(HookKind::Logging));
    assert_eq!(HookKind::parse("audit"), Some(HookKind::Audit));
    assert_eq!(HookKind::parse("AUDIT"), Some(HookKind::Audit));
    assert_eq!(HookKind::parse("audit_log"), Some(HookKind::Audit));
    assert_eq!(HookKind::parse("auditlog"), Some(HookKind::Audit));
    assert_eq!(HookKind::parse("checkpoint"), Some(HookKind::Checkpoint));
    assert_eq!(HookKind::parse("CHECKPOINT"), Some(HookKind::Checkpoint));
    assert_eq!(HookKind::parse("snapshot"), Some(HookKind::Checkpoint));
    assert_eq!(HookKind::parse("rollback"), Some(HookKind::Checkpoint));
    assert_eq!(HookKind::parse("nope"), None);
    assert_eq!(HookKind::parse(""), None);
}

#[test]
fn hook_kind_canonical_is_lowercase_snake_case() {
    assert_eq!(HookKind::Logging.canonical(), "logging");
    assert_eq!(HookKind::Audit.canonical(), "audit");
    assert_eq!(HookKind::Checkpoint.canonical(), "checkpoint");
}

#[test]
fn hook_kind_serializes_as_lowercase_string() {
    assert_eq!(
        serde_json::to_string(&HookKind::Logging).unwrap(),
        "\"logging\""
    );
    assert_eq!(
        serde_json::to_string(&HookKind::Audit).unwrap(),
        "\"audit\""
    );
    assert_eq!(
        serde_json::to_string(&HookKind::Checkpoint).unwrap(),
        "\"checkpoint\""
    );
    let back: HookKind = serde_json::from_str("\"audit\"").unwrap();
    assert_eq!(back, HookKind::Audit);
    let back: HookKind = serde_json::from_str("\"checkpoint\"").unwrap();
    assert_eq!(back, HookKind::Checkpoint);
}

// ---- HooksConfig ----

#[test]
fn default_config_has_version_one_and_empty_list() {
    let c = HooksConfig::default();
    assert_eq!(c.version, 1);
    assert!(c.enabled.is_empty());
}

#[test]
fn enable_is_idempotent() {
    let mut c = HooksConfig::default();
    assert!(c.enable(HookKind::Logging));
    assert!(!c.enable(HookKind::Logging));
    assert_eq!(c.enabled, vec![HookKind::Logging]);
}

#[test]
fn disable_returns_true_only_when_present() {
    let mut c = HooksConfig::default();
    assert!(!c.disable(HookKind::Logging));
    c.enable(HookKind::Logging);
    assert!(c.disable(HookKind::Logging));
    assert!(c.enabled.is_empty());
}

#[test]
fn is_enabled_reflects_state() {
    let mut c = HooksConfig::default();
    assert!(!c.is_enabled(HookKind::Logging));
    c.enable(HookKind::Logging);
    assert!(c.is_enabled(HookKind::Logging));
}

// ---- load / save ----

#[test]
fn load_returns_default_when_file_missing() {
    let (_dir, path) = tmpfile("hooks.json");
    let cfg = load(&path).expect("ok");
    assert_eq!(cfg, HooksConfig::default());
}

#[test]
fn save_then_load_round_trips() {
    let (_dir, path) = tmpfile("hooks.json");
    let mut cfg = HooksConfig::default();
    cfg.enable(HookKind::Logging);
    save(&path, &cfg).expect("save");
    let back = load(&path).expect("load");
    assert_eq!(back, cfg);
}

#[test]
fn save_creates_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent").join("nested").join("hooks.json");
    let cfg = HooksConfig::default();
    save(&path, &cfg).expect("save");
    assert!(path.exists());
}

#[test]
fn save_is_atomic_no_tmp_left_behind_on_success() {
    let (_dir, path) = tmpfile("hooks.json");
    let cfg = HooksConfig::default();
    save(&path, &cfg).expect("save");
    let tmp = tmp_path_for(&path);
    assert!(!tmp.exists(), "tmp should be renamed away");
}

#[test]
fn load_surfaces_malformed_json_as_invalid_data() {
    let (_dir, path) = tmpfile("hooks.json");
    std::fs::write(&path, "{not json").unwrap();
    let err = load(&path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn load_accepts_unknown_future_fields() {
    let (_dir, path) = tmpfile("hooks.json");
    std::fs::write(
        &path,
        r#"{"version":1,"enabled":["logging"],"future_field":"ignored"}"#,
    )
    .unwrap();
    let cfg = load(&path).expect("ok");
    assert_eq!(cfg.enabled, vec![HookKind::Logging]);
}

// ---- register_into ----

#[test]
fn register_into_skips_when_disabled_list_empty() {
    let reg = HookRegistry::new();
    let cfg = HooksConfig::default();
    let names = register_into(&reg, &cfg);
    assert!(names.is_empty());
    assert_eq!(reg.len(), 0);
}

#[test]
fn register_into_registers_logging_hook() {
    let reg = HookRegistry::new();
    let mut cfg = HooksConfig::default();
    cfg.enable(HookKind::Logging);
    let names = register_into(&reg, &cfg);
    assert_eq!(names, vec!["logging".to_string()]);
    assert!(reg.names().contains(&"logging".to_string()));
}

#[test]
fn register_into_registers_audit_hook() {
    let reg = HookRegistry::new();
    let mut cfg = HooksConfig::default();
    cfg.enable(HookKind::Audit);
    let names = register_into(&reg, &cfg);
    assert_eq!(names, vec!["audit".to_string()]);
    assert!(reg.names().contains(&"audit".to_string()));
}

#[test]
fn register_into_registers_checkpoint_hook() {
    let reg = HookRegistry::new();
    let mut cfg = HooksConfig::default();
    cfg.enable(HookKind::Checkpoint);
    let names = register_into(&reg, &cfg);
    assert_eq!(names, vec!["checkpoint".to_string()]);
    assert!(reg.names().contains(&"checkpoint".to_string()));
}

#[test]
fn register_into_registers_multiple_kinds_in_order() {
    let reg = HookRegistry::new();
    let mut cfg = HooksConfig::default();
    cfg.enable(HookKind::Audit);
    cfg.enable(HookKind::Logging);
    let names = register_into(&reg, &cfg);
    assert_eq!(names, vec!["audit".to_string(), "logging".to_string()]);
}

#[test]
fn register_into_skips_already_registered_names() {
    let reg = HookRegistry::new();
    reg.register(Arc::new(LoggingHook));
    let mut cfg = HooksConfig::default();
    cfg.enable(HookKind::Logging);
    let names = register_into(&reg, &cfg);
    assert!(
        names.is_empty(),
        "should NOT claim ownership of pre-existing hook"
    );
    assert_eq!(reg.len(), 1);
}

// ---- AutoHookGuard ----

#[test]
fn auto_guard_unregisters_on_drop() {
    let reg = HookRegistry::new();
    {
        let mut cfg = HooksConfig::default();
        cfg.enable(HookKind::Logging);
        let names = register_into(&reg, &cfg);
        assert!(reg.names().contains(&"logging".to_string()));
        let _g = AutoHookGuard::new(reg.clone(), names);
        assert_eq!(reg.len(), 1);
    }
    assert_eq!(reg.len(), 0, "drop should unregister");
}

#[test]
fn auto_guard_only_unregisters_owned_names() {
    let reg = HookRegistry::new();
    // Pre-existing hook NOT owned by the guard.
    reg.register(Arc::new(LoggingHook));
    {
        let _g = AutoHookGuard::new(reg.clone(), Vec::new());
    }
    assert_eq!(
        reg.len(),
        1,
        "guard with empty names list must not touch unrelated hooks"
    );
}

// ---- load_and_register ----

#[test]
fn load_and_register_no_op_when_file_missing() {
    let (_dir, path) = tmpfile("hooks.json");
    let reg = HookRegistry::new();
    let g = load_and_register(&path, reg.clone());
    assert!(g.names().is_empty());
    assert_eq!(reg.len(), 0);
}

#[test]
fn load_and_register_no_op_when_file_malformed() {
    let (_dir, path) = tmpfile("hooks.json");
    std::fs::write(&path, "{nonsense").unwrap();
    let reg = HookRegistry::new();
    let g = load_and_register(&path, reg.clone());
    assert!(
        g.names().is_empty(),
        "malformed config must not crash agent startup"
    );
    assert_eq!(reg.len(), 0);
}

#[test]
fn load_and_register_registers_then_drop_unregisters() {
    let (_dir, path) = tmpfile("hooks.json");
    let mut cfg = HooksConfig::default();
    cfg.enable(HookKind::Logging);
    save(&path, &cfg).unwrap();
    let reg = HookRegistry::new();
    {
        let g = load_and_register(&path, reg.clone());
        assert_eq!(g.names(), &["logging".to_string()]);
        assert!(reg.names().contains(&"logging".to_string()));
    }
    assert_eq!(reg.len(), 0);
}
