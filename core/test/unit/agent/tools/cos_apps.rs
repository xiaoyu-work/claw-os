use super::*;

/// Build a small tempdir holding two synthetic apps so the
/// dynamic registration path can be exercised without depending
/// on what's installed on the host.
fn write_two_demo_apps() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let fs_dir = root.path().join("fs");
    std::fs::create_dir_all(&fs_dir).unwrap();
    std::fs::write(
        fs_dir.join("app.json"),
        serde_json::json!({
            "id": "fs",
            "version": "0.1.0",
            "name": {"en": "Files"},
            "summary": {"en": "Agent-native file system."},
            "operations": {
                "ls":   {"label": {"en": "List files"}, "args": [], "needs": []},
                "read": {"label": {"en": "Read a file"}, "args": [], "needs": []}
            }
        })
        .to_string(),
    )
    .unwrap();
    let notify_dir = root.path().join("notify");
    std::fs::create_dir_all(&notify_dir).unwrap();
    std::fs::write(
        notify_dir.join("app.json"),
        serde_json::json!({
            "id": "notify",
            "version": "0.1.0",
            "name": {"en": "Notifications"},
            "summary": {"en": "Send desktop notifications."},
            "operations": {
                "send": {"label": {"en": "Send a notification"}, "args": [], "needs": []}
            }
        })
        .to_string(),
    )
    .unwrap();
    root
}

/// Serialised env mutation: several tests set $COS_APPS_DIR in
/// parallel — share one lock so they don't fight.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn with_apps_dir<R>(tmp: &tempfile::TempDir, f: impl FnOnce() -> R) -> R {
    let prev = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_APPS_DIR", tmp.path());
    let r = f();
    match prev {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    r
}

#[test]
fn register_all_picks_up_every_manifest_on_disk() {
    let _g = env_lock();
    let tmp = write_two_demo_apps();
    with_apps_dir(&tmp, || {
        let mut r = ToolRegistry::new();
        register_all(&mut r);
        // 2 typed proxies + catalog + run.
        assert_eq!(r.len(), 4);
        assert!(r.get("cos_app_fs").is_some());
        assert!(r.get("cos_app_notify").is_some());
        assert!(r.get("cos_app_catalog").is_some());
        assert!(r.get("cos_app_run").is_some());
    });
}

#[test]
fn register_default_exposes_only_progressive_app_gateways() {
    let _g = env_lock();
    let tmp = write_two_demo_apps();
    with_apps_dir(&tmp, || {
        let mut registry = ToolRegistry::new();
        register_default(&mut registry);

        assert_eq!(registry.len(), 2);
        assert!(registry.get("cos_app_catalog").is_some());
        assert!(registry.get("cos_app_run").is_some());
        assert!(registry.get("cos_app_fs").is_none());
        assert!(registry.get("cos_app_notify").is_none());
    });
}

#[test]
fn rebuilt_registry_uses_fresh_owned_manifest_metadata() {
    let _g = env_lock();
    let tmp = write_two_demo_apps();
    with_apps_dir(&tmp, || {
        let mut first = ToolRegistry::new();
        register_all(&mut first);
        let first_tool = first.get("cos_app_fs").expect("first fs tool");
        assert!(first_tool
            .description()
            .contains("Agent-native file system"));

        let manifest_path = tmp.path().join("fs").join("app.json");
        let mut manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["summary"]["en"] = Value::String("Reloaded file system.".into());
        manifest["operations"]["stat"] = serde_json::json!({
            "label": {"en": "Inspect a file"},
            "args": [],
            "needs": []
        });
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();

        let mut reloaded = ToolRegistry::new();
        register_all(&mut reloaded);
        let reloaded_tool = reloaded.get("cos_app_fs").expect("reloaded fs tool");
        assert!(reloaded_tool
            .description()
            .contains("Reloaded file system"));
        assert!(reloaded_tool
            .input_schema()
            .pointer("/properties/command/enum")
            .and_then(Value::as_array)
            .expect("command enum")
            .iter()
            .any(|command| command.as_str() == Some("stat")));

        assert!(first_tool
            .description()
            .contains("Agent-native file system"));
    });
}

