use super::*;
use serde_json::json;

fn catalog_value() -> Value {
    json!({
        "schema": "claw.app-store/v1", "repository": REPOSITORY, "runtime_abi": 1,
        "generation": "1", "issued_at": Utc::now() - chrono::Duration::minutes(1),
        "expires_at": Utc::now() + chrono::Duration::days(1),
        "apps": [{
            "app_id": "fixture", "app_version": "1.0.0", "architecture": "all",
            "release_version": "1.0.0", "release_tag": "app-fixture-v1.0.0",
            "artifact": {"file": format!("claw-app-snapshot-{}.tar.gz", "a".repeat(64)),
                "size": 10, "sha256": "a".repeat(64), "media_type": "application/gzip"},
            "manifest": {"path": "app.json", "size": 1, "sha256": "b".repeat(64)},
            "provenance": {"path": ".provenance.json", "sha256": "c".repeat(64),
                "content_digest": format!("sha256:{}", "d".repeat(64)),
                "publisher_key_id": format!("sha256:{}", "e".repeat(64))},
            "display": {"name": "Fixture", "description": "", "source_kind": "product", "source_name": "fixture"},
            "web_ui": {"path": "web-ui.json", "sha256": "f".repeat(64), "entry": "web/index.html"},
        }],
    })
}

#[test]
fn catalog_refuses_stale_or_substituted_release_metadata() {
    let value = catalog_value();
    let bytes = serde_json::to_vec(&value).unwrap();
    let catalog = Catalog::decode(&bytes, Utc::now()).unwrap();
    assert!(catalog.select("fixture", &"a".repeat(64)).is_ok());
    assert!(catalog.select("other-app", &"a".repeat(64)).is_err());
    assert!(catalog.select("fixture", &"0".repeat(64)).is_err());
    for changed in [
        {
            let mut v = value.clone();
            v["repository"] = json!("different/publisher");
            v
        },
        {
            let mut v = value.clone();
            v["expires_at"] = json!(Utc::now() - chrono::Duration::minutes(1));
            v
        },
        {
            let mut v = value.clone();
            v["apps"][0]["release_tag"] = json!("app-other-v1.0.0");
            v
        },
        {
            let mut v = value.clone();
            v["apps"][0]["artifact"]["file"] = json!("../unbound.tar.gz");
            v
        },
        {
            let mut v = value.clone();
            v["apps"][0]["permissions_granted"] = json!(true);
            v
        },
    ] {
        assert!(Catalog::decode(&serde_json::to_vec(&changed).unwrap(), Utc::now()).is_err());
    }
}

#[test]
fn signed_web_resources_remain_bound_to_the_owning_app() {
    let _guard = crate::test_env::lock_env();
    let package = crate::test_env::signed_web_app("web-binding");
    let descriptor = web_descriptor(&package).unwrap();
    assert_eq!(descriptor.app_id, "web-binding");
    let metadata = web_metadata(&package).unwrap();
    assert_eq!(metadata["id"], "web-binding");
    assert!(metadata["tools"].as_array().unwrap().is_empty());
    std::fs::write(package.dir().join("web/app.js"), "substituted UI code").unwrap();
    assert!(package.read_verified("web/app.js").is_err());
    std::fs::write(
        package.dir().join("web-ui.json"),
        r#"{"app_id":"different"}"#,
    )
    .unwrap();
    assert!(web_descriptor(&package).is_err());
}

#[test]
fn store_record_binds_the_exact_manifest_envelope_and_product_descriptor() {
    let _guard = crate::test_env::lock_env();
    let package = crate::test_env::signed_web_app("record-binding");
    let mut value = catalog_value()["apps"][0].clone();
    value["app_id"] = json!(package.id());
    value["app_version"] = json!(package.version());
    value["manifest"] = json!({
        "path": "app.json", "size": package.read_verified("app.json").unwrap().len(),
        "sha256": sha256(&package.read_verified("app.json").unwrap()),
    });
    value["provenance"] = json!({
        "path": ".provenance.json", "sha256": sha256(&std::fs::read(package.dir().join(".provenance.json")).unwrap()),
        "content_digest": package.content_digest(),
        "publisher_key_id": package.audit_facts()["publisher_key_id"],
    });
    value["display"] = json!({"name": "Web fixture", "description": "Signed fixture",
        "source_kind": "product", "source_name": "fixture"});
    value["web_ui"]["sha256"] = json!(sha256(&package.read_verified(WEB_DESCRIPTOR).unwrap()));
    let record: Record = serde_json::from_value(value).unwrap();
    record.verify_package(&package).unwrap();
    let mut changed = record.clone();
    changed.provenance.sha256 = "0".repeat(64);
    assert!(changed.verify_package(&package).is_err());
    let mut changed = record.clone();
    changed.display.source_name = "other-product".into();
    assert!(changed.verify_package(&package).is_err());
    let mut changed = record;
    changed.web_ui.sha256 = "0".repeat(64);
    assert!(changed.verify_package(&package).is_err());
}

