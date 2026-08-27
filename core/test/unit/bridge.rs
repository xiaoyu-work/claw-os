use super::*;

#[test]
fn default_entries_are_runtime_aware() {
    assert_eq!(Runtime::Python.default_entry(), "main.py");
    assert_eq!(Runtime::Node.default_entry(), "main.js");
    // Shell + Binary just need to be non-empty.
    assert!(!Runtime::Shell.default_entry().is_empty());
    assert!(!Runtime::Binary.default_entry().is_empty());
}

#[test]
fn panel_environment_requires_explicit_manifest_opt_in() {
    let filter = |panel_applet| {
        preserved_app_environment(panel_applet, |key| Some(key.to_string()))
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
    };

    let normal = filter(false);
    assert_eq!(normal.len(), SAFE_APP_ENV_KEYS.len());
    for key in SAFE_APP_ENV_KEYS {
        assert!(normal.contains_key(*key), "normal launch dropped {key}");
    }
    for key in PANEL_APPLET_ENV_KEYS {
        assert!(
            !normal.contains_key(*key),
            "normal launch inherited panel-only {key}"
        );
    }

    let panel = filter(true);
    assert_eq!(
        panel.len(),
        SAFE_APP_ENV_KEYS.len() + PANEL_APPLET_ENV_KEYS.len()
    );
    for key in PANEL_APPLET_ENV_KEYS {
        assert!(
            panel.contains_key(*key),
            "opted-in panel applet dropped {key}"
        );
    }
    assert!(!panel.contains_key("COSMIC_PANEL_UNRELATED"));
    assert!(!panel.contains_key("AWS_SECRET_ACCESS_KEY"));
}

#[test]
fn run_app_errors_when_app_dir_missing() {
    let tmp = std::env::temp_dir().join("cos-bridge-test-missing");
    let _ = std::fs::remove_dir_all(&tmp);
    let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
    // No app.json + no main.py → python branch surfaces
    // "app has no main.py" via run_python_app.
    assert!(
        err.contains("main.py") || err.contains("app.json"),
        "expected main.py / app.json reference, got: {err}"
    );
}

