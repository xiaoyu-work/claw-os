use super::*;

fn write_min_app(dir: &Path, id: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("app.json"), body).unwrap();
    fs::write(dir.join("main.py"), format!("# stub for {id}\n")).unwrap();
    crate::test_env::sign_test_package(dir, crate::provenance::PackageKind::App, id);
}

fn reseal_app(dir: &Path, id: &str) {
    let _ = fs::remove_file(dir.join(crate::provenance::envelope::ENVELOPE_FILE));
    crate::test_env::sign_test_package(dir, crate::provenance::PackageKind::App, id);
}

fn install_scratch_entries(root: &Path, id: &str) -> Vec<String> {
    let prefix = format!(".{id}.install-");
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| name.starts_with(&prefix))
        .collect()
}

#[test]
fn install_cancelled_permission_review_preserves_old_version() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let installed = root.path().join("installed").join("cancelled");
    write_min_app(
        &installed,
        "cancelled",
        r#"{"id":"cancelled","version":"1.0.0","name":{"en":"Existing"}}"#,
    );
    write_min_app(
        &source,
        "cancelled",
        r#"{"id":"cancelled","version":"2.0.0","name":{"en":"Replacement"}}"#,
    );
    let old_manifest = std::fs::read(installed.join("app.json")).unwrap();
    let error = stage_app_install(&source, &installed, true, "cancelled", false, &mut |app| {
        assert!(app.is_verified());
        assert_eq!(
            std::fs::read(installed.join("app.json")).unwrap(),
            old_manifest
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&app.dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        Err("operator declined permission review".into())
    })
    .unwrap_err();
    assert!(error.contains("operator declined"));
    assert_eq!(
        std::fs::read(installed.join("app.json")).unwrap(),
        old_manifest
    );
    assert!(install_scratch_entries(installed.parent().unwrap(), "cancelled").is_empty());
}

#[test]
fn install_rejects_package_substitution_during_permission_review() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let installed = root.path().join("installed").join("substitution");
    write_min_app(
        &installed,
        "substitution",
        r#"{"id":"substitution","version":"1.0.0","name":{"en":"Existing"}}"#,
    );
    write_min_app(
        &source,
        "substitution",
        r#"{"id":"substitution","version":"2.0.0","name":{"en":"Replacement"}}"#,
    );
    let old_manifest = std::fs::read(installed.join("app.json")).unwrap();
    let error = stage_app_install(
        &source,
        &installed,
        true,
        "substitution",
        false,
        &mut |app| {
            std::fs::write(app.dir.join("main.py"), "# substituted after review\n").unwrap();
            reseal_app(&app.dir, "substitution");
            Ok(())
        },
    )
    .unwrap_err();
    assert!(
        error.contains("changed during permission review"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(installed.join("app.json")).unwrap(),
        old_manifest
    );
    assert!(install_scratch_entries(installed.parent().unwrap(), "substitution").is_empty());
}

#[test]
fn install_force_publish_failure_restores_existing_install() {
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("cos-install-atomic-rename-src-{pid}"));
    let dst = std::env::temp_dir().join(format!("cos-install-atomic-rename-dst-{pid}"));
    let installed = dst.join("rollback");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);

    write_min_app(
        &installed,
        "rollback",
        r#"{"id":"rollback","version":"0.0.1","name": {"en": "Rollback"}}"#,
    );
    std::fs::write(installed.join("old-state"), b"restorable").unwrap();
    write_min_app(
        &src,
        "rollback",
        r#"{"id":"rollback","version":"0.0.2","name": {"en": "Rollback"}}"#,
    );

    let mut rename_calls = 0;
    let err = stage_app_install_with_ops(
        &src,
        &installed,
        true,
        "rollback",
        true,
        &mut |_| Ok(()),
        (
            |from: &Path, to: &Path| {
                rename_calls += 1;
                if rename_calls == 2 {
                    Err(std::io::Error::other("injected publish failure"))
                } else {
                    std::fs::rename(from, to)
                }
            },
            |_staging: &Path, _dest: &Path| Ok(false),
        ),
    )
    .unwrap_err();

    assert_eq!(rename_calls, 3, "backup, publish, then rollback");
    assert!(err.contains("injected publish failure"), "got: {err}");
    assert!(err.contains("previous install restored"), "got: {err}");
    assert_eq!(
        std::fs::read_to_string(installed.join("old-state")).unwrap(),
        "restorable"
    );
    assert!(std::fs::read_to_string(installed.join("app.json"))
        .unwrap()
        .contains(r#""version":"0.0.1""#));
    assert!(
        install_scratch_entries(&dst, "rollback").is_empty(),
        "rollback must clean staging and consume the backup"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
}

#[test]
fn install_preview_uses_shared_lint_without_executing_the_entrypoint() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("installed");
    let body = r#"{
        "schema_version":2,"id":"linted","version":"1.0.0","name":{"en":"Linted"},
        "mcp":{"entry":"missing.py","tools":[{"name":"linted.run","summary":{"en":"Run"}}]}
    }"#;
    write_min_app(&source, "linted", body);
    let marker = root.path().join("entrypoint-ran");
    fs::write(
        source.join("main.py"),
        format!(
            "from pathlib import Path\nPath({:?}).write_text('ran')\nimport openai\n",
            marker.to_str().unwrap()
        ),
    )
    .unwrap();
    reseal_app(&source, "linted");
    let app = App {
        manifest: AppManifest::from_json(body).unwrap(),
        dir: source.clone(),
        provenance: Ok(verify_app_tree(&source, "linted", TreeTrust::SignatureOnly).unwrap()),
    };
    let violations = app_lint_violations(&app);
    assert_eq!(violations.len(), 2);
    let install = DirectoryInstall::prepare(&source, &destination).unwrap();
    let error = install.preview().unwrap_err();
    assert_eq!(
        error,
        format!(
            "reviewed app lint failed for `linted`: {}",
            serde_json::to_string(&violations).unwrap()
        )
    );
    assert!(!destination.exists());
    assert!(!marker.exists());
}