#[test]
fn register_all_yields_no_typed_proxies_when_apps_dir_empty() {
    let _g = env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let prev = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_APPS_DIR", tmp.path());
    let mut r = ToolRegistry::new();
    register_all(&mut r);
    match prev {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    // Only catalog + run survive when no manifests exist.
    assert_eq!(r.len(), 2);
    assert!(r.get("cos_app_catalog").is_some());
    assert!(r.get("cos_app_run").is_some());
}

#[test]
fn manifest_drives_command_enum() {
    let _g = env_lock();
    let tmp = write_two_demo_apps();
    with_apps_dir(&tmp, || {
        let mut r = ToolRegistry::new();
        register_all(&mut r);
        let tool = r.get("cos_app_fs").expect("fs must be registered");
        let schema = tool.input_schema();
        let enum_vals = schema
            .pointer("/properties/command/enum")
            .and_then(Value::as_array)
            .expect("enum must be present");
        let names: std::collections::HashSet<&str> =
            enum_vals.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains("ls"), "got {names:?}");
        assert!(names.contains("read"), "got {names:?}");
        assert_eq!(names.len(), 2, "extra commands appeared: {names:?}");
    });
}

#[test]
fn description_includes_name_summary_and_verb_labels() {
    let _g = env_lock();
    let tmp = write_two_demo_apps();
    with_apps_dir(&tmp, || {
        let mut r = ToolRegistry::new();
        register_all(&mut r);
        let tool = r.get("cos_app_fs").unwrap();
        let desc = tool.description();
        assert!(desc.contains("Files"), "want app name in description: {desc}");
        assert!(
            desc.contains("Agent-native file system"),
            "want summary in description: {desc}"
        );
        assert!(
            desc.contains("List files") && desc.contains("Read a file"),
            "want verb labels in description: {desc}"
        );
    });
}

#[test]
fn registered_tool_names_are_unique_and_prefixed() {
    let _g = env_lock();
    let tmp = write_two_demo_apps();
    with_apps_dir(&tmp, || {
        let mut r = ToolRegistry::new();
        register_all(&mut r);
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for name in r.names() {
            if let Some(rest) = name.strip_prefix(NAME_PREFIX) {
                assert!(seen.insert(name), "duplicate {name}");
                assert!(!rest.is_empty(), "tool {name} has empty suffix");
            }
        }
        assert_eq!(NAME_PREFIX, "cos_app_");
    });
}

#[tokio::test]
async fn missing_command_field_is_returned_as_tool_error() {
    let tool = CosAppTool::new("cos_app_fs", "fs", "test", &["ls"]);
    let result = tool.exec(json!({ "args": ["whatever"] })).await;
    assert!(result.is_error);
    assert!(result.content.contains("missing 'command'"));
}