#[test]
fn run_app_rejects_non_main_py_for_python() {
    let tmp = std::env::temp_dir().join("cos-bridge-test-pyentry");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(
        tmp.join("app.json"),
        r#"{"id":"x","version":"0","name":"X","runtime":"python","entry":"alt.py"}"#,
    )
    .unwrap();
    let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
    assert!(
        err.contains("entry='main.py'"),
        "expected python-entry guard, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn run_app_errors_on_unknown_runtime() {
    let tmp = std::env::temp_dir().join("cos-bridge-test-unknown");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(
        tmp.join("app.json"),
        r#"{"id":"x","version":"0","name":"X","runtime":"rust"}"#,
    )
    .unwrap();
    let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
    // serde rejects unknown runtime values at parse time.
    assert!(
        err.contains("unknown variant") || err.contains("runtime"),
        "expected runtime parse error, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn run_app_node_entry_missing_surfaces_clear_error() {
    let tmp = std::env::temp_dir().join("cos-bridge-test-node-missing");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(
        tmp.join("app.json"),
        r#"{"id":"x","version":"0","name":"X","runtime":"node"}"#,
    )
    .unwrap();
    let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
    assert!(
        err.contains("app entry not found"),
        "expected entry-missing error, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Regression: bridge previously did `child.wait()` BEFORE reading
/// stdout/stderr. When the child wrote more than the Linux pipe
/// buffer (~64KB) to stdout, the child blocked on write() while
/// the parent blocked on wait() — `cos` process hung forever. The
/// fix routes both run_python_app and run_app through
/// `wait_with_output`, which drains the streams in background
/// threads.
///
/// This test asks a tiny Python app to emit a JSON payload well
/// above 64KB. Before the fix this test would never return; we
/// add a generous-but-not-infinite outer timeout to make a
/// regression a quick CI failure instead of a hang.
#[cfg(unix)]
#[test]
fn run_python_app_handles_stdout_larger_than_pipe_buffer() {
    let _env = crate::test_env::lock_env();
    // Skip if python3 isn't on PATH (some minimal CI images).
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let tmp = std::env::temp_dir().join("cos-bridge-test-bigstdout");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    // ~256 KB of payload — comfortably over the 64KB pipe buffer.
    std::fs::write(
        tmp.join("main.py"),
        "def run(command, args):\n    return {\"data\": \"x\" * 262144, \"args\": args}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("app.json"),
        r#"{
              "id": "x",
              "version": "0",
              "name": "X",
              "operations": {
                "noop": {
                  "label": "Noop",
                  "args": [{"name": "path", "kind": "path", "default": "."}]
                }
              }
            }"#,
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let _session = crate::test_env::TestSessionGuard::admin(state.path());
    let _local_sessions =
        crate::test_env::TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let runner = state.path().join("claw-app-runner");
    std::fs::write(
        &runner,
        "#!/bin/sh\n[ \"$1\" = \"--\" ] && shift\nexec \"$@\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _runner = crate::test_env::TestEnvVarGuard::set("CLAW_APP_RUNNER_BIN", &runner);

    // Hard timeout: any deadlock regresses this into a 10s failure
    // rather than a session-killing hang.
    let (tx, rx) = std::sync::mpsc::channel();
    let app_dir = tmp.clone();
    let t = std::thread::spawn(move || {
        let r = run_python_app(&app_dir, "noop", &[], "/tmp", "/tmp");
        let _ = tx.send(r);
    });
    let result = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("run_python_app deadlocked on >64KB stdout");
    let _ = t.join();
    let out = result.expect("run_python_app errored").expect("got json");
    assert!(
        out.len() >= 262_144,
        "payload truncated, got {} bytes",
        out.len()
    );
    assert!(out.contains("\"data\""), "json missing data field");
    let payload: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        payload["args"][0],
        std::env::current_dir()
            .unwrap()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn operation_defaults_bind_the_same_argv_and_narrow_caps() {
    let _env = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    let manifest = Manifest::from_json(
        r#"{
              "id": "defaults",
              "version": "0.1",
              "name": "Defaults",
              "operations": {
                "ls": {
                  "label": "List",
                  "args": [{"name": "path", "kind": "path", "default": "."}],
                  "needs": [
                    {"verb": "fs.read",
                     "scope": {"kind": "from-arg", "arg": "path"},
                     "why": "List the current directory."}
                  ]
                },
                "search": {
                  "label": "Search",
                  "args": [
                    {"name": "query", "kind": "text", "required": true},
                    {"name": "path", "kind": "path", "default": "/workspace"}
                  ],
                  "needs": [
                    {"verb": "fs.read",
                     "scope": {"kind": "from-arg", "arg": "path"},
                     "why": "Search the workspace."}
                  ]
                },
                "download": {
                  "label": "Download",
                  "args": [
                    {"name": "url", "kind": "text", "required": true},
                    {"name": "output", "kind": "path",
                     "default_from": {
                       "arg": "url",
                       "transform": "url-path-basename",
                       "prefix": "~/",
                       "fallback": "download"
                     }}
                  ],
                  "needs": [
                    {"verb": "fs.write",
                     "scope": {"kind": "from-arg", "arg": "output"},
                     "why": "Save the download."}
                  ]
                }
              }
            }"#,
    )
    .unwrap();

    let current_dir = std::env::current_dir().unwrap().canonicalize().unwrap();
    let ls = bind_operation_args(&manifest.operations["ls"], &[]).unwrap();
    assert_eq!(ls.argv, vec![current_dir.to_string_lossy()]);
    let mut ls_parent = CapSet::new();
    ls_parent.insert(Cap::new(
        Verb::FS_READ,
        Scope::path(current_dir.to_string_lossy()),
    ));
    let ls_caps =
        constrained_operation_caps(&ls_parent, true, &manifest.operations["ls"], &ls.values)
            .unwrap();
    assert!(ls_caps.covers(&Cap::new(
        Verb::FS_READ,
        Scope::path(current_dir.to_string_lossy()),
    )));

    let search =
        bind_operation_args(&manifest.operations["search"], &["needle".to_string()]).unwrap();
    assert_eq!(search.argv, vec!["needle", "/workspace"]);
    assert!(bind_operation_args(&manifest.operations["search"], &[])
        .unwrap_err()
        .contains("required operation arg `query`"));
    let mut search_parent = CapSet::new();
    search_parent.insert(Cap::new(Verb::FS_READ, Scope::path("/workspace")));
    let search_caps = constrained_operation_caps(
        &search_parent,
        true,
        &manifest.operations["search"],
        &search.values,
    )
    .unwrap();
    assert!(search_caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/workspace"))));

    let url = "https://example.com/releases/artifact.bin?download=1".to_string();
    let download =
        bind_operation_args(&manifest.operations["download"], std::slice::from_ref(&url)).unwrap();
    let default_output = home.path().join("artifact.bin");
    assert_eq!(
        download.argv,
        vec![url.clone(), default_output.to_string_lossy().into_owned()]
    );
    let mut download_parent = CapSet::new();
    download_parent.insert(Cap::new(
        Verb::FS_WRITE,
        Scope::path(default_output.to_string_lossy()),
    ));
    let download_caps = constrained_operation_caps(
        &download_parent,
        true,
        &manifest.operations["download"],
        &download.values,
    )
    .unwrap();
    assert!(download_caps.covers(&Cap::new(
        Verb::FS_WRITE,
        Scope::path(default_output.to_string_lossy()),
    )));

    let explicit_output = home.path().join("chosen.bin");
    let explicit_args = vec![
        url,
        "--output".to_string(),
        explicit_output.to_string_lossy().into_owned(),
    ];
    let explicit = bind_operation_args(&manifest.operations["download"], &explicit_args).unwrap();
    assert_eq!(explicit.argv, explicit_args);
    let mut explicit_parent = CapSet::new();
    explicit_parent.insert(Cap::new(
        Verb::FS_WRITE,
        Scope::path(explicit_output.to_string_lossy()),
    ));
    let explicit_caps = constrained_operation_caps(
        &explicit_parent,
        true,
        &manifest.operations["download"],
        &explicit.values,
    )
    .unwrap();
    assert!(explicit_caps.covers(&Cap::new(
        Verb::FS_WRITE,
        Scope::path(explicit_output.to_string_lossy()),
    )));
    assert!(!explicit_caps.covers(&Cap::new(
        Verb::FS_WRITE,
        Scope::path(default_output.to_string_lossy()),
    )));
}