#[test]
fn install_in_place_rechecks_the_reviewed_manifest_contract() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("in-place");
    write_min_app(
        &source,
        "in-place",
        r#"{"id":"in-place","version":"1.0.0","name":{"en":"Original"}}"#,
    );
    let install = DirectoryInstall::prepare(&source, root.path()).unwrap();
    assert!(install.is_in_place());
    let error = install
        .publish(false, false, &mut |app| {
            let mut manifest = serde_json::to_value(&app.manifest).unwrap();
            manifest["operations"] = serde_json::json!({
                "read": {
                    "label":{"en":"Read"},
                    "needs":[{
                        "verb":"fs.read",
                        "scope":{"kind":"fixed","scope":{"kind":"path","value":"/private"}},
                        "why":{"en":"New request"}
                    }]
                }
            });
            AppManifest::from_json(&manifest.to_string()).unwrap();
            fs::write(
                source.join("app.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            reseal_app(&source, "in-place");
            Ok(())
        })
        .unwrap_err();
    assert!(
        error.contains("changed during permission review"),
        "{error}"
    );
    assert!(install_scratch_entries(root.path(), "in-place").is_empty());
}

#[test]
fn install_rechecks_destination_without_force_after_review() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("installed");
    write_min_app(
        &source,
        "raced",
        r#"{"id":"raced","version":"1.0.0","name":{"en":"Raced"}}"#,
    );
    let install = DirectoryInstall::prepare(&source, &destination).unwrap();
    let installed = install.destination();
    let error = install
        .publish(false, false, &mut |_| {
            fs::create_dir(installed).unwrap();
            fs::write(installed.join("winner"), "other install").unwrap();
            Ok(())
        })
        .unwrap_err();
    assert_eq!(
        error,
        format!(
            "destination `{}` already exists. Re-run with --force to overwrite.",
            installed.display()
        )
    );
    assert_eq!(
        fs::read_to_string(installed.join("winner")).unwrap(),
        "other install"
    );
    assert!(!installed.join("app.json").exists());
    assert!(install_scratch_entries(&destination, "raced").is_empty());
}

#[test]
fn install_failed_rollback_retains_a_recoverable_backup() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("installed");
    let installed = destination.join("retained");
    write_min_app(
        &installed,
        "retained",
        r#"{"id":"retained","version":"1.0.0","name":{"en":"Original"}}"#,
    );
    write_min_app(
        &source,
        "retained",
        r#"{"id":"retained","version":"2.0.0","name":{"en":"Replacement"}}"#,
    );
    let original = fs::read(installed.join("app.json")).unwrap();
    let mut renames = 0;
    let error = stage_app_install_with_ops(
        &source,
        &installed,
        true,
        "retained",
        false,
        &mut |_| Ok(()),
        (
            |from: &Path, to: &Path| {
                renames += 1;
                if renames == 1 {
                    fs::rename(from, to)
                } else {
                    Err(io::Error::other("injected rename failure"))
                }
            },
            |_staging: &Path, _dest: &Path| Ok(false),
        ),
    )
    .unwrap_err();
    assert_eq!(renames, 3);
    let backups = install_scratch_paths(&destination, "retained", "backup").unwrap();
    assert_eq!(backups.len(), 1);
    assert!(error.contains("rollback"), "{error}");
    assert!(
        error.ends_with(&format!(
            "previous install retained at {}",
            backups[0].display()
        )),
        "{error}"
    );
    assert_eq!(fs::read(backups[0].join("app.json")).unwrap(), original);
    assert!(!installed.exists());
    assert!(install_scratch_paths(&destination, "retained", "staging")
        .unwrap()
        .is_empty());

    let error = DirectoryInstall::prepare(&source, &destination)
        .unwrap()
        .publish(true, false, &mut |_| Err("retry review declined".into()))
        .unwrap_err();
    assert_eq!(error, "retry review declined");
    assert_eq!(fs::read(installed.join("app.json")).unwrap(), original);
    assert!(install_scratch_entries(&destination, "retained").is_empty());
}

#[test]
fn install_exchange_error_does_not_fall_back_to_rename() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("installed");
    let installed = destination.join("exchange");
    write_min_app(
        &installed,
        "exchange",
        r#"{"id":"exchange","version":"1.0.0","name":{"en":"Original"}}"#,
    );
    write_min_app(
        &source,
        "exchange",
        r#"{"id":"exchange","version":"2.0.0","name":{"en":"Replacement"}}"#,
    );
    let original = fs::read(installed.join("app.json")).unwrap();
    let error = stage_app_install_with_ops(
        &source,
        &installed,
        true,
        "exchange",
        false,
        &mut |_| Ok(()),
        (
            |_from: &Path, _to: &Path| panic!("exchange errors must not fall back to rename"),
            |_staging: &Path, _dest: &Path| Err(io::Error::other("injected exchange failure")),
        ),
    )
    .unwrap_err();
    assert!(
        error.starts_with("atomically exchange staged app"),
        "{error}"
    );
    assert!(error.ends_with("injected exchange failure"), "{error}");
    assert_eq!(fs::read(installed.join("app.json")).unwrap(), original);
    assert!(install_scratch_entries(&destination, "exchange").is_empty());
}
