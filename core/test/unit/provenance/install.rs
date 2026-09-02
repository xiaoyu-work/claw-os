use super::*;

use std::fs;
use std::path::{Path, PathBuf};

use crate::provenance::sign::{self, SigningKeyFile};
use crate::provenance::trust::{TrustRootSpec, TrustTier, TRUST_SCHEMA_V1, USAGE_PACKAGE_SIGNING};

fn tmpdir(label: &str) -> PathBuf {
    let p = crate::test_env::secure_scratch_dir(&format!("install-{label}"));
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

fn signed_source(root: &Path, id: &str, body: &str) -> (PathBuf, SigningKeyFile) {
    let dir = root.join(format!("src-{id}"));
    fs::create_dir_all(&dir).unwrap();
    write_file(
        &dir.join("app.json"),
        &format!(r#"{{"id":"{id}","version":"1.0.0","name":"{id}","operations":{{}}}}"#),
    );
    write_file(&dir.join("main.py"), body);
    let key = SigningKeyFile::generate(None).unwrap();
    sign::sign_directory(
        &dir,
        &sign::SignRequest {
            kind: PackageKind::App,
            id: id.to_string(),
            version: "1.0.0".to_string(),
            manifest_schema: "cos.app-manifest/v1".to_string(),
            manifest_path: "app.json".to_string(),
            entrypoints: vec!["main.py".to_string()],
            resources: vec![],
        },
        &key,
    )
    .unwrap();
    (dir, key)
}

#[cfg(unix)]
fn trust_for(key: &SigningKeyFile, root: &Path) -> TrustStore {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(root).unwrap();
    let file = serde_json::json!({
        "schema": TRUST_SCHEMA_V1,
        "keys": [{
            "key_id": key.key_id,
            "algorithm": "ed25519",
            "public_key": key.public_key,
            "usages": [USAGE_PACKAGE_SIGNING],
            "kinds": ["app", "skill", "mcp"],
            "status": "active",
        }],
    });
    let path = root.join("keys.json");
    fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
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
fn unsafe_nodes_are_rejected_before_verification() {
    let root = tmpdir("unsafe");
    let limits = Limits::default();

    let symlinked = root.join("symlink");
    fs::create_dir_all(&symlinked).unwrap();
    write_file(&symlinked.join("a"), "a");
    std::os::unix::fs::symlink("/etc/passwd", symlinked.join("leak")).unwrap();
    let err = assert_safe_tree(&symlinked, &limits).unwrap_err();
    assert!(format!("{err}").contains("symlink"), "{err}");

    let hard = root.join("hardlink");
    fs::create_dir_all(&hard).unwrap();
    write_file(&hard.join("a"), "a");
    fs::hard_link(hard.join("a"), hard.join("b")).unwrap();
    let err = assert_safe_tree(&hard, &limits).unwrap_err();
    assert!(format!("{err}").contains("hard link"), "{err}");

    let case = root.join("case");
    fs::create_dir_all(&case).unwrap();
    write_file(&case.join("Main.py"), "a");
    write_file(&case.join("main.py"), "b");
    let err = assert_safe_tree(&case, &limits).unwrap_err();
    assert!(format!("{err}").contains("case-collides"), "{err}");

    let writable = root.join("writable");
    fs::create_dir_all(&writable).unwrap();
    write_file(&writable.join("a"), "a");
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(writable.join("a"), fs::Permissions::from_mode(0o666)).unwrap();
    }
    let err = assert_safe_tree(&writable, &limits).unwrap_err();
    assert!(format!("{err}").contains("world-writable"), "{err}");

    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn fifo_nodes_are_rejected() {
    let root = tmpdir("fifo");
    let dir = root.join("pkg");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pipe");
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
    let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
    if rc == 0 {
        let err = assert_safe_tree(&dir, &Limits::default()).unwrap_err();
        assert!(format!("{err}").contains("FIFO"), "{err}");
    }
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn bundle_limits_are_enforced() {
    let root = tmpdir("limits");
    let dir = root.join("pkg");
    fs::create_dir_all(&dir).unwrap();
    for i in 0..5 {
        write_file(&dir.join(format!("f{i}")), "0123456789");
    }
    let mut limits = Limits::default();
    limits.max_files = 3;
    let err = assert_safe_tree(&dir, &limits).unwrap_err();
    assert!(format!("{err}").contains("file count"), "{err}");

    let mut limits = Limits::default();
    limits.max_total_bytes = 12;
    let err = assert_safe_tree(&dir, &limits).unwrap_err();
    assert!(format!("{err}").contains("total bytes"), "{err}");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn staging_is_private_and_self_cleaning() {
    use std::os::unix::fs::PermissionsExt;
    let root = tmpdir("staging");
    let path = {
        let staging = Staging::create(&root, "pkg").unwrap();
        let mode = fs::metadata(staging.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        staging.path().to_path_buf()
    };
    assert!(!path.exists(), "unpublished staging must be removed on drop");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn publish_is_atomic_and_retains_a_verifiable_artifact() {
    let root = tmpdir("publish");
    let data = root.join("data");
    let _guard = crate::test_env::lock_env();
    let _env = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", &data);

    let (source, key) = signed_source(&root, "notes", "print('v1')\n");
    let trust = trust_for(&key, &root.join("trust"));
    let dest = root.join("live").join("notes");

    let staged = stage_directory(
        &source,
        &dest,
        PackageKind::App,
        Some("notes"),
        &trust,
        &Limits::default(),
    )
    .unwrap();
    let digest = staged.verified.content_digest().to_string();
    let published = publish(staged, &dest, false, &Limits::default()).unwrap();
    assert!(!published.replaced);
    assert_eq!(published.content_digest, digest);
    assert!(dest.join("main.py").is_file());
    // No staging leftovers next to the live directory.
    let leftovers: Vec<_> = fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");

    // The retained artifact re-verifies on its own.
    let artifact = artifact_dir(PackageKind::App, "notes", &digest);
    assert!(artifact.is_dir());
    crate::provenance::verify::verify_package(
        &artifact,
        &crate::provenance::VerifyOptions::new(PackageKind::App)
            .expect_id("notes")
            .signature_only(),
        &trust,
    )
    .unwrap();

    // A second install without --force refuses rather than merging.
    let staged = stage_directory(
        &source,
        &dest,
        PackageKind::App,
        Some("notes"),
        &trust,
        &Limits::default(),
    )
    .unwrap();
    let err = publish(staged, &dest, false, &Limits::default()).unwrap_err();
    assert!(matches!(err, InstallError::DestinationExists(_)), "{err}");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn rollback_only_lands_on_a_verified_artifact() {
    let root = tmpdir("rollback");
    let data = root.join("data");
    let _guard = crate::test_env::lock_env();
    let _env = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", &data);

    let (source_v1, key) = signed_source(&root, "notes", "print('v1')\n");
    let trust = trust_for(&key, &root.join("trust"));
    let dest = root.join("live").join("notes");
    let staged = stage_directory(
        &source_v1,
        &dest,
        PackageKind::App,
        Some("notes"),
        &trust,
        &Limits::default(),
    )
    .unwrap();
    let v1 = staged.verified.content_digest().to_string();
    publish(staged, &dest, false, &Limits::default()).unwrap();

    // Publish a second version side by side.
    write_file(&source_v1.join("main.py"), "print('v2')\n");
    sign::sign_directory(
        &source_v1,
        &sign::SignRequest {
            kind: PackageKind::App,
            id: "notes".to_string(),
            version: "2.0.0".to_string(),
            manifest_schema: "cos.app-manifest/v1".to_string(),
            manifest_path: "app.json".to_string(),
            entrypoints: vec!["main.py".to_string()],
            resources: vec![],
        },
        &key,
    )
    .unwrap();
    let staged = stage_directory(
        &source_v1,
        &dest,
        PackageKind::App,
        Some("notes"),
        &trust,
        &Limits::default(),
    )
    .unwrap();
    let v2 = staged.verified.content_digest().to_string();
    publish(staged, &dest, true, &Limits::default()).unwrap();
    assert_ne!(v1, v2);
    assert_eq!(fs::read_to_string(dest.join("main.py")).unwrap(), "print('v2')\n");
    assert_eq!(list_artifacts(PackageKind::App, "notes").len(), 2);

    // Roll back to the earlier verified artifact.
    let back = rollback(
        PackageKind::App,
        "notes",
        &v1,
        &dest,
        &trust,
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(back.content_digest, v1);
    assert_eq!(fs::read_to_string(dest.join("main.py")).unwrap(), "print('v1')\n");

    // An unknown digest is refused rather than guessed at.
    let err = rollback(
        PackageKind::App,
        "notes",
        &format!("sha256:{}", "0".repeat(64)),
        &dest,
        &trust,
        &Limits::default(),
    )
    .unwrap_err();
    assert!(format!("{err}").contains("no retained artifact"), "{err}");

    // A tampered artifact cannot be rolled back onto.
    let artifact = artifact_dir(PackageKind::App, "notes", &v2);
    write_file(&artifact.join("main.py"), "print('evil')\n");
    let err = rollback(
        PackageKind::App,
        "notes",
        &v2,
        &dest,
        &trust,
        &Limits::default(),
    )
    .unwrap_err();
    assert_eq!(err.code(), "provenance.content_mismatch");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn unsigned_source_never_reaches_the_live_directory() {
    let root = tmpdir("unsigned");
    let data = root.join("data");
    let _guard = crate::test_env::lock_env();
    let _env = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", &data);

    let source = root.join("src");
    fs::create_dir_all(&source).unwrap();
    write_file(
        &source.join("app.json"),
        r#"{"id":"notes","version":"1","name":"notes","operations":{}}"#,
    );
    let dest = root.join("live").join("notes");
    let trust = TrustStore::default();
    let err = stage_directory(
        &source,
        &dest,
        PackageKind::App,
        Some("notes"),
        &trust,
        &Limits::default(),
    )
    .unwrap_err();
    assert_eq!(err.code(), "provenance.unsigned");
    assert!(!dest.exists());
    let _ = fs::remove_dir_all(&root);
}
