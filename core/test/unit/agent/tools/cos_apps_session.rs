use super::*;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn write_kv_app(root: &Path) {
    let dir = root.join("kv");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("app.json"),
        serde_json::json!({
            "id": "kv",
            "version": "0.1.0",
            "name": {"en": "KV"},
            "summary": {"en": "Key/value."},
            "operations": {},
            "session": {
                "entry": "server.py",
                "tools": [
                    {
                        "name": "kv.get",
                        "summary": {"en": "Read a value."},
                        "args": [{"name":"key","kind":"name","required":true}],
                        "needs": [
                            {"verb":"data.kv.read",
                             "scope":{"kind":"from-arg","arg":"key"},
                             "why":{"en":"Read by key."}}
                        ]
                    }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("server.py"),
        "# placeholder — not exec'd in this test\n",
    )
    .unwrap();
}

fn install_test_app_runner(root: &Path) -> crate::test_env::TestEnvVarGuard {
    use std::os::unix::fs::PermissionsExt;

    let runner = root.join("claw-app-runner");
    std::fs::write(
        &runner,
        "#!/bin/sh\n[ \"$1\" = \"--\" ] && shift\nexec \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::test_env::TestEnvVarGuard::set("CLAW_APP_RUNNER_BIN", runner)
}

#[test]
fn register_all_emits_one_tool_per_manifest_entry_plus_meta() {
    let _g = env_lock();
    let tmp = tempfile::tempdir().unwrap();
    write_kv_app(tmp.path());
    let prev = std::env::var("COS_APPS_DIR").ok();
    std::env::set_var("COS_APPS_DIR", tmp.path());

    let mut r = ToolRegistry::new();
    register_all(&mut r);
    let names = r.names_unfiltered();
    // One session tool + two meta-tools.
    assert!(names.contains(&"app_kv__kv_get"), "got {names:?}");
    assert!(names.contains(&"cos_app_session_open"));
    assert!(names.contains(&"cos_app_session_close"));

    match prev {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
}

#[test]
fn registry_name_replaces_dots_with_underscores() {
    assert_eq!(registry_name_for("kv", "kv.get"), "app_kv__kv_get");
    assert_eq!(
        registry_name_for("calendar", "calendar.find_slots"),
        "app_calendar__calendar_find_slots"
    );
}

#[test]
fn build_schema_marks_required_args() {
    use crate::caps::manifest::{Arg, ArgBinding, ArgKind};
    use crate::i18n::LocalizedText;
    let args = vec![
        Arg {
            name: "key".into(),
            kind: ArgKind::Name,
            binding: Some(ArgBinding::Positional),
            required: true,
            repeatable: false,
            choices: Vec::new(),
            default: None,
            default_from: None,
            trusted_resolver: None,
            label: LocalizedText::default(),
        },
        Arg {
            name: "provider".into(),
            kind: ArgKind::Name,
            binding: Some(ArgBinding::Positional),
            required: false,
            repeatable: true,
            choices: vec![serde_json::json!("a"), serde_json::json!("b")],
            default: None,
            default_from: None,
            trusted_resolver: None,
            label: LocalizedText::default(),
        },
        Arg {
            name: "ttl".into(),
            kind: ArgKind::Number,
            binding: Some(ArgBinding::Positional),
            required: false,
            repeatable: false,
            choices: Vec::new(),
            default: Some(serde_json::json!(60)),
            default_from: None,
            trusted_resolver: None,
            label: LocalizedText::default(),
        },
    ];
    let schema = build_schema(&args);
    let required = schema["required"].as_array().unwrap();
    assert_eq!(required.len(), 1);
    assert_eq!(required[0].as_str(), Some("key"));
    assert_eq!(
        schema["properties"]["ttl"]["default"],
        serde_json::json!(60)
    );
    assert_eq!(schema["properties"]["provider"]["type"], "array");
    assert_eq!(
        schema["properties"]["provider"]["items"]["enum"],
        serde_json::json!(["a", "b"])
    );
    assert_eq!(schema["properties"]["key"]["type"], "string");
    assert_eq!(schema["properties"]["ttl"]["type"], "number");
}

/// Spawn the real `apps/kv` server via [`open_session`], drive it
/// across multiple calls, and verify session state persists. This
/// is the canonical proof that the **App → MCP server** wiring
/// (manifest schema + Python SDK + kernel bring-up + bridge)
/// works end to end. We use `COS_CAPS_MODE=permissive` so the
/// test doesn't need to set up role grants; the caps-gate
/// codepath is still exercised — `crate::caps::require` is
/// called for every tool, it just allows through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pilot_kv_e2e_call_chain() {
    let _g = env_lock();
    let apps_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("apps");
    if !apps_dir.join("kv").join("server.py").is_file() {
        eprintln!("skip pilot_kv_e2e: {} not present", apps_dir.display());
        return;
    }
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skip pilot_kv_e2e: python3 not on PATH");
        return;
    }

    let data = tempfile::tempdir().unwrap();
    let prev_apps = std::env::var("COS_APPS_DIR").ok();
    let prev_data = std::env::var("COS_DATA_DIR").ok();
    let prev_mode = std::env::var("COS_CAPS_MODE").ok();
    std::env::set_var("COS_APPS_DIR", &apps_dir);
    std::env::set_var("COS_DATA_DIR", data.path());
    std::env::set_var("COS_CAPS_MODE", "permissive");
    let _session = crate::test_env::TestSessionGuard::admin(data.path());
    let _local_sessions =
        crate::test_env::TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let _runner = install_test_app_runner(data.path());

    // Make sure no stale entry from a previous test run survives.
    let _ = close_session("kv").await;

    let opened = open_session("kv").await.expect("open kv");
    assert!(
        opened.1 >= 5,
        "kv should advertise ≥5 tools, got {}",
        opened.1
    );

    // 1) set, get — verify in-memory state survives.
    let r = opened
        .0
        .call_tool("kv.set", Some(serde_json::json!({"key":"x","value":"42"})))
        .await
        .expect("set");
    assert!(!r.is_error.unwrap_or(false));

    let r = opened
        .0
        .call_tool("kv.get", Some(serde_json::json!({"key":"x"})))
        .await
        .expect("get");
    let text = first_text(&r);
    assert!(text.contains("42"), "kv.get returned: {text}");

    let r = opened.0.call_tool("kv.list", None).await.expect("list");
    let text = first_text(&r);
    assert!(text.contains("\"x\""), "kv.list returned: {text}");

    let closed = close_session("kv").await;
    assert!(closed);
    let opened2 = open_session("kv").await.expect("re-open kv");
    let r = opened2
        .0
        .call_tool("kv.get", Some(serde_json::json!({"key":"x"})))
        .await
        .expect("get after restart");
    let text = first_text(&r);
    assert!(
        text.contains("42"),
        "post-restart get should re-load value: {text}"
    );

    let _ = close_session("kv").await;

    match prev_apps {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    match prev_data {
        Some(v) => std::env::set_var("COS_DATA_DIR", v),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
    match prev_mode {
        Some(v) => std::env::set_var("COS_CAPS_MODE", v),
        None => std::env::remove_var("COS_CAPS_MODE"),
    }
}

/// Race test: two callers concurrently invoke `open_session` on
/// the same app. The per-app lock guarantees exactly one child is
/// spawned + one session table entry is created. Without the
/// lock both callers would race past the manager probe, both
/// would spawn a child, and one of them would be silently
/// overwritten in `table.insert` — leaving an orphan whose stdio
/// handles get dropped immediately.
///
/// We assert this by counting how many distinct `Arc<McpClient>`s
/// the two opens return — they must both be the same Arc, which
/// proves the second caller found the first's entry under the
/// lock and short-circuited the spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_race_single_child() {
    let _g = env_lock();
    let apps_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("apps");
    if !apps_dir.join("kv").join("server.py").is_file() {
        eprintln!(
            "skip open_race_single_child: {} not present",
            apps_dir.display()
        );
        return;
    }
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skip open_race_single_child: python3 not on PATH");
        return;
    }

    let data = tempfile::tempdir().unwrap();
    let prev_apps = std::env::var("COS_APPS_DIR").ok();
    let prev_data = std::env::var("COS_DATA_DIR").ok();
    let prev_mode = std::env::var("COS_CAPS_MODE").ok();
    std::env::set_var("COS_APPS_DIR", &apps_dir);
    std::env::set_var("COS_DATA_DIR", data.path());
    std::env::set_var("COS_CAPS_MODE", "permissive");
    let _session = crate::test_env::TestSessionGuard::admin(data.path());
    let _local_sessions =
        crate::test_env::TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let _runner = install_test_app_runner(data.path());

    let _ = close_session("kv").await;

    // Spawn two concurrent open_session calls. With the bug, both
    // would race past the manager probe and each spawn its own
    // server. With the per-app lock, the second blocks until the
    // first finishes, then short-circuits.
    let t1 = tokio::spawn(async { open_session("kv").await });
    let t2 = tokio::spawn(async { open_session("kv").await });
    let (r1, r2) = (t1.await.unwrap(), t2.await.unwrap());
    let (c1, _) = r1.expect("first open");
    let (c2, _) = r2.expect("second open");

    // Both callers must observe the same client (`Arc::ptr_eq`).
    // A second spawn would have produced a fresh Arc.
    assert!(
        Arc::ptr_eq(&c1, &c2),
        "open_session race produced two distinct sessions"
    );

    let _ = close_session("kv").await;

    match prev_apps {
        Some(v) => std::env::set_var("COS_APPS_DIR", v),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    match prev_data {
        Some(v) => std::env::set_var("COS_DATA_DIR", v),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
    match prev_mode {
        Some(v) => std::env::set_var("COS_CAPS_MODE", v),
        None => std::env::remove_var("COS_CAPS_MODE"),
    }
}

fn first_text(res: &crate::agent::tools::mcp::protocol::CallToolResult) -> String {
    use crate::agent::tools::mcp::protocol::ContentItem;
    for item in &res.content {
        if let ContentItem::Text { text } = item {
            return text.clone();
        }
    }
    String::new()
}
