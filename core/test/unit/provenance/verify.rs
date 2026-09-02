use super::*;

use std::fs;
use std::path::{Path, PathBuf};

use crate::provenance::sign::{self, SigningKeyFile};
use crate::provenance::trust::{TrustRootSpec, TRUST_SCHEMA_V1, USAGE_PACKAGE_SIGNING};

fn tmpdir(label: &str) -> PathBuf {
    let p = crate::test_env::secure_scratch_dir(&format!("verify-{label}"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

fn write_file(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }
}

/// Build a signed App package with `app.json` + `main.py`.
fn signed_app(root: &Path, id: &str, main_body: &str) -> (PathBuf, SigningKeyFile) {
    signed_app_with_resources(root, id, main_body, &[])
}

fn signed_app_with_resources(
    root: &Path,
    id: &str,
    main_body: &str,
    resources: &[(&str, &str)],
) -> (PathBuf, SigningKeyFile) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    write_file(
        &dir.join("app.json"),
        &format!(r#"{{"id":"{id}","version":"1.0.0","name":"{id}","operations":{{}}}}"#),
    );
    write_file(&dir.join("main.py"), main_body);
    for (path, body) in resources {
        write_file(&dir.join(path), body);
    }
    let key = SigningKeyFile::generate(None).unwrap();
    let request = sign::SignRequest {
        kind: PackageKind::App,
        id: id.to_string(),
        version: "1.0.0".to_string(),
        manifest_schema: "cos.app-manifest/v1".to_string(),
        manifest_path: "app.json".to_string(),
        entrypoints: vec!["main.py".to_string()],
        resources: resources
            .iter()
            .map(|(path, _)| (*path).to_string())
            .collect(),
    };
    sign::sign_directory(&dir, &request, &key).unwrap();
    (dir, key)
}

#[cfg(unix)]
fn trust_for(keys: &[&SigningKeyFile], root: &Path) -> TrustStore {
    fs::create_dir_all(root).unwrap();
    let entries: Vec<serde_json::Value> = keys
        .iter()
        .map(|k| {
            serde_json::json!({
                "key_id": k.key_id,
                "algorithm": "ed25519",
                "public_key": k.public_key,
                "usages": [USAGE_PACKAGE_SIGNING],
                "kinds": ["app", "skill", "mcp"],
                "status": "active",
            })
        })
        .collect();
    let file = serde_json::json!({ "schema": TRUST_SCHEMA_V1, "keys": entries });
    let path = root.join("keys.json");
    fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let roots = vec![TrustRootSpec {
        path: root.to_path_buf(),
        tier: TrustTier::User,
        allowed_uids: vec![crate::provenance::fsec::effective_uid()],
        domain: crate::provenance::state::TrustDomain::Owner(
            crate::provenance::fsec::effective_uid(),
        ),
    }];
    crate::test_env::record_trust_state(&roots);
    TrustStore::load_roots(&roots)
}

#[cfg(unix)]
#[test]
fn signed_package_verifies_and_binds_its_manifest() {
    let root = tmpdir("ok");
    let (dir, key) = signed_app(&root, "notes", "print('hi')\n");
    let trust = trust_for(&[&key], &root.join("trust"));
    let pkg = verify_package(&dir, &VerifyOptions::new(PackageKind::App), &trust).unwrap();
    assert_eq!(pkg.id(), "notes");
    assert_eq!(pkg.version(), "1.0.0");
    assert!(matches!(pkg.source(), TrustSource::Publisher { .. }));
    assert!(pkg.manifest_text().unwrap().contains("\"id\":\"notes\""));
    assert_eq!(pkg.read_verified_text("main.py").unwrap(), "print('hi')\n");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn verified_snapshot_is_materialized_from_revalidated_descriptors() {
    use std::os::unix::fs::PermissionsExt;

    let root = tmpdir("materialized");
    let (dir, key) = signed_app_with_resources(
        &root,
        "notes",
        "print('signed')\n",
        &[("assets/nested/config.json", "{\"enabled\":true}\n")],
    );
    let trust = trust_for(&[&key], &root.join("trust"));
    let package = verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::App).expect_id("notes"),
        &trust,
    )
    .unwrap();
    let snapshot = root.join("snapshot");
    package.materialize_snapshot(&snapshot).unwrap();
    assert_eq!(
        fs::read_to_string(snapshot.join("main.py")).unwrap(),
        "print('signed')\n"
    );
    assert_eq!(
        fs::metadata(&snapshot).unwrap().permissions().mode() & 0o222,
        0
    );
    assert_eq!(
        fs::metadata(snapshot.join("main.py"))
            .unwrap()
            .permissions()
            .mode()
            & 0o222,
        0
    );
    assert_eq!(
        fs::read_to_string(snapshot.join("assets/nested/config.json")).unwrap(),
        "{\"enabled\":true}\n"
    );
    assert_eq!(
        fs::metadata(snapshot.join("assets/nested"))
            .unwrap()
            .permissions()
            .mode()
            & 0o222,
        0
    );

    write_file(&dir.join("main.py"), "print('replaced')\n");
    let error = package
        .materialize_snapshot(&root.join("replaced"))
        .unwrap_err();
    assert!(error.to_string().contains("changed"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn untrusted_and_forged_keys_are_rejected() {
    let root = tmpdir("forged");
    let (dir, key) = signed_app(&root, "notes", "x=1\n");
    let other = SigningKeyFile::generate(None).unwrap();

    // Key not in the trust store.
    let empty = trust_for(&[&other], &root.join("trust"));
    let err = verify_package(&dir, &VerifyOptions::new(PackageKind::App), &empty).unwrap_err();
    assert_eq!(err.code(), "provenance.untrusted_key");

    // Signature bytes swapped for another key's signature while the
    // envelope still claims the trusted key.
    let envelope_path = dir.join(crate::provenance::envelope::ENVELOPE_FILE);
    let mut env: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&envelope_path).unwrap()).unwrap();
    env["signature"]["value"] = serde_json::json!("0".repeat(128));
    fs::write(&envelope_path, env.to_string()).unwrap();
    let trust = trust_for(&[&key], &root.join("trust2"));
    let err = verify_package(&dir, &VerifyOptions::new(PackageKind::App), &trust).unwrap_err();
    assert_eq!(err.code(), "provenance.signature_rejected");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn modified_added_and_removed_files_are_detected() {
    let root = tmpdir("tamper");
    let (dir, key) = signed_app(&root, "notes", "print('a')\n");
    let trust = trust_for(&[&key], &root.join("trust"));
    let options = VerifyOptions::new(PackageKind::App);
    verify_package(&dir, &options, &trust).unwrap();

    // Modified entry.
    write_file(&dir.join("main.py"), "print('evil')\n");
    let err = verify_package(&dir, &options, &trust).unwrap_err();
    assert_eq!(err.code(), "provenance.content_mismatch");
    write_file(&dir.join("main.py"), "print('a')\n");
    verify_package(&dir, &options, &trust).unwrap();

    // Added file the signature does not cover.
    write_file(&dir.join("extra.py"), "print('extra')\n");
    let err = verify_package(&dir, &options, &trust).unwrap_err();
    assert!(
        format!("{err}").contains("not covered by the signature"),
        "{err}"
    );
    fs::remove_file(dir.join("extra.py")).unwrap();

    // Removed file.
    fs::remove_file(dir.join("main.py")).unwrap();
    let err = verify_package(&dir, &options, &trust).unwrap_err();
    assert!(
        format!("{err}").contains("missing from the package"),
        "{err}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn modified_manifest_cannot_influence_capability_derivation() {
    let root = tmpdir("manifest");
    let (dir, key) = signed_app(&root, "notes", "x=1\n");
    let trust = trust_for(&[&key], &root.join("trust"));
    // Rewrite the manifest to claim a wildcard capability need.
    write_file(
        &dir.join("app.json"),
        r#"{"id":"notes","version":"9.9.9","name":"notes","operations":{}}"#,
    );
    let err = verify_package(&dir, &VerifyOptions::new(PackageKind::App), &trust).unwrap_err();
    assert_eq!(err.code(), "provenance.content_mismatch");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn kind_and_id_confusion_are_rejected() {
    let root = tmpdir("kind");
    let (dir, key) = signed_app(&root, "notes", "x=1\n");
    let trust = trust_for(&[&key], &root.join("trust"));
    let err = verify_package(&dir, &VerifyOptions::new(PackageKind::Skill), &trust).unwrap_err();
    assert_eq!(err.code(), "provenance.identity_mismatch");
    let err = verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::App).expect_id("other"),
        &trust,
    )
    .unwrap_err();
    assert_eq!(err.code(), "provenance.identity_mismatch");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn symlinks_and_special_files_are_refused_inside_a_package() {
    let root = tmpdir("symlink");
    let (dir, key) = signed_app(&root, "notes", "x=1\n");
    let trust = trust_for(&[&key], &root.join("trust"));
    std::os::unix::fs::symlink("/etc/passwd", dir.join("leak")).unwrap();
    let err = verify_package(&dir, &VerifyOptions::new(PackageKind::App), &trust).unwrap_err();
    assert!(format!("{err}").contains("symlink"), "{err}");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn hardlinked_files_are_refused() {
    let root = tmpdir("hardlink");
    let (dir, key) = signed_app(&root, "notes", "x=1\n");
    let trust = trust_for(&[&key], &root.join("trust"));
    fs::hard_link(dir.join("main.py"), dir.join("alias.py")).unwrap();
    let err = verify_package(&dir, &VerifyOptions::new(PackageKind::App), &trust).unwrap_err();
    assert!(format!("{err}").contains("hard"), "{err}");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn unsigned_package_fails_closed_without_a_developer_grant() {
    let root = tmpdir("unsigned");
    let dir = root.join("scratch");
    fs::create_dir_all(&dir).unwrap();
    write_file(
        &dir.join("app.json"),
        r#"{"id":"scratch","version":"1","name":"scratch","operations":{}}"#,
    );
    let trust = TrustStore::default();
    let err = verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::App).expect_id("scratch"),
        &trust,
    )
    .unwrap_err();
    assert_eq!(err.code(), "provenance.unsigned");
    assert!(format!("{err}").contains("dev-trust"), "{err}");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn source_tree_is_never_promoted_to_vendor_trust() {
    // The repository's own apps/ tree is not under an approved package
    // root, so it can never inherit Debian/rootfs trust.
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("apps");
    assert!(!crate::provenance::verify::is_vendor_root_path(&repo));
    assert!(!crate::provenance::verify::is_vendor_root_path(
        &std::env::temp_dir()
    ));
}

#[cfg(unix)]
#[test]
fn read_verified_detects_replacement_after_verification() {
    let root = tmpdir("toctou");
    let (dir, key) = signed_app(&root, "notes", "print('good')\n");
    let trust = trust_for(&[&key], &root.join("trust"));
    let pkg = verify_package(&dir, &VerifyOptions::new(PackageKind::App), &trust).unwrap();
    assert_eq!(
        pkg.read_verified_text("main.py").unwrap(),
        "print('good')\n"
    );

    // Replace the file by rename — the classic verify-then-execute race.
    write_file(&dir.join("main.py.new"), "print('evil')\n");
    fs::rename(dir.join("main.py.new"), dir.join("main.py")).unwrap();
    let err = pkg.read_verified("main.py").unwrap_err();
    assert_eq!(err.code(), "provenance.content_mismatch");
    assert!(pkg.open_entrypoint("main.py").is_err());
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn assert_current_detects_a_replaced_package_directory() {
    let root = tmpdir("swapdir");
    let (dir, key) = signed_app(&root, "notes", "x=1\n");
    let trust = trust_for(&[&key], &root.join("trust"));
    let pkg = verify_package(&dir, &VerifyOptions::new(PackageKind::App), &trust).unwrap();
    pkg.assert_current(&trust).unwrap();

    let replacement = root.join("replacement");
    fs::create_dir_all(&replacement).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    fs::rename(&replacement, &dir).unwrap();
    let err = pkg.assert_current(&trust).unwrap_err();
    assert!(format!("{err}").contains("replaced"), "{err}");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn revocation_invalidates_caches_and_stops_future_use() {
    let root = tmpdir("revcache");
    let (dir, key) = signed_app(&root, "notes", "x=1\n");
    let trust_root = root.join("trust");
    let trust = trust_for(&[&key], &trust_root);
    let options = VerifyOptions::new(PackageKind::App);
    let first = verify_package_cached(&dir, &options, &trust).unwrap();
    let digest = first.content_digest().to_string();

    // Revoke the package digest and reload.
    let file = serde_json::json!({
        "schema": TRUST_SCHEMA_V1,
        "keys": [{
            "key_id": key.key_id,
            "algorithm": "ed25519",
            "public_key": key.public_key,
            "usages": [USAGE_PACKAGE_SIGNING],
            "kinds": ["app"],
            "status": "active",
        }],
        "revoked_packages": [digest],
    });
    fs::write(
        trust_root.join("keys.json"),
        serde_json::to_vec_pretty(&file).unwrap(),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            trust_root.join("keys.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let revoked = TrustStore::load_roots(&[TrustRootSpec {
        path: trust_root.clone(),
        tier: TrustTier::User,
        allowed_uids: vec![crate::provenance::fsec::effective_uid()],
        domain: crate::provenance::state::TrustDomain::Owner(
            crate::provenance::fsec::effective_uid(),
        ),
    }]);
    assert_ne!(revoked.generation(), trust.generation());
    // The cached snapshot must not be reusable under the new store.
    assert!(first.assert_current(&revoked).is_err());
    let err = verify_package_cached(&dir, &options, &revoked).unwrap_err();
    assert_eq!(err.code(), "provenance.untrusted_key");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn developer_grant_authorises_one_tree_at_one_digest() {
    let root = tmpdir("devgrant");
    let dir = root.join("scratch");
    fs::create_dir_all(&dir).unwrap();
    write_file(
        &dir.join("SKILL.md"),
        "---\nname: scratch\n---\nlocal notes\n",
    );

    let body = sign::build_body(
        &dir,
        &sign::SignRequest {
            kind: PackageKind::Skill,
            id: "scratch".to_string(),
            version: "dev".to_string(),
            manifest_schema: "developer".to_string(),
            manifest_path: "SKILL.md".to_string(),
            entrypoints: vec![],
            resources: vec![],
        },
    )
    .unwrap();
    let digest = crate::provenance::envelope::content_digest(&body.files);

    let dev_root = root.join("devtrust");
    fs::create_dir_all(&dev_root).unwrap();
    let grants = serde_json::json!({
        "schema": crate::provenance::trust::DEV_TRUST_SCHEMA_V1,
        "grants": [{
            "kind": "skill",
            "id": "scratch",
            "path": dir.canonicalize().unwrap(),
            "content_digest": digest,
            "granted_at": "2026-01-01T00:00:00Z",
        }],
    });
    let grants_path = dev_root.join("grants.json");
    fs::write(&grants_path, serde_json::to_vec_pretty(&grants).unwrap()).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&grants_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let dev_roots = vec![TrustRootSpec {
        path: dev_root.clone(),
        tier: TrustTier::Developer,
        allowed_uids: vec![crate::provenance::fsec::effective_uid()],
        domain: crate::provenance::state::TrustDomain::Owner(
            crate::provenance::fsec::effective_uid(),
        ),
    }];
    crate::test_env::record_trust_state(&dev_roots);
    let trust = TrustStore::load_roots(&dev_roots);

    let options = VerifyOptions::new(PackageKind::Skill).expect_id("scratch");
    let pkg = verify_package(&dir, &options, &trust).unwrap();
    assert_eq!(pkg.source(), &TrustSource::Developer);
    assert_eq!(pkg.tier(), TrustTier::Developer);
    assert!(!pkg.tier().allows_privileged_routes());

    // Editing the tree invalidates the grant.
    write_file(&dir.join("SKILL.md"), "---\nname: scratch\n---\nedited\n");
    let err = verify_package(&dir, &options, &trust).unwrap_err();
    assert_eq!(err.code(), "provenance.developer_grant_stale");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn model_visible_input_cannot_add_a_trust_root() {
    // The only trust inputs are the compiled-in roots and an explicit
    // `load_roots` call. Nothing reads a manifest, tool argument or env
    // var to widen trust.
    let roots = TrustStore::default_roots();
    for root in &roots {
        let display = root.path.display().to_string();
        assert!(
            display.starts_with("/usr/lib/cos")
                || display.starts_with("/etc/cos")
                || display.contains(".config/cos/trust"),
            "unexpected trust root {display}"
        );
    }
}
