use super::*;

fn install_cmd(args: &[String]) -> Result<Option<String>, String> {
    install_cmd_with_confirmation(args, &mut |app, _source, _auto_yes, _dev_trust| {
        apps::permission_review::PermissionReview::from_manifest(&app.manifest)
            .map(|review| (review, None))
    })
}

fn parse(out: Option<String>) -> Value {
    serde_json::from_str(&out.unwrap()).unwrap()
}

fn write_min_app(dir: &Path, id: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("app.json"), body).unwrap();
    std::fs::write(dir.join("main.py"), format!("# stub for {id}\n")).unwrap();
    crate::test_env::sign_test_package(dir, crate::provenance::PackageKind::App, id);
}

fn reseal_app(dir: &Path, id: &str) {
    let _ = std::fs::remove_file(dir.join(crate::provenance::envelope::ENVELOPE_FILE));
    crate::test_env::sign_test_package(dir, crate::provenance::PackageKind::App, id);
}

fn write_unsigned_app(dir: &Path, id: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("app.json"), body).unwrap();
    std::fs::write(dir.join("main.py"), format!("# stub for {id}\n")).unwrap();
}

fn install_scratch_entries(root: &Path, id: &str) -> Vec<String> {
    let prefix = format!(".{id}.install-");
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| name.starts_with(&prefix))
        .collect()
}

#[test]
fn lint_cli_projects_the_shared_violations_and_preserves_its_envelope() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("linted");
    write_min_app(
        &directory,
        "linted",
        r#"{
            "schema_version":2,"id":"linted","version":"1.0.0","name":{"en":"Linted"},
            "mcp":{"tools":[{"name":"linted.run","summary":{"en":"Run"}}]}
        }"#,
    );
    std::fs::write(directory.join("main.py"), "import openai\n").unwrap();
    reseal_app(&directory, "linted");
    let discovered = apps::discover(root.path());
    let violations = apps::lint::app_lint_violations(&discovered["linted"]);
    assert_eq!(violations.len(), 2);
    let expected = json!({
        "results": [{"app":"linted","ok":false,"violations":violations}],
        "ok": false,
        "hint": "Lint failed. Apps must (a) route AI calls through `claw_os_sdk.ai` (not direct provider SDKs) \
                 and (b) ship every package-relative file referenced by their `mcp.entry` so the kernel agent can spawn \
                 the MCP server. Run `cos app tool list <app>` to inspect the declared tool surface.",
    });
    for target in [None, Some("linted")] {
        assert_eq!(parse(lint_apps(&discovered, target).unwrap()), expected);
    }
    assert_eq!(
        lint_apps(&discovered, Some("absent")).unwrap_err(),
        "unknown app: absent. installed: [\"linted\"]"
    );

    std::fs::write(directory.join("main.py"), "from claw_os_sdk import ai\n").unwrap();
    std::fs::write(directory.join("server.py"), "# static fixture\n").unwrap();
    reseal_app(&directory, "linted");
    let discovered = apps::discover(root.path());
    assert_eq!(
        parse(lint_apps(&discovered, Some("linted")).unwrap()),
        json!({
            "results": [{"app":"linted","ok":true,"violations":[]}],
            "ok": true,
            "hint": "All apps route their AI calls through the kernel gate and ship every declared MCP entry.",
        })
    );
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
              "name": {"en": "Notes"},
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
    let v = parse(install_cmd(&[src.display().to_string(), "--yes".into()]).unwrap());
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
        r#"{"id":"calc","version":"0.0.1","name": {"en": "Calc"}}"#,
    );

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    let prev_share = std::env::var_os("COS_APPLICATIONS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    std::env::set_var("COS_APPLICATIONS_DIR", &apps_share);
    let v = parse(install_cmd(&[src.display().to_string(), "--yes".into()]).unwrap());
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
fn install_yes_cannot_replace_os_confirmation_when_the_broker_is_unavailable() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("installed");
    write_min_app(
        &source,
        "needs-os-review",
        r#"{"id":"needs-os-review","version":"1.0.0","name":{"en":"Requires OS review"}}"#,
    );
    let _apps = crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", &destination);
    let _socket =
        crate::test_env::TestEnvVarGuard::set("CLAWD_SOCKET", root.path().join("missing.sock"));
    for flags in [
        vec![source.display().to_string(), "--yes".to_string()],
        vec![
            source.display().to_string(),
            "--yes".to_string(),
            "--no-consent".to_string(),
        ],
    ] {
        let error = super::install_cmd(&flags).unwrap_err();
        assert!(
            error.contains("system review service is unavailable"),
            "{error}"
        );
        assert!(!destination.join("needs-os-review").exists());
    }
}

