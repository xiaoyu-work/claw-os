use super::*;

#[test]
fn recovery_hint_permission_denied() {
    let hint = recovery_hint("Permission denied on /home/cos/file.txt").unwrap();
    assert_eq!(hint["hint"], "Permission denied. Check file permissions.");
    let try_cmds = hint["try"].as_array().unwrap();
    assert!(try_cmds
        .iter()
        .any(|v| v.as_str().unwrap().contains("chmod")));
}

#[test]
fn recovery_hint_eperm_variant() {
    let hint = recovery_hint("EPERM: operation not permitted").unwrap();
    assert_eq!(hint["hint"], "Permission denied. Check file permissions.");
}

#[test]
fn recovery_hint_file_not_found() {
    let hint = recovery_hint("No such file or directory: /home/cos/missing").unwrap();
    assert_eq!(
        hint["hint"],
        "File or command not found. Verify the path exists."
    );
    let try_cmds = hint["try"].as_array().unwrap();
    assert!(try_cmds
        .iter()
        .any(|v| v.as_str().unwrap().contains("cos app fs ls")));
}

#[test]
fn recovery_hint_enoent_variant() {
    let hint = recovery_hint("ENOENT: cannot open /tmp/data").unwrap();
    assert!(hint["hint"].as_str().unwrap().contains("not found"));
}

#[test]
fn recovery_hint_not_found_variant() {
    let hint = recovery_hint("command not found: foobar").unwrap();
    assert!(hint["hint"].as_str().unwrap().contains("not found"));
}

#[test]
fn recovery_hint_disk_full() {
    let hint = recovery_hint("No space left on device").unwrap();
    assert_eq!(hint["hint"], "Disk full. Free space before retrying.");
    let try_cmds = hint["try"].as_array().unwrap();
    assert!(try_cmds
        .iter()
        .any(|v| v.as_str().unwrap().contains("cos sys resources")));
}

#[test]
fn recovery_hint_enospc_variant() {
    let hint = recovery_hint("ENOSPC: write failed").unwrap();
    assert!(hint["hint"].as_str().unwrap().contains("Disk full"));
}

#[test]
fn recovery_hint_connection_refused() {
    let hint = recovery_hint("Connection refused to localhost:8080").unwrap();
    assert!(hint["hint"]
        .as_str()
        .unwrap()
        .contains("Connection refused"));
    let try_cmds = hint["try"].as_array().unwrap();
    assert!(try_cmds
        .iter()
        .any(|v| v.as_str().unwrap().contains("cos service")));
}

#[test]
fn recovery_hint_econnrefused_variant() {
    let hint = recovery_hint("ECONNREFUSED: connect failed").unwrap();
    assert!(hint["hint"]
        .as_str()
        .unwrap()
        .contains("Connection refused"));
}

#[test]
fn recovery_hint_timeout() {
    let hint = recovery_hint("Operation timed out after 30s").unwrap();
    assert!(hint["hint"].as_str().unwrap().contains("timed out"));
}

#[test]
fn recovery_hint_timeout_variant() {
    let hint = recovery_hint("request timeout").unwrap();
    assert!(hint["hint"].as_str().unwrap().contains("timed out"));
}

#[test]
fn recovery_hint_address_in_use() {
    let hint = recovery_hint("address already in use: 0.0.0.0:3000").unwrap();
    assert!(hint["hint"].as_str().unwrap().contains("already in use"));
}

#[test]
fn recovery_hint_eaddrinuse_variant() {
    let hint = recovery_hint("EADDRINUSE: bind failed").unwrap();
    assert!(hint["hint"].as_str().unwrap().contains("already in use"));
}

#[test]
fn recovery_hint_out_of_memory() {
    let hint = recovery_hint("Out of memory: cannot allocate").unwrap();
    assert!(hint["hint"].as_str().unwrap().contains("Out of memory"));
}

#[test]
fn recovery_hint_enomem_variant() {
    let hint = recovery_hint("ENOMEM: mmap failed").unwrap();
    assert!(hint["hint"].as_str().unwrap().contains("Out of memory"));
}

#[test]
fn recovery_hint_oom_variant() {
    let hint = recovery_hint("process killed by OOM killer").unwrap();
    assert!(hint["hint"].as_str().unwrap().contains("Out of memory"));
}

