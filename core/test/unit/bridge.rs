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
                    {"name": "output", "kind": "path", "binding": "flag",
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
    let ls_resolved = manifest.resolve_needs("ls", &ls.values).unwrap();
    let ls_caps = constrained_operation_caps(
        &ls_parent,
        true,
        &manifest.operations["ls"].needs,
        &ls_resolved,
    )
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
        .contains("argument `query` is required"));
    let mut search_parent = CapSet::new();
    search_parent.insert(Cap::new(Verb::FS_READ, Scope::path("/workspace")));
    let search_resolved = manifest.resolve_needs("search", &search.values).unwrap();
    let search_caps = constrained_operation_caps(
        &search_parent,
        true,
        &manifest.operations["search"].needs,
        &search_resolved,
    )
    .unwrap();
    assert!(search_caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/workspace"))));

    let url = "https://example.com/releases/artifact.bin?download=1".to_string();
    let download =
        bind_operation_args(&manifest.operations["download"], std::slice::from_ref(&url)).unwrap();
    let default_output = home.path().join("artifact.bin");
    assert_eq!(
        download.argv,
        vec![
            url.clone(),
            "--output".to_string(),
            default_output.to_string_lossy().into_owned()
        ]
    );
    let mut download_parent = CapSet::new();
    download_parent.insert(Cap::new(
        Verb::FS_WRITE,
        Scope::path(default_output.to_string_lossy()),
    ));
    let download_resolved = manifest.resolve_needs("download", &download.values).unwrap();
    let download_caps = constrained_operation_caps(
        &download_parent,
        true,
        &manifest.operations["download"].needs,
        &download_resolved,
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
    let explicit_resolved = manifest.resolve_needs("download", &explicit.values).unwrap();
    let explicit_caps = constrained_operation_caps(
        &explicit_parent,
        true,
        &manifest.operations["download"].needs,
        &explicit_resolved,
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

#[test]
fn approval_request_ids_are_read_only_from_a_typed_approval_denial() {
    let denial = ClawdCallError {
        message: "launcher cannot delegate sys.identity:name:accounts; awaiting approval".into(),
        data: Some(serde_json::json!({
            "status": "approval_required",
            "approval_requests": ["ap-1", "ap-2"],
        })),
    };
    assert_eq!(approval_requests(&denial), vec!["ap-1", "ap-2"]);

    // Any other failure is terminal: the launcher must not start waiting.
    for data in [
        None,
        Some(serde_json::json!({"approval_requests": ["ap-1"]})),
        Some(serde_json::json!({"status": "approval_required"})),
        Some(serde_json::json!({"status": "other", "approval_requests": ["ap-1"]})),
    ] {
        let error = ClawdCallError {
            message: "nope".into(),
            data,
        };
        assert!(approval_requests(&error).is_empty());
    }
}

#[test]
fn an_approval_wait_can_be_cancelled() {
    cancel_pending_approval_wait();
    let error = wait_for_approvals(&["ap-1".to_string()])
        .expect_err("a cancelled wait must end with a terminal error");
    assert!(error.contains("cancelled"), "unexpected: {error}");
    // The flag is taken, so a later launch is not poisoned by it.
    assert!(!APPROVAL_WAIT_CANCELLED.load(Ordering::SeqCst));
}

#[test]
fn no_approval_secret_travels_through_the_environment() {
    // Approval is settled in-process: the launcher waits and retries
    // over its own connection, so nothing is exported to the App or
    // read back from the environment.
    let preserved = preserved_app_environment(false, |key| Some(format!("value-of-{key}")));
    assert!(
        !preserved
            .iter()
            .any(|(key, _)| key.to_ascii_uppercase().contains("APPROVAL")),
        "no approval material may be forwarded into an App"
    );
}

#[test]
fn defaulted_bool_args_never_shift_the_effective_argv() {
    let manifest = Manifest::from_json(
        r#"{
              "id": "flags",
              "version": "0.1",
              "name": "Flags",
              "operations": {
                "sync": {
                  "label": "Sync",
                  "args": [
                    {"name": "recursive", "kind": "bool", "binding": "flag",
                     "default": true},
                    {"name": "target", "kind": "name", "default": "primary"},
                    {"name": "limit", "kind": "integer", "binding": "flag",
                     "default": 10}
                  ],
                  "needs": [
                    {"verb": "data.kv.read",
                     "scope": {"kind": "from-arg", "arg": "target"},
                     "why": "Read the target store."}
                  ]
                }
              }
            }"#,
    )
    .unwrap();
    let operation = &manifest.operations["sync"];

    let bound = bind_operation_args(operation, &[]).unwrap();
    assert_eq!(
        bound.argv,
        vec![
            "primary".to_string(),
            "--recursive".to_string(),
            "--limit".to_string(),
            "10".to_string()
        ],
        "flag defaults retain their declared argv binding"
    );
    assert_eq!(bound.values["recursive"], serde_json::Value::Bool(true));
    assert_eq!(
        bound.values["target"],
        serde_json::Value::String("primary".into())
    );
    assert_eq!(bound.values["limit"], serde_json::json!(10));

    // The authority re-binds this argv and must reach the same values,
    // otherwise the scope it derives would name a different resource.
    let rebound = crate::caps::args::bind_cli_args(&operation.args, &bound.argv).unwrap();
    assert_eq!(rebound["target"], bound.values["target"]);
    assert_eq!(rebound["recursive"], bound.values["recursive"]);
    assert_eq!(rebound["limit"], bound.values["limit"]);

    let mut parent = CapSet::new();
    parent.insert(Cap::new(Verb::DATA_KV_READ, Scope::name("primary")));
    let resolved = manifest.resolve_needs("sync", &rebound).unwrap();
    let caps = constrained_operation_caps(&parent, true, &operation.needs, &resolved).unwrap();
    assert!(caps.covers(&Cap::new(Verb::DATA_KV_READ, Scope::name("primary"))));
}

#[test]
fn canonical_argv_matches_bound_boolean_and_delimiter_values() {
    let manifest = Manifest::from_json(
        r#"{
            "id":"canonical","version":"0.1","name":"Canonical",
            "operations":{"run":{"label":"Run","args":[
                {"name":"text","kind":"text","required":true},
                {"name":"confirm","kind":"bool","binding":"flag","default":false},
                {"name":"enabled","kind":"bool","binding":"positional","default":true},
                {"name":"limit","kind":"integer","binding":"flag","default":10},
                {"name":"label","kind":"text","binding":"flag"}
            ]}}
        }"#,
    )
    .unwrap();
    let operation = &manifest.operations["run"];

    let inline_true = bind_operation_args(
        operation,
        &["hello".into(), "--confirm=true".into()],
    )
    .unwrap();
    assert_eq!(
        inline_true.argv,
        ["hello", "true", "--confirm", "--limit", "10"]
    );
    assert_eq!(inline_true.values["confirm"], serde_json::json!(true));

    let inline_false = bind_operation_args(
        operation,
        &["hello".into(), "--confirm=false".into()],
    )
    .unwrap();
    assert_eq!(
        inline_false.argv,
        ["hello", "true", "--confirm=false", "--limit", "10"]
    );
    assert_eq!(inline_false.values["confirm"], serde_json::json!(false));
    let rebound =
        crate::caps::args::bind_cli_args(&operation.args, &inline_false.argv).unwrap();
    assert_eq!(rebound, inline_false.values);

    let delimited = bind_operation_args(operation, &["--".into(), "--literal".into()]).unwrap();
    assert_eq!(
        delimited.argv,
        ["--limit", "10", "--", "--literal", "true"]
    );
    let rebound = crate::caps::args::bind_cli_args(&operation.args, &delimited.argv).unwrap();
    assert_eq!(rebound, delimited.values);

    let option_shaped_value = bind_operation_args(
        operation,
        &["hello".into(), "--label=--urgent".into()],
    )
    .unwrap();
    assert_eq!(
        option_shaped_value.argv,
        ["hello", "true", "--limit", "10", "--label=--urgent"]
    );
    let rebound =
        crate::caps::args::bind_cli_args(&operation.args, &option_shaped_value.argv).unwrap();
    assert_eq!(rebound, option_shaped_value.values);
}

#[test]
fn repeatable_flags_round_trip_through_canonical_argv() {
    let manifest = Manifest::from_json(
        r#"{
            "id":"repeat","version":"0.1","name":"Repeat",
            "operations":{"fetch":{"label":"Fetch","args":[
                {"name":"url","kind":"text","required":true},
                {"name":"header","kind":"text","binding":"flag","repeatable":true}
            ]}}
        }"#,
    )
    .unwrap();
    let operation = &manifest.operations["fetch"];
    let bound = bind_operation_args(
        operation,
        &[
            "https://example.test".into(),
            "--header=A: 1".into(),
            "--header".into(),
            "--urgent".into(),
        ],
    )
    .unwrap_err();
    assert!(bound.contains("valid text value"));

    let bound = bind_operation_args(
        operation,
        &[
            "https://example.test".into(),
            "--header=A: 1".into(),
            "--header=--urgent".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        bound.argv,
        [
            "https://example.test",
            "--header",
            "A: 1",
            "--header=--urgent"
        ]
    );
    assert_eq!(
        bound.values["header"],
        serde_json::json!(["A: 1", "--urgent"])
    );
    let rebound = crate::caps::args::bind_cli_args(&operation.args, &bound.argv).unwrap();
    assert_eq!(rebound, bound.values);
}

#[test]
fn explicit_false_overrides_true_default_for_authority_and_child() {
    let manifest = Manifest::from_json(
        r#"{
            "id":"boolean","version":"0.1","name":"Boolean",
            "operations":{"run":{"label":"Run","args":[
                {"name":"enabled","kind":"bool","binding":"flag","default":true}
            ],"needs":[
                {"verb":"data.kv.read","scope":{"kind":"fixed",
                 "scope":{"kind":"name","value":"enabled"}},
                 "when":{"kind":"arg-equals","arg":"enabled","value":true},
                 "why":"Read enabled data"}
            ]}}
        }"#,
    )
    .unwrap();
    let operation = &manifest.operations["run"];
    let bound =
        bind_operation_args(operation, &["--enabled=false".to_string()]).unwrap();
    assert_eq!(bound.argv, ["--enabled=false"]);
    assert_eq!(bound.values["enabled"], serde_json::json!(false));
    let rebound = crate::caps::args::bind_cli_args(&operation.args, &bound.argv).unwrap();
    assert_eq!(rebound, bound.values);
    assert_eq!(
        manifest.resolve_needs("run", &rebound).unwrap(),
        [Vec::new()]
    );
}