#[test]
fn install_review_only_reports_permissions_without_publishing() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("installed");
    write_min_app(
        &source,
        "review",
        r#"{
            "id": "review", "version": "1.0.0", "name": {"en": "Review"},
            "operations": {
                "fetch": {
                    "label": {"en": "Fetch"},
                    "args": [{"name": "url", "kind": "text", "required": true}],
                    "needs": [{
                        "verb": "net.dial",
                        "scope": {"kind": "from-arg", "arg": "url", "transform": "url-host"},
                        "why": {"en": "Connect to the selected service"}
                    }]
                }
            }
        }"#,
    );
    let previous = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &destination);
    let result = install_cmd(&[
        source.display().to_string(),
        "--review".into(),
        "--yes".into(),
    ]);
    match previous {
        Some(value) => std::env::set_var("COS_APPS_DIR", value),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    let result = parse(result.unwrap());
    assert_eq!(result["installed"], false);
    assert_eq!(result["review_only"], true);
    assert_eq!(result["permission_review"]["permissions_granted"], false);
    assert_eq!(
        result["permission_review"]["permissions"][0]["verb"],
        "net.dial"
    );
    assert_eq!(
        result["permission_review"]["permissions"][0]["scope"]["kind"],
        "from-arg"
    );
    assert!(result["provenance"].is_object());
    assert!(!destination.exists());
}

#[test]
fn install_review_cannot_trust_unsigned_metadata_via_dev_flag() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("installed");
    write_unsigned_app(
        &source,
        "unsigned-review",
        r#"{"id":"unsigned-review","version":"1.0.0","name":{"en":"Unverified"}}"#,
    );
    let previous = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &destination);
    let result = install_cmd(&[
        source.display().to_string(),
        "--review".into(),
        "--dev-trust".into(),
    ]);
    match previous {
        Some(value) => std::env::set_var("COS_APPS_DIR", value),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    assert!(result.is_err());
    assert!(!destination.exists());
}

#[test]
fn install_rejects_non_directory_source() {
    let err = install_cmd(&["/dev/null".into()]).unwrap_err();
    assert!(err.contains("not a directory"), "got: {err}");
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
              "name": {"en": "Bad"},
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
fn install_copies_non_ai_app_with_permission_disclosure() {
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
              "name": {"en": "Calc"}
            }"#,
    );

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    let v = parse(install_cmd(&[src.display().to_string(), "--yes".into()]).unwrap());
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert_eq!(v["installed"], true);
    assert_eq!(v["app"], "calc");
    assert_eq!(v["copied"], true);
    assert_eq!(v["consent"]["needed"], false);
    assert_eq!(v["permission_review"]["permissions_granted"], false);
    assert_eq!(v["permission_review"]["permissions"], json!([]));
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
              "name": {"en": "Summ"},
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
    let v = parse(
        install_cmd(&[
            src.display().to_string(),
            "--no-consent".into(),
            "--yes".into(),
        ])
        .unwrap(),
    );
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
    assert_eq!(v["permission_review"]["permissions_granted"], false);
    assert_eq!(
        v["permission_review"]["ai_policy"]["budget"]["monthly_units"],
        100
    );
    assert!(dst.join("summ").join("app.json").is_file());

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_dir_all(&cfg);
}

#[test]
fn install_yes_does_not_grant_ai_consent() {
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
              "name": {"en": "Yes"},
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
    assert_eq!(v["consent"]["granted"], false);
    assert_eq!(v["consent"]["deferred"], true);
    assert_eq!(v["permission_review"]["permissions_granted"], false);
    assert!(v["consent"].get("approved_at").is_none());

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
              "name": {"en": "Twice"}
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
              "name": {"en": "Force"}
            }"#,
    );
    std::fs::create_dir_all(dst.join("force")).unwrap();
    std::fs::write(dst.join("force").join("stale"), b"junk").unwrap();

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    let v =
        parse(install_cmd(&[src.display().to_string(), "--force".into(), "--yes".into()]).unwrap());
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