#[test]
fn recovery_hint_unknown_error_returns_none() {
    assert!(recovery_hint("something completely unexpected happened").is_none());
}

#[test]
fn recovery_hint_empty_string_returns_none() {
    assert!(recovery_hint("").is_none());
}

#[test]
fn recovery_hint_case_insensitive() {
    // Should match regardless of case
    assert!(recovery_hint("PERMISSION DENIED").is_some());
    assert!(recovery_hint("permission denied").is_some());
    assert!(recovery_hint("Permission Denied").is_some());
}

#[test]
fn recovery_hint_returns_valid_json_structure() {
    // Every hint should have both "hint" (string) and "try" (array of strings)
    let test_errors = [
        "permission denied",
        "no such file",
        "no space left",
        "connection refused",
        "timed out",
        "address already in use",
        "out of memory",
    ];
    for error in &test_errors {
        let hint = recovery_hint(error).unwrap_or_else(|| panic!("Expected hint for '{}'", error));
        assert!(
            hint["hint"].is_string(),
            "Missing 'hint' string for '{}'",
            error
        );
        assert!(
            hint["try"].is_array(),
            "Missing 'try' array for '{}'",
            error
        );
        let try_arr = hint["try"].as_array().unwrap();
        assert!(!try_arr.is_empty(), "Empty 'try' array for '{}'", error);
        for cmd in try_arr {
            assert!(cmd.is_string(), "Non-string in 'try' array for '{}'", error);
            assert!(
                cmd.as_str().unwrap().starts_with("cos "),
                "Recovery command should start with 'cos': {}",
                cmd
            );
        }
    }
}

#[test]
fn schema_for_known_builtin() {
    let schemas = command_schemas();
    assert!(schemas.iter().any(|(n, _, _)| *n == "checkpoint"));
    assert!(schemas.iter().any(|(n, _, _)| *n == "credential"));
    assert!(schemas.iter().any(|(n, _, _)| *n == "cron"));
    assert!(schemas.iter().any(|(n, _, _)| *n == "service"));
}

#[test]
fn perms_namespace_is_not_user_facing() {
    let result = dispatch(&["perms".into(), "check".into(), "ui.notify".into()]);
    assert!(result.is_err());
}

#[test]
fn hidden_policy_bridge_remains_available_to_runtimes() {
    let _lock = crate::test_env::lock_env();
    let prev_sess = std::env::var_os("COS_SESSION");
    let prev_mode = std::env::var_os("COS_PERMS_MODE");
    std::env::remove_var("COS_SESSION");
    std::env::set_var("COS_PERMS_MODE", "permissive");

    let output = dispatch(&["__policy".into(), "check".into(), "ui.notify".into()])
        .expect("hidden policy bridge should dispatch")
        .expect("hidden policy bridge should return JSON");
    let v: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(v["decision"], "allow");

    match prev_sess {
        Some(value) => std::env::set_var("COS_SESSION", value),
        None => std::env::remove_var("COS_SESSION"),
    }
    match prev_mode {
        Some(value) => std::env::set_var("COS_PERMS_MODE", value),
        None => std::env::remove_var("COS_PERMS_MODE"),
    }
}

#[test]
fn show_command_schema_returns_json() {
    let result = show_command_schema("checkpoint", "create");
    assert!(result.is_ok());
    let output = result.unwrap().unwrap();
    let v: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(v["command"], "cos checkpoint create");
    assert!(v["parameters"].is_array());
    assert!(v["example"].is_string());
}

#[test]
fn show_builtin_schema_returns_all_commands() {
    let result = show_builtin_schema("credential");
    assert!(result.is_ok());
    let output = result.unwrap().unwrap();
    let v: Value = serde_json::from_str(&output).unwrap();
    assert!(v["commands"].is_array());
    assert!(v["commands"].as_array().unwrap().len() > 3);
}

#[test]
fn show_command_schema_unknown_returns_error() {
    let result = show_command_schema("nonexistent", "cmd");
    assert!(result.is_err());
}

#[test]
fn show_command_schema_unknown_command_returns_error() {
    let result = show_command_schema("checkpoint", "nonexistent");
    assert!(result.is_err());
}