#[test]
fn in_process_wild_need_rejects_typed_wildcard_authority() {
    let manifest = Manifest::from_json(
        r#"{"id":"wild","version":"1","name":"Wild",
             "operations":{"dial":{"label":"Dial","needs":[
               {"verb":"net.dial","scope":{"kind":"wild"},"why":"Dial"}
             ]}}}"#,
    )
    .unwrap();
    let operation = &manifest.operations["dial"];
    let resolved = manifest.resolve_needs("dial", &BTreeMap::new()).unwrap();
    let parent = CapSet::from_caps([Cap::new(Verb::NET_DIAL, Scope::host("**"))]);
    assert!(constrained_operation_caps(
        &parent,
        true,
        &operation.needs,
        &resolved
    )
    .is_err());
}

#[test]
fn trusted_email_provider_is_bound_before_capability_derivation() {
    let credentials = tempfile::tempdir().unwrap();
    let _credentials =
        crate::test_env::TestEnvVarGuard::set("COS_CREDENTIALS_DIR", credentials.path());
    let _smtp = crate::test_env::TestEnvVarGuard::set("SMTP_HOST", "mail.example.test");
    let manifest = Manifest::from_json(
        r#"{
            "id":"email","version":"0.1","name":"Email",
            "operations":{"send":{"label":"Send","args":[
                {"name":"body","kind":"text","required":true},
                {"name":"provider","kind":"name","binding":"flag",
                 "trusted_resolver":"email-provider"},
                {"name":"host","kind":"host","binding":"flag",
                 "trusted_resolver":"email-host"}
            ],"needs":[{"verb":"secret.read","scope":{
                "kind":"from-arg-map","arg":"provider","values":{
                    "smtp":{"kind":"name","value":"default/SMTP_PASSWORD"},
                    "gmail":{"kind":"name","value":"default/GOOGLE_ACCESS_TOKEN"}
                }},"why":"Read provider credential"},
                {"verb":"net.dial","scope":{"kind":"from-arg","arg":"host"},
                 "why":"Connect to provider"}]}}
        }"#,
    )
    .unwrap();
    let operation = &manifest.operations["send"];
    let trusted = trusted_pre_dispatch_args("email", operation, &["hello".into()]).unwrap();
    assert_eq!(
        trusted,
        ["hello", "--provider", "smtp", "--host", "mail.example.test"]
    );
    let bound = bind_operation_args(operation, &trusted).unwrap();
    assert_eq!(bound.values["provider"], serde_json::json!("smtp"));
    assert_eq!(bound.values["host"], serde_json::json!("mail.example.test"));
    assert_eq!(bound.argv, trusted);

    let resolved = manifest.resolve_needs("send", &bound.values).unwrap();
    assert_eq!(
        resolved[0][0].scope,
        Scope::name("default/SMTP_PASSWORD")
    );
    assert_eq!(resolved[1][0].scope, Scope::host("mail.example.test"));
}

