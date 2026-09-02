use super::*;

use std::fs;
use std::path::PathBuf;

use crate::provenance::envelope::{Envelope, ENVELOPE_FILE};

fn tmpdir(label: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "cos-prov-sign-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&p).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

fn request(id: &str) -> SignRequest {
    SignRequest {
        kind: PackageKind::App,
        id: id.to_string(),
        version: "0.1.0".to_string(),
        manifest_schema: "cos.app-manifest/v1".to_string(),
        manifest_path: "app.json".to_string(),
        entrypoints: vec!["main.py".to_string()],
        resources: vec![],
    }
}

#[cfg(unix)]
#[test]
fn generated_keys_are_distinct_and_self_consistent() {
    let a = SigningKeyFile::generate(None).unwrap();
    let b = SigningKeyFile::generate(None).unwrap();
    assert_ne!(a.private_key, b.private_key);
    assert_ne!(a.key_id, b.key_id);
    let signing = a.signing_key().unwrap();
    assert_eq!(
        hex::encode(signing.verifying_key().to_bytes()),
        a.public_key
    );
    assert_eq!(
        super::key_id_for(&signing.verifying_key().to_bytes()),
        a.key_id
    );
}

#[cfg(unix)]
#[test]
fn key_files_must_not_be_readable_by_others() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("keyperm");
    let path = dir.join("key.json");
    let key = SigningKeyFile::generate(None).unwrap();
    key.write_new(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    SigningKeyFile::load(&path).unwrap();

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let err = SigningKeyFile::load(&path).unwrap_err();
    assert!(matches!(err, SignError::KeyFilePermissions { .. }), "{err}");

    // Never clobbers an existing key.
    assert!(key.write_new(&path).is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn sign_then_verify_round_trips() {
    let dir = tmpdir("roundtrip");
    let pkg = dir.join("notes");
    fs::create_dir_all(pkg.join("lib")).unwrap();
    fs::write(pkg.join("app.json"), "{}").unwrap();
    fs::write(pkg.join("main.py"), "print(1)\n").unwrap();
    fs::write(pkg.join("lib/util.py"), "def f(): pass\n").unwrap();
    let key = SigningKeyFile::generate(None).unwrap();
    let envelope = sign_directory(&pkg, &request("notes"), &key).unwrap();

    // Round-trips through the on-disk JSON unchanged.
    let raw = fs::read_to_string(pkg.join(ENVELOPE_FILE)).unwrap();
    let parsed = Envelope::parse(&raw).unwrap();
    assert_eq!(parsed, envelope);
    assert_eq!(parsed.signature.key_id, key.key_id);

    // The signature really covers the canonical bytes.
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let vk = VerifyingKey::from_bytes(&parsed.public_key_bytes().unwrap()).unwrap();
    vk.verify(
        &parsed.signing_bytes(),
        &Signature::from_bytes(&parsed.signature_bytes().unwrap()),
    )
    .unwrap();

    // Directory entries are included so an unexpected directory is
    // detected, and the envelope never describes itself.
    assert!(parsed.package.files.iter().any(|f| f.path == "lib"));
    assert!(parsed
        .package
        .files
        .iter()
        .all(|f| f.path != ENVELOPE_FILE));
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn signing_refuses_symlinks_and_special_files() {
    let dir = tmpdir("refuse");
    let pkg = dir.join("notes");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("app.json"), "{}").unwrap();
    fs::write(pkg.join("main.py"), "x\n").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", pkg.join("leak")).unwrap();
    let err = build_body(&pkg, &request("notes")).unwrap_err();
    assert!(format!("{err}").contains("symlink"), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn signing_refuses_group_writable_content() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("gwrite");
    let pkg = dir.join("notes");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("app.json"), "{}").unwrap();
    fs::write(pkg.join("main.py"), "x\n").unwrap();
    fs::set_permissions(pkg.join("main.py"), fs::Permissions::from_mode(0o664)).unwrap();
    let err = build_body(&pkg, &request("notes")).unwrap_err();
    assert!(format!("{err}").contains("group- or world-writable"), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn entrypoints_must_exist_in_the_tree() {
    let dir = tmpdir("entry");
    let pkg = dir.join("notes");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("app.json"), "{}").unwrap();
    let err = build_body(&pkg, &request("notes")).unwrap_err();
    assert!(format!("{err}").contains("main.py"), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn trust_entry_contains_public_material_only() {
    let key = SigningKeyFile::generate(Some("release".to_string())).unwrap();
    let entry = key.trust_entry(&[PackageKind::App]);
    let text = entry.to_string();
    assert!(text.contains(&key.public_key));
    assert!(!text.contains(&key.private_key));
}