#[test]
fn show_command_schema_has_param_details() {
    let result = show_command_schema("checkpoint", "create");
    let output = result.unwrap().unwrap();
    let v: Value = serde_json::from_str(&output).unwrap();
    let params = v["parameters"].as_array().unwrap();
    assert!(!params.is_empty());
    // Each param should have name, type, required, description, kind
    for p in params {
        assert!(p["name"].is_string());
        assert!(p["type"].is_string());
        assert!(p["required"].is_boolean());
        assert!(p["description"].is_string());
        assert!(
            p["kind"] == "positional" || p["kind"] == "flag",
            "kind must be positional or flag, got: {}",
            p["kind"]
        );
    }
}

#[test]
fn show_builtin_schema_all_primitives() {
    // Every primitive that has a schema should produce valid output
    let primitives = ["checkpoint", "credential", "cron", "service", "sys"];
    for name in &primitives {
        let result = show_builtin_schema(name);
        assert!(result.is_ok(), "Failed for primitive: {name}");
        let output = result.unwrap().unwrap();
        let v: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["app"], *name);
        assert!(v["description"].is_string());
        assert!(v["commands"].is_array());
        assert!(
            !v["commands"].as_array().unwrap().is_empty(),
            "No commands for: {name}"
        );
    }
}

#[test]
fn error_code_from_hint_maps_correctly() {
    assert_eq!(
        error_code_from_hint("Permission denied on /etc"),
        Some(crate::errors::IO_PERMISSION_DENIED)
    );
    assert_eq!(
        error_code_from_hint("No such file: /missing"),
        Some(crate::errors::IO_FILE_NOT_FOUND)
    );
    assert_eq!(
        error_code_from_hint("connection refused"),
        Some(crate::errors::IO_CONNECTION_REFUSED)
    );
    assert_eq!(
        error_code_from_hint("No space left on device"),
        Some(crate::errors::IO_DISK_FULL)
    );
    assert_eq!(
        error_code_from_hint("Operation timed out"),
        Some(crate::errors::LIMIT_TIMEOUT)
    );
    assert_eq!(
        error_code_from_hint("address already in use"),
        Some(crate::errors::RESOURCE_BUSY)
    );
    assert_eq!(
        error_code_from_hint("out of memory"),
        Some(crate::errors::LIMIT_OOM)
    );
    assert_eq!(error_code_from_hint("something random"), None);
}

fn parse(out: Option<String>) -> Value {
    serde_json::from_str(&out.expect("dispatch returned None")).expect("not JSON")
}

#[test]
fn dispatch_help_flag_returns_overview() {
    let v = parse(dispatch(&["--help".into()]).unwrap());
    assert_eq!(v["name"], "cos");
    assert!(v["primitives"].is_array());
}

#[test]
fn dispatch_h_short_flag_returns_overview() {
    let v = parse(dispatch(&["-h".into()]).unwrap());
    assert_eq!(v["name"], "cos");
}

#[test]
fn dispatch_bare_help_returns_overview() {
    let v = parse(dispatch(&["help".into()]).unwrap());
    assert!(v["primitives"].is_array());
}

#[test]
fn dispatch_help_topic_returns_primitive() {
    let v = parse(dispatch(&["help".into(), "sys".into()]).unwrap());
    assert_eq!(v["app"], "sys");
    assert!(v["commands"].is_object());
}

#[test]
fn dispatch_help_unknown_topic_returns_overview_with_note() {
    let v = parse(dispatch(&["help".into(), "nope".into()]).unwrap());
    assert!(v["primitives"].is_array());
    assert!(v["note"].as_str().unwrap().contains("unknown help topic"));
}

#[test]
fn dispatch_builtin_null_result_yields_no_output() {
    // A handler that has already written its human-facing output
    // directly to stdout (e.g. `cos agent ask` printing the plain
    // answer) returns Value::Null to signal "nothing more to
    // render". dispatch_builtin must surface that as Ok(None) so
    // main.rs does not print a stray `null` line afterwards.
    fn silent(_command: &str, _args: &[String]) -> Result<Value, String> {
        Ok(Value::Null)
    }
    let result = dispatch_builtin(&["agent".into(), "ask".into()], "agent", silent);
    assert!(matches!(result, Ok(None)));
}