#[test]
fn local_calendar_resolution_is_identical_for_in_process_caps() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let app_dir = repository.join("apps/calendar");
    let manifest = load_manifest(&app_dir).unwrap().unwrap();
    let operation = &manifest.operations["today"];
    let credentials = tempfile::tempdir().unwrap();
    let _credentials =
        crate::test_env::TestEnvVarGuard::set("COS_CREDENTIALS_DIR", credentials.path());
    let trusted = trusted_pre_dispatch_args("calendar", operation, &[]).unwrap();
    assert_eq!(trusted, ["--provider", "local"]);
    let bound = bind_operation_args(operation, &trusted).unwrap();
    let resolved = manifest.resolve_needs("today", &bound.values).unwrap();

    let mut parent = CapSet::new();
    parent.insert(Cap::new(Verb::DATA_DB_READ, Scope::name("calendar")));
    parent.insert(Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/GOOGLE_ACCESS_TOKEN"),
    ));
    parent.insert(Cap::new(Verb::NET_DIAL, Scope::host("www.googleapis.com")));
    let caps =
        constrained_operation_caps(&parent, true, &operation.needs, &resolved).unwrap();
    assert!(caps.covers(&Cap::new(
        Verb::DATA_DB_READ,
        Scope::name("calendar")
    )));
    assert!(!caps.covers(&Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/GOOGLE_ACCESS_TOKEN")
    )));
    assert!(!caps.covers(&Cap::new(
        Verb::NET_DIAL,
        Scope::host("www.googleapis.com")
    )));

    let namespace = credentials.path().join("default");
    std::fs::create_dir_all(&namespace).unwrap();
    std::fs::write(namespace.join("GOOGLE_ACCESS_TOKEN.json"), "{}").unwrap();
    let trusted = trusted_pre_dispatch_args("calendar", operation, &[]).unwrap();
    assert_eq!(trusted, ["--provider", "google"]);
}

