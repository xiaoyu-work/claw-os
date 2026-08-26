use super::*;

#[test]
fn lookup_finds_known_tools() {
    assert!(lookup("fs.read_text").is_some());
    assert!(lookup("fs.list").is_some());
    assert!(lookup("kv.get").is_some());
    assert!(lookup("does.not.exist").is_none());
}

#[test]
fn list_names_covers_catalog() {
    let names = list_names();
    assert!(names.contains(&"fs.read_text"));
    assert_eq!(names.len(), CATALOG.len());
}

#[test]
fn catalog_names_are_unique_and_sane() {
    let mut seen = std::collections::HashSet::new();
    for t in CATALOG {
        assert!(
            seen.insert(t.name),
            "duplicate tool name in catalog: {}",
            t.name
        );
        assert!(t.name.contains('.'), "tool names should be ns.name: {}", t.name);
        assert!(!t.summary.is_empty());
        assert!(!t.args_schema.is_empty());
        assert!(!t.returns_schema.is_empty());
    }
}

#[test]
fn execute_rejects_unknown_tool() {
    let err = execute("nope.nope", "app", &json!({})).unwrap_err();
    assert!(err.contains("unknown tool"), "got: {err}");
}

#[test]
fn execute_rejects_missing_required_arg() {
    let err = execute("fs.read_text", "app", &json!({})).unwrap_err();
    assert!(err.contains("path"), "got: {err}");
}

#[test]
fn schemas_parse_as_json() {
    for t in CATALOG {
        serde_json::from_str::<Value>(t.args_schema).expect(t.name);
        serde_json::from_str::<Value>(t.returns_schema).expect(t.name);
    }
}

#[test]
fn derive_scope_uses_path_for_fs_tools() {
    let tool = lookup("fs.read_text").unwrap();
    let scope = derive_scope(tool, &json!({"path": "/tmp/x"})).unwrap();
    assert!(matches!(scope, Scope::Path(p) if p == "/tmp/x"));
}

#[test]
fn derive_scope_uses_name_for_kv_tools() {
    let tool = lookup("kv.get").unwrap();
    let scope = derive_scope(tool, &json!({"key": "user_pref"})).unwrap();
    assert!(matches!(scope, Scope::Name(n) if n == "user_pref"));
}

#[test]
fn sanitize_key_replaces_special_chars() {
    // sanitize_key now anchors the on-disk name with a 16-hex
    // SHA-256 prefix so visually-similar keys ("a/b" vs "a:b")
    // get distinct files. The human-readable suffix only
    // contains [A-Za-z0-9_-] and is informational. We check both
    // shape and uniqueness.
    let a = sanitize_key("a-b_c");
    let b = sanitize_key("a/b");
    let c = sanitize_key("../etc/passwd");
    // 16 hex chars + `.` + suffix (or just 16 hex chars when the
    // suffix is empty).
    for k in [&a, &b, &c] {
        let prefix: String = k.chars().take(16).collect();
        assert_eq!(prefix.len(), 16);
        assert!(prefix.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(
            k.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'),
            "unexpected non-alphanumeric byte in sanitized key {k:?}",
        );
    }
    // Visually-similar but semantically distinct keys must NOT
    // collide on disk.
    assert_ne!(sanitize_key("a/b"), sanitize_key("a:b"));
    assert_ne!(sanitize_key("foo bar"), sanitize_key("foo_bar"));
}

// ---- ai.tools[] allowlist enforcement -----------------------------
//
// These tests mutate $COS_APPS_DIR which is process-global, so the
// module shares one Mutex with itself (same pattern as
// `agent::tools::cos_apps`). We never go through the real
// `/usr/lib/cos/apps` filesystem.

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn with_tmp_apps<F: FnOnce()>(label: &str, manifests: &[(&str, &str)], f: F) {
    let _g = env_lock();
    let dir = std::env::temp_dir().join(format!(
        "cos-tools-allow-{}-{}",
        label,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for (id, body) in manifests {
        let app_dir = dir.join(id);
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("app.json"), body).unwrap();
    }
    let prev = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_APPS_DIR", &dir);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    match prev {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

fn manifest_with_tools(id: &str, tools: &[&str]) -> String {
    let tools_json = serde_json::to_string(tools).unwrap();
    format!(
        r#"{{
            "id": "{id}",
            "version": "0.1.0",
            "name": {{ "en": "test" }},
            "summary": {{ "en": "test fixture" }},
            "runtime": "python",
            "entry": "main.py",
            "operations": {{}},
            "ai": {{
                "budget": {{ "monthly_units": 0 }},
                "origins": ["trusted"],
                "tools": {tools_json}
            }}
        }}"#
    )
}

fn manifest_no_ai(id: &str) -> String {
    format!(
        r#"{{
            "id": "{id}",
            "version": "0.1.0",
            "name": {{ "en": "test" }},
            "summary": {{ "en": "test fixture" }},
            "runtime": "python",
            "entry": "main.py",
            "operations": {{}}
        }}"#
    )
}

#[test]
fn execute_rejects_tool_not_in_allowlist() {
    let app = "demo-app";
    let m = manifest_with_tools(app, &["kv.get"]);
    // Self-check the fixture parses; if it doesn't, the apps
    // discovery silently drops it and the assertion below fires
    // with the cryptic "unknown app" message instead of the
    // intended allowlist error.
    crate::caps::manifest::Manifest::from_json(&m)
        .expect("test fixture manifest must parse");
    with_tmp_apps("not-in-allowlist", &[(app, &m)], || {
        // valid path arg so we get past derive_scope; allowlist
        // check must still trip.
        let err = execute("fs.read_text", app, &json!({"path": "/tmp/x"}))
            .unwrap_err();
        assert!(
            err.starts_with("tool not in ai.tools:"),
            "wrong error bucket: {err}"
        );
        assert!(err.contains("fs.read_text"), "{err}");
        assert!(err.contains(app), "{err}");
    });
}

#[test]
fn execute_rejects_app_without_ai_block() {
    let app = "no-ai-app";
    let m = manifest_no_ai(app);
    with_tmp_apps("no-ai-block", &[(app, &m)], || {
        let err = execute("fs.read_text", app, &json!({"path": "/tmp/x"}))
            .unwrap_err();
        assert!(
            err.starts_with("no ai policy:"),
            "wrong error bucket: {err}"
        );
        assert!(err.contains(app), "{err}");
    });
}

#[test]
fn execute_rejects_unknown_app() {
    let other = "different-app";
    let m = manifest_with_tools(other, &["kv.get"]);
    with_tmp_apps("unknown-app", &[(other, &m)], || {
        let err = execute("kv.get", "nope", &json!({"key": "x"}))
            .unwrap_err();
        assert!(err.starts_with("unknown app:"), "got: {err}");
    });
}

#[test]
fn execute_allowlist_runs_after_arg_shape_check() {
    // Even when the tool IS in the allowlist, malformed args still
    // fail with a tool-impl error (not the allowlist message). This
    // preserves the order baked into execute_inner — bad args
    // short-circuit before the manifest lookup.
    let app = "demo-app";
    let m = manifest_with_tools(app, &["fs.read_text"]);
    with_tmp_apps("args-shape-first", &[(app, &m)], || {
        let err = execute("fs.read_text", app, &json!({})).unwrap_err();
        assert!(err.contains("path"), "expected arg error, got: {err}");
        assert!(
            !err.starts_with("tool not in ai.tools:"),
            "allowlist must not fire on bad args: {err}"
        );
    });
}