#[test]
fn dispatch_builtin_recovery_envelope_propagates_failure() {
    // Regression: a builtin handler that returns Err with a string
    // matching a `recovery_hint` pattern (e.g. "Permission denied"
    // when writing config as a non-root user) used to
    // be re-wrapped in `Ok(Some(envelope))`. That zeroed out the CLI
    // exit code, so callers like cosmic-settings' agent page parsed
    // the failure as a default-valued success and silently flipped
    // the provider back to openai. The wrapper must keep failures
    // failing while still attaching the recovery hints.
    fn boom(_command: &str, _args: &[String]) -> Result<Value, String> {
        Err("write /var/lib/foo.tmp: Permission denied (os error 13)".into())
    }
    let result = dispatch_builtin(&["agent".into(), "boom".into()], "agent", boom);
    let err = result.expect_err("dispatch_builtin must propagate Err for failed primitives");
    let v: Value = serde_json::from_str(&err).expect("recovery envelope must be JSON");
    assert!(
        v["error"].as_str().unwrap().contains("Permission denied"),
        "error preserved: {v}"
    );
    assert!(v["recovery"].is_object(), "recovery attached: {v}");
    assert_eq!(
        v["code"].as_str(),
        Some(crate::errors::IO_PERMISSION_DENIED),
        "structured error code attached: {v}"
    );
}

#[test]
fn dispatch_version_returns_envelope() {
    for flag in ["--version", "-v", "-V"] {
        let v = parse(dispatch(&[flag.into()]).unwrap());
        assert_eq!(v["name"], "cos");
        assert_eq!(v["version"], VERSION);
    }
}

#[test]
fn dispatch_builtin_help_token_returns_overview() {
    for flag in ["--help", "-h", "help"] {
        let v = parse(dispatch(&["sys".into(), flag.into()]).unwrap());
        assert_eq!(v["app"], "sys", "flag: {flag}");
        assert!(v["commands"].is_object());
    }
}

#[test]
fn dispatch_agent_help_does_not_hijack() {
    // `cos agent --help` must return the command list rather than
    // dropping into the interactive chat/setup shortcut.
    let v = parse(dispatch(&["agent".into(), "--help".into()]).unwrap());
    assert_eq!(v["app"], "agent");
    assert!(v["commands"].is_object());
}

#[test]
fn browser_module_compiles() {
    // cos browser is no longer a user CLI primitive — it's exposed
    // only as the `cos_browser` agent tool. Smoke-test that the module
    // is still wired up by reaching the unknown-command path.
    let err = crate::browser::run("__nope__", &[]).unwrap_err();
    assert!(err.contains("unknown"));
}

// -----------------------------------------------------------------
// `cos app consent` CLI surface — see consent_cmd() above.
// -----------------------------------------------------------------

fn empty_apps() -> std::collections::BTreeMap<String, apps::App> {
    std::collections::BTreeMap::new()
}

#[test]
fn consent_help_lists_subcommands() {
    let v = parse(consent_cmd(&[], &empty_apps()).unwrap());
    assert_eq!(v["app"], "consent");
    let subs = v["subcommands"].as_object().unwrap();
    for k in ["list", "show", "path", "grant", "revoke"] {
        assert!(subs.contains_key(k), "missing subcommand {k}");
    }
}

#[test]
fn consent_path_returns_user_config_path() {
    let v = parse(consent_cmd(&["path".into(), "myapp".into()], &empty_apps()).unwrap());
    assert_eq!(v["app"], "myapp");
    let p = v["path"].as_str().unwrap();
    assert!(p.contains("consents"));
    assert!(p.ends_with("myapp.json"));
}