#[tokio::test]
async fn catalog_signatures_require_the_pinned_primary_publisher_and_exact_bytes() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("gnupg");
    std::fs::create_dir(&home).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let base = || {
        let mut command = std::process::Command::new("/usr/bin/gpg");
        command
            .args([
                "--no-options",
                "--batch",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
            ])
            .arg("--homedir")
            .arg(&home);
        command
    };
    assert!(base()
        .args([
            "--quick-generate-key",
            "Store signature fixture",
            "ed25519",
            "sign",
            "1d"
        ])
        .output()
        .unwrap()
        .status
        .success());
    let keys = base()
        .args(["--with-colons", "--list-keys"])
        .output()
        .unwrap();
    let fingerprint = String::from_utf8(keys.stdout)
        .unwrap()
        .lines()
        .find(|line| line.starts_with("fpr:"))
        .unwrap()
        .split(':')
        .nth(9)
        .unwrap()
        .to_string();
    let public = base()
        .args(["--armor", "--export", &fingerprint])
        .output()
        .unwrap();
    let content = root.path().join("index.json");
    let detached = root.path().join("index.json.asc");
    std::fs::write(&content, b"{\"catalog\":\"exact\"}").unwrap();
    assert!(base()
        .arg("--armor")
        .arg("--output")
        .arg(&detached)
        .arg("--detach-sign")
        .arg(&content)
        .output()
        .unwrap()
        .status
        .success());
    let signature = std::fs::read(detached).unwrap();
    let public = String::from_utf8(public.stdout).unwrap();
    verify_signature(
        b"{\"catalog\":\"exact\"}",
        &signature,
        &public,
        &fingerprint,
    )
    .await
    .unwrap();
    assert!(verify_signature(
        b"{\"catalog\":\"changed\"}",
        &signature,
        &public,
        &fingerprint
    )
    .await
    .is_err());
    assert!(verify_signature(
        b"{\"catalog\":\"exact\"}",
        &signature,
        &public,
        ARCHIVE_FINGERPRINT
    )
    .await
    .is_err());
    let _ = std::process::Command::new("/usr/bin/gpgconf")
        .arg("--homedir")
        .arg(&home)
        .args(["--kill", "gpg-agent"])
        .output();
}

fn archive(root: &Path, kind: tar::EntryType, mode: u32) -> std::path::PathBuf {
    let source = root.join("source.tar.gz");
    let gzip = flate2::write::GzEncoder::new(
        std::fs::File::create(&source).unwrap(),
        flate2::Compression::default(),
    );
    let mut archive = tar::Builder::new(gzip);
    let mut header = tar::Header::new_ustar();
    header.set_path("payload").unwrap();
    header.set_entry_type(kind);
    header.set_size(1);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    archive.append(&header, std::io::Cursor::new(b"x")).unwrap();
    archive.into_inner().unwrap().finish().unwrap();
    source
}

#[test]
fn snapshot_extraction_rejects_links_writable_files_and_trailing_payloads() {
    for (kind, mode) in [
        (tar::EntryType::Symlink, 0o644),
        (tar::EntryType::Regular, 0o666),
    ] {
        let root = tempfile::tempdir().unwrap();
        let source = archive(root.path(), kind, mode);
        assert!(extract_snapshot(&source, &root.path().join("app")).is_err());
    }
    let root = tempfile::tempdir().unwrap();
    let source = archive(root.path(), tar::EntryType::Regular, 0o644);
    extract_snapshot(&source, &root.path().join("valid")).unwrap();
    assert_eq!(
        std::fs::read(root.path().join("valid/payload")).unwrap(),
        b"x"
    );
    let file = std::fs::OpenOptions::new()
        .append(true)
        .open(&source)
        .unwrap();
    let mut extra = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    extra.write_all(b"unindexed second gzip member").unwrap();
    extra.finish().unwrap();
    assert!(extract_snapshot(&source, &root.path().join("invalid")).is_err());
}