#[test]
fn stdin_forwarding_requires_an_explicit_operation_contract() {
    let app = tempfile::tempdir().unwrap();
    std::fs::write(
        app.path().join("app.json"),
        r#"{
            "id":"stdin","version":"0.1","name":"Stdin",
            "operations":{
                "pipe":{"label":"Pipe","stdin":true},
                "closed":{"label":"Closed"}
            }
        }"#,
    )
    .unwrap();
    assert!(operation_forwards_stdin(app.path(), "pipe").unwrap());
    assert!(!operation_forwards_stdin(app.path(), "closed").unwrap());
}

#[cfg(unix)]
#[test]
fn explicit_stdin_bytes_reach_python_and_polyglot_children() {
    let _env = crate::test_env::lock_env();
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

    let python = state.path().join("python-app");
    std::fs::create_dir_all(&python).unwrap();
    std::fs::write(
        python.join("app.json"),
        r#"{"id":"python-app","version":"1","name":"Pipe",
             "operations":{"read":{"label":"Read","stdin":true}}}"#,
    )
    .unwrap();
    std::fs::write(
        python.join("main.py"),
        "import sys\ndef run(command, args):\n    return {'input': sys.stdin.read()}\n",
    )
    .unwrap();
    let state_text = state.path().to_string_lossy();
    let output = run_python_app_with_stdin(
        &python,
        "read",
        &[],
        &state_text,
        &state_text,
        Some(b"python input"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output).unwrap()["input"],
        "python input"
    );
    let closed = run_python_app(
        &python,
        "read",
        &[],
        &state_text,
        &state_text,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&closed).unwrap()["input"],
        ""
    );

    let shell = state.path().join("shell-app");
    std::fs::create_dir_all(&shell).unwrap();
    std::fs::write(
        shell.join("app.json"),
        r#"{"id":"shell-app","version":"1","name":"Pipe","runtime":"shell",
             "operations":{"read":{"label":"Read","stdin":true}}}"#,
    )
    .unwrap();
    let entry = shell.join("main.sh");
    std::fs::write(
        &entry,
        "#!/bin/sh\npayload=$(cat)\nprintf '{\"input\":\"%s\"}\\n' \"$payload\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = run_app_with_stdin(
        &shell,
        "read",
        &[],
        &state_text,
        &state_text,
        Some(b"shell input"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output).unwrap()["input"],
        "shell input"
    );
}