#[test]
fn consent_show_missing_file_reports_absent() {
    let tmp = std::env::temp_dir().join(format!("cos-consent-router-show-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
    let v = parse(consent_cmd(&["show".into(), "never-granted".into()], &empty_apps()).unwrap());
    match prev {
        Some(x) => std::env::set_var("COS_USER_CONFIG_DIR", x),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(v["present"], false);
    assert!(v["consent"].is_null());
}

#[test]
fn consent_grant_unknown_app_errors() {
    let err = consent_cmd(
        &["grant".into(), "ghost".into(), "--yes".into()],
        &empty_apps(),
    )
    .unwrap_err();
    assert!(err.contains("unknown app"));
    assert!(err.contains("ghost"));
}

#[test]
fn consent_revoke_missing_file_is_noop() {
    let tmp =
        std::env::temp_dir().join(format!("cos-consent-router-revoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
    let v = parse(consent_cmd(&["revoke".into(), "never-granted".into()], &empty_apps()).unwrap());
    match prev {
        Some(x) => std::env::set_var("COS_USER_CONFIG_DIR", x),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(v["revoked"], false);
}

#[test]
fn consent_grant_yes_writes_record_and_show_reads_it_back() {
    use crate::caps::manifest::{AiBudget, AiPolicy, AiSafety, Manifest, PromptOrigin, Runtime};
    use crate::i18n::LocalizedText;
    use std::collections::BTreeMap;

    let tmp = std::env::temp_dir().join(format!("cos-consent-router-grant-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);

    let manifest = Manifest {
        id: "demo".into(),
        version: "0.0.1".into(),
        name: LocalizedText::en("Demo"),
        summary: LocalizedText::default(),
        icon: None,
        runtime: Runtime::default(),
        entry: None,
        operations: BTreeMap::new(),
        ai: Some(AiPolicy {
            budget: AiBudget {
                monthly_units: 1000,
            },
            safety: AiSafety::Standard,
            origins: vec![PromptOrigin::Trusted],
            tools: Vec::new(),
        }),
        session: None,
        desktop: None,
        dependencies: serde_json::Value::Null,
    };
    let mut discovered = std::collections::BTreeMap::new();
    discovered.insert(
        "demo".to_string(),
        apps::App {
            manifest,
            dir: tmp.join("does-not-matter"),
        },
    );

    let granted = parse(
        consent_cmd(
            &["grant".into(), "demo".into(), "--yes".into()],
            &discovered,
        )
        .unwrap(),
    );
    assert_eq!(granted["granted"], true);
    assert_eq!(granted["app"], "demo");

    let shown = parse(consent_cmd(&["show".into(), "demo".into()], &discovered).unwrap());
    assert_eq!(shown["present"], true);
    assert_eq!(shown["consent"]["policy"]["budget"]["monthly_units"], 1000);

    let listed = parse(consent_cmd(&["list".into()], &discovered).unwrap());
    let rows = listed["consents"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["app"], "demo");
    assert_eq!(rows[0]["status"], "fresh");

    let revoked = parse(consent_cmd(&["revoke".into(), "demo".into()], &discovered).unwrap());
    assert_eq!(revoked["revoked"], true);

    match prev {
        Some(x) => std::env::set_var("COS_USER_CONFIG_DIR", x),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

// -----------------------------------------------------------------
// `cos app install` CLI surface — see install_cmd() above.
// -----------------------------------------------------------------

fn write_min_app(dir: &std::path::Path, id: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("app.json"), body).unwrap();
    // A tiny main.py so the copy step has something to move.
    std::fs::write(dir.join("main.py"), format!("# stub for {id}\n")).unwrap();
}

#[test]
fn install_generates_desktop_entry_for_gui_app() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-gui-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-gui-dst-{pid}"));
    let apps_share = std::env::temp_dir().join(format!("cos-install-gui-apps-{pid}"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_dir_all(&apps_share);
    write_min_app(
        &src,
        "notes",
        r#"{
              "id": "notes",
              "version": "0.0.1",
              "name": "Notes",
              "desktop": {
                "icon": "notes",
                "categories": ["Utility"],
                "mime_types": ["text/markdown"]
              }
            }"#,
    );

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    let prev_share = std::env::var_os("COS_APPLICATIONS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    std::env::set_var("COS_APPLICATIONS_DIR", &apps_share);
    let v = parse(install_cmd(&[src.display().to_string()]).unwrap());
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    match prev_share {
        Some(x) => std::env::set_var("COS_APPLICATIONS_DIR", x),
        None => std::env::remove_var("COS_APPLICATIONS_DIR"),
    }

    assert_eq!(v["installed"], true);
    assert_eq!(v["desktop"]["generated"], true);
    let entry = apps_share.join("com.clawos.notes.desktop");
    assert!(entry.is_file(), "expected {} to exist", entry.display());
    let body = std::fs::read_to_string(&entry).unwrap();
    assert!(
        body.contains("Exec=cos app notes --gui %F"),
        "Exec must route through `cos app`; got:\n{body}"
    );
    assert!(body.contains("Categories=ClawOS;Utility;"), "got:\n{body}");
    assert!(body.contains("MimeType=text/markdown;"), "got:\n{body}");
    assert!(body.contains("X-CLAW-App-Id=notes"), "got:\n{body}");

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_dir_all(&apps_share);
}

#[test]
fn install_skips_desktop_entry_for_headless_app() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-headless-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-headless-dst-{pid}"));
    let apps_share = std::env::temp_dir().join(format!("cos-install-headless-apps-{pid}"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_dir_all(&apps_share);
    write_min_app(
        &src,
        "calc",
        r#"{"id":"calc","version":"0.0.1","name":"Calc"}"#,
    );

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    let prev_share = std::env::var_os("COS_APPLICATIONS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    std::env::set_var("COS_APPLICATIONS_DIR", &apps_share);
    let v = parse(install_cmd(&[src.display().to_string()]).unwrap());
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    match prev_share {
        Some(x) => std::env::set_var("COS_APPLICATIONS_DIR", x),
        None => std::env::remove_var("COS_APPLICATIONS_DIR"),
    }

    assert_eq!(v["installed"], true);
    assert!(
        v.get("desktop").is_none(),
        "headless app must not emit a launcher"
    );
    assert!(!apps_share.join("com.clawos.calc.desktop").exists());

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_dir_all(&apps_share);
}

#[test]
fn install_requires_source() {
    let err = install_cmd(&[]).unwrap_err();
    assert!(err.contains("usage:"), "got: {err}");
}

#[test]
fn install_rejects_non_directory_source() {
    let err = install_cmd(&["/dev/null".into()]).unwrap_err();
    assert!(err.contains("not a directory"), "got: {err}");
}

#[test]
fn create_scaffolds_cli_app_without_desktop() {
    let pid = std::process::id();
    let parent = std::env::temp_dir().join(format!("cos-create-cli-{pid}"));
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(&parent).unwrap();

    let v =
        parse(create_cmd(&["mycli".into(), "--dir".into(), parent.display().to_string()]).unwrap());
    assert_eq!(v["created"], true);
    assert_eq!(v["kind"], "cli");

    let app_dir = parent.join("mycli");
    let manifest = std::fs::read_to_string(app_dir.join("app.json")).unwrap();
    // The generated manifest must parse + validate exactly as the
    // kernel would at discover/install time.
    let parsed = apps::AppManifest::from_json(&manifest).unwrap();
    parsed.validate().unwrap();
    assert!(parsed.operations.contains_key("greet"));
    assert!(parsed.desktop.is_none(), "cli kind must omit desktop");

    let entry = std::fs::read_to_string(app_dir.join("main.py")).unwrap();
    assert!(entry.contains("def run(command, args):"));
    assert!(
        !entry.contains("is_gui_launch"),
        "cli stub has no GUI branch"
    );

    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn create_scaffolds_both_surfaces() {
    let pid = std::process::id();
    let parent = std::env::temp_dir().join(format!("cos-create-both-{pid}"));
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(&parent).unwrap();

    let v = parse(
        create_cmd(&[
            "notes".into(),
            "--kind".into(),
            "both".into(),
            "--dir".into(),
            parent.display().to_string(),
        ])
        .unwrap(),
    );
    assert_eq!(v["kind"], "both");

    let app_dir = parent.join("notes");
    let parsed =
        apps::AppManifest::from_json(&std::fs::read_to_string(app_dir.join("app.json")).unwrap())
            .unwrap();
    parsed.validate().unwrap();
    assert!(parsed.operations.contains_key("greet"));
    assert!(parsed.desktop.is_some(), "both kind must include desktop");

    let entry = std::fs::read_to_string(app_dir.join("main.py")).unwrap();
    assert!(entry.contains("gui.is_gui_launch(command)"));
    assert!(entry.contains("from claw_os_sdk import gui"));

    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn create_desktop_kind_is_gui_only() {
    let pid = std::process::id();
    let parent = std::env::temp_dir().join(format!("cos-create-desktop-{pid}"));
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(&parent).unwrap();

    create_cmd(&[
        "viewer".into(),
        "--kind".into(),
        "desktop".into(),
        "--dir".into(),
        parent.display().to_string(),
    ])
    .unwrap();

    let parsed = apps::AppManifest::from_json(
        &std::fs::read_to_string(parent.join("viewer").join("app.json")).unwrap(),
    )
    .unwrap();
    parsed.validate().unwrap();
    assert!(
        parsed.operations.is_empty(),
        "desktop kind has no operations"
    );
    assert!(parsed.desktop.is_some());

    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn create_rejects_invalid_id() {
    let err = create_cmd(&["Bad-ID".into()]).unwrap_err();
    assert!(err.contains("invalid app id"), "got: {err}");
}

#[test]
fn create_rejects_unknown_kind() {
    let err = create_cmd(&["x".into(), "--kind".into(), "wat".into()]).unwrap_err();
    assert!(err.contains("unknown --kind"), "got: {err}");
}

#[test]
fn create_requires_id() {
    let err = create_cmd(&[]).unwrap_err();
    assert!(err.contains("usage:"), "got: {err}");
}

#[test]
fn create_refuses_existing_dir_without_force() {
    let pid = std::process::id();
    let parent = std::env::temp_dir().join(format!("cos-create-exists-{pid}"));
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(parent.join("dup")).unwrap();
    let err =
        create_cmd(&["dup".into(), "--dir".into(), parent.display().to_string()]).unwrap_err();
    assert!(err.contains("already exists"), "got: {err}");
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn install_rejects_missing_manifest() {
    let tmp = std::env::temp_dir().join(format!("cos-install-no-manifest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let err = install_cmd(&[tmp.display().to_string()]).unwrap_err();
    assert!(err.contains("no app.json"), "got: {err}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn install_rejects_unknown_tool_in_manifest() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-bad-tool-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-bad-tool-dst-{pid}"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    write_min_app(
        &src,
        "bad",
        r#"{
              "id": "bad",
              "version": "0.0.1",
              "name": "Bad",
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": ["trusted"],
                "tools": ["fs.unicorn"]
              }
            }"#,
    );

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    let err = install_cmd(&[src.display().to_string()]).unwrap_err();
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);

    assert!(err.contains("manifest catalog check"), "got: {err}");
    assert!(err.contains("fs.unicorn"), "got: {err}");
}

#[test]
fn install_copies_app_without_ai_block_and_skips_consent() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-noai-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-noai-dst-{pid}"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    write_min_app(
        &src,
        "calc",
        r#"{
              "id": "calc",
              "version": "0.0.1",
              "name": "Calc"
            }"#,
    );

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    let v = parse(install_cmd(&[src.display().to_string()]).unwrap());
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert_eq!(v["installed"], true);
    assert_eq!(v["app"], "calc");
    assert_eq!(v["copied"], true);
    assert_eq!(v["consent"]["needed"], false);
    assert!(dst.join("calc").join("app.json").is_file());
    assert!(dst.join("calc").join("main.py").is_file());

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
}

#[test]
fn install_no_consent_defers_consent_for_ai_app() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-defer-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-defer-dst-{pid}"));
    let cfg = std::env::temp_dir().join(format!("cos-install-defer-cfg-{pid}"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_dir_all(&cfg);
    write_min_app(
        &src,
        "summ",
        r#"{
              "id": "summ",
              "version": "0.0.1",
              "name": "Summ",
              "ai": {
                "budget": {"monthly_units": 100},
                "safety": "strict",
                "origins": ["trusted"],
                "tools": ["fs.read_text"]
              }
            }"#,
    );

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    let prev_cfg = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    std::env::set_var("COS_USER_CONFIG_DIR", &cfg);
    let v = parse(install_cmd(&[src.display().to_string(), "--no-consent".into()]).unwrap());
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    match prev_cfg {
        Some(x) => std::env::set_var("COS_USER_CONFIG_DIR", x),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }

    assert_eq!(v["installed"], true);
    assert_eq!(v["consent"]["needed"], true);
    assert_eq!(v["consent"]["granted"], false);
    assert_eq!(v["consent"]["deferred"], true);
    assert!(dst.join("summ").join("app.json").is_file());

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_dir_all(&cfg);
}

#[test]
fn install_yes_grants_consent_for_ai_app() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-yes-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-yes-dst-{pid}"));
    let cfg = std::env::temp_dir().join(format!("cos-install-yes-cfg-{pid}"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_dir_all(&cfg);
    write_min_app(
        &src,
        "yes",
        r#"{
              "id": "yes",
              "version": "0.0.1",
              "name": "Yes",
              "ai": {
                "budget": {"monthly_units": 100},
                "safety": "strict",
                "origins": ["trusted"]
              }
            }"#,
    );

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    let prev_cfg = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    std::env::set_var("COS_USER_CONFIG_DIR", &cfg);
    let v = parse(install_cmd(&[src.display().to_string(), "--yes".into()]).unwrap());
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    match prev_cfg {
        Some(x) => std::env::set_var("COS_USER_CONFIG_DIR", x),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }

    assert_eq!(v["installed"], true);
    assert_eq!(v["consent"]["needed"], true);
    assert_eq!(v["consent"]["granted"], true);
    assert!(v["consent"]["approved_at"].is_string());

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_dir_all(&cfg);
}

#[test]
fn install_refuses_to_overwrite_without_force() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-overw-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-overw-dst-{pid}"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    write_min_app(
        &src,
        "twice",
        r#"{
              "id": "twice",
              "version": "0.0.1",
              "name": "Twice"
            }"#,
    );
    std::fs::create_dir_all(dst.join("twice")).unwrap();
    std::fs::write(dst.join("twice").join("placeholder"), b"existing").unwrap();

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    let err = install_cmd(&[src.display().to_string()]).unwrap_err();
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);

    assert!(err.contains("already exists"), "got: {err}");
    assert!(err.contains("--force"), "got: {err}");
}

#[test]
fn install_force_replaces_existing_install() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-force-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-force-dst-{pid}"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    write_min_app(
        &src,
        "force",
        r#"{
              "id": "force",
              "version": "0.0.1",
              "name": "Force"
            }"#,
    );
    std::fs::create_dir_all(dst.join("force")).unwrap();
    std::fs::write(dst.join("force").join("stale"), b"junk").unwrap();

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    let v = parse(install_cmd(&[src.display().to_string(), "--force".into()]).unwrap());
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert_eq!(v["installed"], true);
    assert_eq!(v["copied"], true);
    assert!(dst.join("force").join("app.json").is_file());
    assert!(
        !dst.join("force").join("stale").is_file(),
        "--force must clear the old tree before copying"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
}

/// Regression: a symlink anywhere in the install source tree must
/// be rejected. Otherwise an attacker who can plant a link inside a
/// "trusted developer tree" can either copy out-of-tree files (e.g.
/// `/etc/shadow`, the system credential store) into the installed
/// App location, or escape the source tree during recursion.
#[cfg(unix)]
#[test]
fn install_rejects_symlink_in_source_tree() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-symlink-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-symlink-dst-{pid}"));
    let outside = std::env::temp_dir().join(format!("cos-install-symlink-outside-{pid}"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_file(&outside);

    write_min_app(
        &src,
        "linky",
        r#"{
              "id": "linky",
              "version": "0.0.1",
              "name": "Linky"
            }"#,
    );

    // Create a target outside the source tree we wouldn't want
    // materialised inside the App.
    std::fs::write(&outside, b"secret-bytes-not-meant-for-this-app").unwrap();
    // Plant a symlink in the source tree pointing at the outside
    // target. With the old `fs::copy` traversal this would be
    // copied verbatim under `dst/linky/secret`.
    std::os::unix::fs::symlink(&outside, src.join("secret")).unwrap();

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    let err = install_cmd(&[src.display().to_string()]).unwrap_err();
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert!(
        err.contains("symlink"),
        "expected symlink rejection error, got: {err}"
    );
    // The installed dest must not exist (or at minimum must not
    // contain the would-be copied symlink target).
    let leaked = dst.join("linky").join("secret");
    assert!(
        !leaked.is_file(),
        "symlink target must not have been materialised in install dest"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_file(&outside);
}