#[tokio::test]
async fn unknown_command_propagates_app_error() {
    // Pick a command the schema says exists; a non-existent app
    // dir will surface as a bridge error so we exercise the
    // error-pass-through path without depending on python in
    // CI.
    let tool = CosAppTool::new("cos_app_fs", "definitely-not-an-app", "test", &["ls"]);
    // Force an apps dir that doesn't contain the app.
    let prev = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_APPS_DIR", std::env::temp_dir());
    let result = tool.exec(json!({ "command": "ls", "args": [] })).await;
    match prev {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    assert!(result.is_error, "expected error for missing app");
}

#[tokio::test]
async fn strict_mode_without_session_denies_invocation() {
    // With strict perms and no session, the capability gate must
    // refuse before we ever reach the bridge. Other tests set
    // env in parallel; we serialise via a process-wide lock.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let prev_mode = std::env::var("COS_PERMS_MODE").ok();
    let prev_session = std::env::var("COS_SESSION").ok();
    std::env::set_var("COS_PERMS_MODE", "strict");
    std::env::remove_var("COS_SESSION");

    let tool = CosAppTool::new("cos_app_fs", "fs", "test", &["ls"]);
    let result = tool.exec(json!({ "command": "ls", "args": [] })).await;

    match prev_mode {
        Some(v) => std::env::set_var("COS_PERMS_MODE", v),
        None => std::env::remove_var("COS_PERMS_MODE"),
    }
    if let Some(v) = prev_session {
        std::env::set_var("COS_SESSION", v);
    }

    assert!(result.is_error, "expected denial in strict mode");
    // Summary always names the verb that was denied.
    assert!(
        result.content.contains("agent.invoke"),
        "denial summary should mention the verb, got: {}",
        result.content
    );
}

#[tokio::test]
async fn schema_introspection_bypasses_capability_gate() {
    // `__schema__` is the introspection escape hatch — the agent
    // registry must be able to describe an app it cannot run.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let prev_mode = std::env::var("COS_PERMS_MODE").ok();
    let prev_apps = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_PERMS_MODE", "strict");
    // Force a missing app dir so the bridge errors rather than
    // launching python — we only care that we got *past* the gate.
    std::env::set_var("COS_APPS_DIR", std::env::temp_dir());

    let tool = CosAppTool::new("cos_app_fs", "fs", "test", &["ls"]);
    let result = tool.exec(json!({ "command": "__schema__", "args": [] })).await;

    match prev_mode {
        Some(v) => std::env::set_var("COS_PERMS_MODE", v),
        None => std::env::remove_var("COS_PERMS_MODE"),
    }
    match prev_apps {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    // The bridge will error (no python app installed under temp),
    // but the error must NOT be a capability denial — the schema
    // path is supposed to skip the gate entirely.
    if result.is_error {
        assert!(
            !result.content.contains("agent.invoke"),
            "__schema__ should bypass the capability gate, got: {}",
            result.content
        );
    }
}

#[test]
fn name_prefix_constant() {
    assert_eq!(NAME_PREFIX, "cos_app_");
}

// ----- catalog + run --------------------------------------------------

fn write_demo_apps_dir() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let demo_dir = root.path().join("demo");
    std::fs::create_dir_all(&demo_dir).unwrap();
    let manifest = serde_json::json!({
        "id": "demo",
        "version": "0.1.0",
        "name": {"en": "Demo App"},
        "summary": {"en": "Toy app used by catalog tests."},
        "operations": {
            "ping": {
                "label": {"en": "Ping"},
                "summary": {"en": "Echo a fixed reply."},
                "args": [],
                "needs": []
            }
        }
    });
    std::fs::write(demo_dir.join("app.json"), manifest.to_string()).unwrap();
    root
}

#[tokio::test]
async fn catalog_list_includes_installed_apps() {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let tmp = write_demo_apps_dir();
    let prev_apps = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_APPS_DIR", tmp.path());

    let tool = CosAppCatalog;
    let result = tool.exec(json!({ "command": "list" })).await;

    match prev_apps {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert!(!result.is_error, "catalog list unexpectedly errored: {}", result.content);
    assert!(result.content.contains("demo"), "expected demo app in list, got: {}", result.content);
    assert!(
        result.content.contains("Toy app"),
        "summary should appear, got: {}",
        result.content
    );
}

#[tokio::test]
async fn catalog_search_matches_on_label() {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let tmp = write_demo_apps_dir();
    let prev_apps = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_APPS_DIR", tmp.path());

    let tool = CosAppCatalog;
    let hit = tool.exec(json!({ "command": "search", "args": ["ping"] })).await;
    let miss = tool.exec(json!({ "command": "search", "args": ["zzzz_no_match"] })).await;

    match prev_apps {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert!(!hit.is_error);
    assert!(hit.content.contains("demo"), "expected hit on label 'Ping'");
    assert!(!miss.is_error);
    assert!(miss.content.contains("no apps match"));
}

#[tokio::test]
async fn catalog_show_dumps_operation_detail() {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let tmp = write_demo_apps_dir();
    let prev_apps = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_APPS_DIR", tmp.path());

    let tool = CosAppCatalog;
    let result = tool.exec(json!({ "command": "show", "args": ["demo"] })).await;
    let missing = tool.exec(json!({ "command": "show", "args": ["ghost"] })).await;

    match prev_apps {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert!(!result.is_error, "show errored: {}", result.content);
    assert!(result.content.contains("ping"));
    assert!(result.content.contains("Ping"));
    assert!(missing.is_error);
}

#[tokio::test]
async fn catalog_bypasses_capability_gate() {
    // Catalog must work in strict mode without a session, because
    // it's a read-only manifest inspection — the entire point is
    // for the agent to discover what *could* be invoked.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let tmp = write_demo_apps_dir();
    let prev_mode = std::env::var("COS_PERMS_MODE").ok();
    let prev_session = std::env::var("COS_SESSION").ok();
    let prev_apps = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_PERMS_MODE", "strict");
    std::env::remove_var("COS_SESSION");
    std::env::set_var("COS_APPS_DIR", tmp.path());

    let tool = CosAppCatalog;
    let result = tool.exec(json!({ "command": "list" })).await;

    match prev_mode {
        Some(v) => std::env::set_var("COS_PERMS_MODE", v),
        None => std::env::remove_var("COS_PERMS_MODE"),
    }
    if let Some(v) = prev_session {
        std::env::set_var("COS_SESSION", v);
    }
    match prev_apps {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert!(!result.is_error, "catalog must bypass caps; got: {}", result.content);
}

#[tokio::test]
async fn run_rejects_unknown_app() {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let prev_apps = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_APPS_DIR", std::env::temp_dir());

    let tool = CosAppRun;
    let result = tool
        .exec(json!({ "app": "definitely-not-installed-xyz", "command": "ls" }))
        .await;

    match prev_apps {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert!(result.is_error);
    assert!(
        result.content.contains("no app named")
            || result.content.contains("definitely-not-installed-xyz"),
        "expected unknown-app error, got: {}",
        result.content
    );
}

#[tokio::test]
async fn run_rejects_invalid_app_id() {
    let tool = CosAppRun;
    let result = tool
        .exec(json!({ "app": "Bad/App!", "command": "ls" }))
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("invalid app id"));
}

#[tokio::test]
async fn run_requires_app_and_command_fields() {
    let tool = CosAppRun;
    let missing_app = tool.exec(json!({ "command": "ls" })).await;
    let missing_cmd = tool.exec(json!({ "app": "fs" })).await;
    assert!(missing_app.is_error);
    assert!(missing_cmd.is_error);
    assert!(missing_app.content.contains("missing 'app'"));
    assert!(missing_cmd.content.contains("missing 'command'"));
}

#[test]
fn is_valid_app_id_accepts_canonical_and_rejects_garbage() {
    assert!(is_valid_app_id("fs"));
    assert!(is_valid_app_id("a"));
    assert!(is_valid_app_id("my-app_2"));
    assert!(!is_valid_app_id(""));
    assert!(!is_valid_app_id("0name"));
    assert!(!is_valid_app_id("Cap"));
    assert!(!is_valid_app_id("with space"));
    assert!(!is_valid_app_id("../etc"));
}
#[tokio::test]
async fn generic_catalog_and_run_use_the_injected_app_root() {
    let _guard = env_lock();
    let injected = write_two_demo_apps();
    let ambient = tempfile::tempdir().unwrap();
    let _apps = crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", ambient.path());
    let mut registry = ToolRegistry::new();
    register_default_with_root(&mut registry, injected.path().to_path_buf());

    let catalog = registry.get("cos_app_catalog").unwrap();
    let listed = catalog
        .exec(serde_json::json!({"command":"list","args":[]}))
        .await;
    assert!(!listed.is_error);
    assert!(listed.content.contains("fs"));

    let run = registry.get("cos_app_run").unwrap();
    let schema = run
        .exec(serde_json::json!({
            "app":"fs",
            "command":"__schema__",
            "args":[]
        }))
        .await;
    assert!(!schema.is_error, "schema failed: {}", schema.content);
    assert!(crate::apps::find(ambient.path(), "fs").is_none());
}
