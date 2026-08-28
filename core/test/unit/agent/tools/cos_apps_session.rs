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
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::App, "kv");
}

/// Copy the in-tree `apps/kv` package into a scratch root and sign it.
///
/// The repository checkout is not an approved package root, so an
/// in-tree App is quarantined by design. Tests that need to *run* one
/// stage a signed copy instead of weakening the gate.
fn signed_copy_of_repo_apps() -> std::path::PathBuf {
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("apps");
    let root = std::env::temp_dir().join(format!(
        "cos-session-apps-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let kv_src = source.join("kv");
    if !kv_src.join("server.py").is_file() {
        return source;
    }
    // `_shared` is the sibling helper tree the App imports at runtime;
    // it is mounted read-only next to the package, not part of it.
    let shared = source.join("_shared");
    if shared.is_dir() {
        copy_tree(&shared, &root.join("_shared"));
    }
    let kv_dst = root.join("kv");
    copy_tree(&kv_src, &kv_dst);
    crate::test_env::sign_test_package(&kv_dst, crate::provenance::PackageKind::App, "kv");
    root
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap().filter_map(Result::ok) {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&from).unwrap();
        if meta.is_dir() {
            copy_tree(&from, &to);
        } else if meta.is_file() {
            std::fs::copy(&from, &to).unwrap();
        }
    }
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
            required_when: None,
            repeatable: false,
            aliases: Vec::new(),
            positional_alias: false,
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
            required_when: None,
            repeatable: true,
            aliases: Vec::new(),
            positional_alias: false,
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
            required_when: None,
            repeatable: false,
            aliases: Vec::new(),
            positional_alias: false,
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

#[test]
fn build_schema_exposes_conditional_requiredness() {
    let args: Vec<crate::caps::manifest::Arg> =
        serde_json::from_value(serde_json::json!([
        {"name":"state","kind":"name","required":true},
        {
            "name":"confirm","kind":"bool","choices":[true],
            "required_when":{"kind":"arg-equals","arg":"state","value":"off"}
        }
    ]))
        .unwrap();
    let schema = build_schema(&args);
    assert_eq!(
        schema["allOf"][0],
        serde_json::json!({
            "if":{"properties":{"state":{"const":"off"}},"required":["state"]},
            "then":{"required":["confirm"]},
            "else":{"not":{"required":["confirm"]}}
        })
    );
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
    let apps_dir = signed_copy_of_repo_apps();
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
    let apps_dir = signed_copy_of_repo_apps();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn injected_app_root_is_used_for_discovery_and_execution() {
    let _g = env_lock();
    // A signed copy, not the checkout: the repository tree is not an
    // approved package root, so an in-tree App is quarantined by
    // design and could not be opened from either root.
    let injected_root = signed_copy_of_repo_apps();
    if !injected_root.join("kv").join("server.py").is_file()
        || std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
    {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let ambient_root = temp.path().join("ambient-apps");
    std::fs::create_dir_all(&ambient_root).unwrap();
    let _apps = crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", &ambient_root);
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_MODE", "permissive");
    let _session = crate::test_env::TestSessionGuard::admin(temp.path());
    let _local_sessions =
        crate::test_env::TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let _runner = install_test_app_runner(temp.path());
    let app = crate::apps::find_verified(&injected_root, "kv").expect("injected kv app");

    let _ = close_session_at("kv", &injected_root).await;
    let opened = open_session_at("kv", &app.dir, &injected_root, &app.manifest)
        .await
        .expect("open from injected root");

    assert!(opened.1 >= 5);
    assert!(crate::apps::find(&ambient_root, "kv").is_none());
    assert!(close_session_at("kv", &injected_root).await);
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


// ---------------------------------------------------------------------------
// The session server runs the signed snapshot, or it does not run
// ---------------------------------------------------------------------------

/// A minimal stdio App package whose session entry is a real script.
fn session_package(root: &Path, id: &str, entrypoints: &[&str]) -> std::path::PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("app.json"),
        serde_json::json!({
            "id": id,
            "version": "1.0.0",
            "name": id,
            "runtime": "python",
            "operations": {},
            "session": {
                "transport": "stdio",
                "entry": "server.py",
                "tools": []
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(dir.join("server.py"), "# verified\n").unwrap();
    std::fs::write(dir.join("helper.py"), "# signed but not declared\n").unwrap();
    crate::test_env::install_test_trust();
    crate::test_env::sign_test_package_with_entrypoints(
        &dir,
        crate::provenance::PackageKind::App,
        id,
        entrypoints,
    );
    dir
}

fn launch_for(dir: &Path, id: &str) -> Result<crate::bridge::AppLaunch, String> {
    let app = crate::apps::find_verified(dir.parent().unwrap(), id)?;
    let verified = app.require_verified()?;
    crate::bridge::AppLaunch::new(std::sync::Arc::clone(verified))
}

#[cfg(unix)]
#[test]
fn the_session_entry_must_be_a_declared_signed_entrypoint() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let root = crate::test_env::secure_scratch_dir("session-entry");
    let apps = root.join("apps");
    std::fs::create_dir_all(&apps).unwrap();

    // Declared: resolves.
    let dir = session_package(&apps, "declared", &["server.py"]);
    let launch = launch_for(&dir, "declared").expect("verified");
    assert_eq!(
        declared_session_entry(&launch).expect("declared entry"),
        "server.py"
    );

    // Present in the package and covered by the signed file tree, but
    // never declared as an entrypoint. Being signed is not the same as
    // being something the publisher said may be executed — otherwise a
    // signed package becomes a launcher for anything shipped with it.
    let other = session_package(&apps, "undeclared", &["helper.py"]);
    let launch = launch_for(&other, "undeclared").expect("verified");
    let error = declared_session_entry(&launch).expect_err("undeclared entry is refused");
    assert!(
        error.contains("not a declared, signed entrypoint"),
        "unexpected: {error}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn replacing_the_session_script_after_binding_is_detected() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let root = crate::test_env::secure_scratch_dir("session-toctou");
    let apps = root.join("apps");
    std::fs::create_dir_all(&apps).unwrap();
    let dir = session_package(&apps, "toctou", &["server.py"]);
    let launch = launch_for(&dir, "toctou").expect("verified");

    let entry = declared_session_entry(&launch).expect("entry");
    let binding = launch.bind(&[entry.clone()]).expect("bind");
    let bound = SessionBinding::new(binding, entry, dir.join("server.py"));
    bound.assert_pinned().expect("nothing has moved yet");

    // Replace the script the way an attacker would: a fresh file at the
    // same path. The descriptors this binding holds still name the
    // verified inode, so the swap is visible as a different identity.
    std::fs::remove_file(dir.join("server.py")).unwrap();
    std::fs::write(dir.join("server.py"), "# swapped\n").unwrap();
    let error = bound
        .assert_pinned()
        .expect_err("a replaced session script must fail the launch");
    assert!(error.contains("replaced after verification"), "unexpected: {error}");

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn replacing_the_package_directory_after_binding_is_detected() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let root = crate::test_env::secure_scratch_dir("session-dir-swap");
    let apps = root.join("apps");
    std::fs::create_dir_all(&apps).unwrap();
    let dir = session_package(&apps, "dirswap", &["server.py"]);
    let launch = launch_for(&dir, "dirswap").expect("verified");
    let entry = declared_session_entry(&launch).expect("entry");
    let binding = launch.bind(&[entry.clone()]).expect("bind");
    let bound = SessionBinding::new(binding, entry, dir.join("server.py"));
    bound.assert_pinned().expect("clean");

    // Swap the whole directory for another one — the classic
    // "verify one tree, execute another" move.
    let decoy = apps.join("dirswap-decoy");
    std::fs::create_dir_all(&decoy).unwrap();
    std::fs::write(decoy.join("server.py"), "# decoy\n").unwrap();
    std::fs::rename(&dir, apps.join("dirswap-old")).unwrap();
    std::fs::rename(&decoy, &dir).unwrap();

    let error = bound
        .assert_pinned()
        .expect_err("a replaced package directory must fail the launch");
    // Either shape is a refusal: the decoy may not even contain the
    // signed files, in which case they are simply gone.
    assert!(
        error.contains("replaced after verification") || error.contains("unreadable"),
        "unexpected: {error}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn a_revoked_package_cannot_be_bound_for_a_session() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let root = crate::test_env::secure_scratch_dir("session-revoked");
    let apps = root.join("apps");
    std::fs::create_dir_all(&apps).unwrap();
    let dir = session_package(&apps, "revoked", &["server.py"]);
    let launch = launch_for(&dir, "revoked").expect("verified");
    let entry = declared_session_entry(&launch).expect("entry");
    assert!(
        launch.bind(&[entry.clone()]).is_ok(),
        "the package binds while it is still trusted"
    );

    // Revoke the artifact, then try to bind again. `bind` re-asserts
    // the snapshot against the current store before it opens anything,
    // so the launch is refused rather than started and then stopped.
    let digest = launch.package().content_digest().to_string();
    crate::test_env::revoke_test_package(&digest);
    let error = match launch.bind(&[entry]) {
        Ok(_) => panic!("a revoked package must not be bound for launch"),
        Err(error) => error,
    };
    assert!(error.contains("provenance check"), "unexpected: {error}");

    crate::test_env::install_test_trust();
    let _ = std::fs::remove_dir_all(&root);
}