#[test]
fn bundled_lone_limits_bind_before_optional_selectors() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let load = |path: &[&str]| {
        let path = path
            .iter()
            .fold(repository.join("apps"), |path, component| {
                path.join(component)
            })
            .join("app.json");
        Manifest::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
    };

    let events = load(&["event-center"]);
    let recent = &events.operations["recent"];
    let lone_limit = bind_operation_args(recent, &["25".into()]).unwrap();
    assert_eq!(lone_limit.values["limit"], serde_json::json!(25));
    assert_eq!(lone_limit.argv, ["25"]);
    let selected = bind_operation_args(
        recent,
        &["--source".into(), "security".into()],
    )
    .unwrap();
    assert_eq!(selected.values["source"], serde_json::json!("security"));
    assert_eq!(selected.values["limit"], serde_json::json!(100));
    assert_eq!(selected.argv, ["100", "--source", "security"]);

    let containers = load(&["container-manager"]);
    let logs = &containers.operations["logs"];
    let docker = bind_operation_args(
        logs,
        &["docker".into(), "web".into(), "50".into()],
    )
    .unwrap();
    assert_eq!(docker.values["lines"], serde_json::json!(50));
    assert_eq!(docker.argv, ["docker", "web", "50"]);
    let containerd = bind_operation_args(
        logs,
        &[
            "containerd".into(),
            "web".into(),
            "25".into(),
            "--namespace".into(),
            "default".into(),
        ],
    )
    .unwrap();
    assert_eq!(containerd.values["lines"], serde_json::json!(25));
    assert_eq!(containerd.values["namespace"], serde_json::json!("default"));
    assert_eq!(
        containerd.argv,
        ["containerd", "web", "25", "--namespace", "default"]
    );

    let net = load(&["net"]);
    let output = effective_app_home().join("download.bin");
    let args = vec![
        "https://example.test/download.bin".to_string(),
        output.to_string_lossy().into_owned(),
    ];
    let download = bind_operation_args(&net.operations["download"], &args).unwrap();
    assert_eq!(download.argv, args);
    assert_eq!(
        download.values["output"],
        serde_json::json!(output.to_string_lossy())
    );
}