#[test]
fn install_force_lint_failure_preserves_existing_install() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-atomic-lint-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-atomic-lint-dst-{pid}"));
    let installed = dst.join("atomic");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);

    write_min_app(
        &installed,
        "atomic",
        r#"{"id":"atomic","version":"0.0.1","name": {"en": "Atomic"}}"#,
    );
    std::fs::write(installed.join("old-state"), b"still usable").unwrap();
    reseal_app(&installed, "atomic");
    write_min_app(
        &src,
        "atomic",
        r#"{"id":"atomic","version":"0.0.2","name": {"en": "Atomic"}}"#,
    );
    std::fs::write(src.join("main.py"), b"import openai\n").unwrap();
    reseal_app(&src, "atomic");

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    let err = install_cmd(&[src.display().to_string(), "--force".into()]).unwrap_err();
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert!(err.contains("staged app lint failed"), "got: {err}");
    assert_eq!(
        std::fs::read_to_string(installed.join("old-state")).unwrap(),
        "still usable"
    );
    assert!(std::fs::read_to_string(installed.join("app.json"))
        .unwrap()
        .contains(r#""version":"0.0.1""#));
    assert!(
        install_scratch_entries(&dst, "atomic").is_empty(),
        "failed install must clean staging and backup directories"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
}

#[test]
fn install_recovers_backup_left_by_interrupted_forced_install() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-recovery-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-recovery-dst-{pid}"));
    let installed = dst.join("recover");
    let backup = dst.join(".recover.install-backup-interrupted");
    let staging = dst.join(".recover.install-staging-interrupted");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);

    write_min_app(
        &backup,
        "recover",
        r#"{"id":"recover","version":"0.0.1","name": {"en": "Recover"}}"#,
    );
    std::fs::write(backup.join("old-state"), b"recovered").unwrap();
    reseal_app(&backup, "recover");
    write_min_app(
        &staging,
        "recover",
        r#"{"id":"recover","version":"0.0.2","name": {"en": "Recover"}}"#,
    );
    write_min_app(
        &src,
        "recover",
        r#"{"id":"recover","version":"0.0.3","name": {"en": "Recover"}}"#,
    );
    std::fs::write(src.join("main.py"), b"import openai\n").unwrap();
    reseal_app(&src, "recover");

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &dst);
    let err = install_cmd(&[src.display().to_string(), "--force".into()]).unwrap_err();
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert!(err.contains("staged app lint failed"), "got: {err}");
    assert_eq!(
        std::fs::read_to_string(installed.join("old-state")).unwrap(),
        "recovered"
    );
    assert!(std::fs::read_to_string(installed.join("app.json"))
        .unwrap()
        .contains(r#""version":"0.0.1""#));
    assert!(
        install_scratch_entries(&dst, "recover").is_empty(),
        "recovery and the failed retry must clean all transaction directories"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
}

#[test]
fn install_same_path_keeps_development_tree_in_place() {
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("cos-install-in-place-{pid}"));
    let source = root.join("devapp");
    let _ = std::fs::remove_dir_all(&root);
    write_min_app(
        &source,
        "devapp",
        r#"{"id":"devapp","version":"0.0.1","name": {"en": "Dev App"}}"#,
    );
    std::fs::write(source.join("working-copy"), b"preserve me").unwrap();
    reseal_app(&source, "devapp");

    let prev_apps = std::env::var_os("COS_APPS_DIR");
    std::env::set_var("COS_APPS_DIR", &root);
    let value = parse(
        install_cmd(&[
            source.display().to_string(),
            "--force".into(),
            "--yes".into(),
        ])
        .unwrap(),
    );
    match prev_apps {
        Some(x) => std::env::set_var("COS_APPS_DIR", x),
        None => std::env::remove_var("COS_APPS_DIR"),
    }

    assert_eq!(value["in_place"], true);
    assert_eq!(value["copied"], false);
    assert_eq!(
        std::fs::read_to_string(source.join("working-copy")).unwrap(),
        "preserve me"
    );
    assert!(install_scratch_entries(&root, "devapp").is_empty());

    let _ = std::fs::remove_dir_all(&root);
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
              "name": {"en": "Linky"}
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
