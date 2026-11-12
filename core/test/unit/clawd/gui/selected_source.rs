use super::*;
use std::fs;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};

use crate::provenance::sign::{self, SignRequest, SigningKeyFile};
use crate::provenance::trust::{TrustRootSpec, TrustTier};

#[test]
fn pinned_source_still_rejects_held_fd_tamper_modes_and_identical_directory_replacement() {
    fs::create_dir_all("build").unwrap();
    let scratch = tempfile::Builder::new()
        .prefix("selected-source-")
        .tempdir_in("build")
        .unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let dir = root.join("source-window");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    for (name, bytes) in [
        ("app.json", br#"{"id":"source-window","version":"1","name":{"en":"Source window"},"operations":{}}"#.as_slice()),
        ("main.py", b"print('source')\n".as_slice()),
        ("resource.bin", b"original\0\xff".as_slice()),
    ] {
        fs::write(dir.join(name), bytes).unwrap();
        fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o644)).unwrap();
    }
    let key = SigningKeyFile::generate(None).unwrap();
    let request = SignRequest {
        kind: PackageKind::App,
        id: "source-window".into(),
        version: "1".into(),
        manifest_schema: "cos.app-manifest/v1".into(),
        manifest_path: "app.json".into(),
        entrypoints: vec!["main.py".into()],
        resources: vec!["resource.bin".into()],
    };
    sign::sign_directory(&dir, &request, &key).unwrap();
    let trust_dir = root.join("trust");
    fs::create_dir(&trust_dir).unwrap();
    let public = trust_dir.join("publisher.json");
    fs::write(
        &public,
        serde_json::to_vec(&key.trust_entry(&[PackageKind::App])).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&public, fs::Permissions::from_mode(0o600)).unwrap();
    let uid = crate::provenance::fsec::effective_uid();
    let mut allowed_uids = root
        .ancestors()
        .map(|path| fs::metadata(path).unwrap().uid())
        .collect::<Vec<_>>();
    allowed_uids.push(uid);
    allowed_uids.sort_unstable();
    allowed_uids.dedup();
    let roots = vec![TrustRootSpec {
        path: trust_dir,
        tier: TrustTier::User,
        allowed_uids,
        domain: crate::provenance::state::TrustDomain::Owner(uid),
    }];
    crate::test_env::record_trust_state(&roots);
    let trust = TrustStore::load_roots(&roots);
    let options = VerifyOptions::new(PackageKind::App).expect_id("source-window");
    let retained = verify_package(&dir, &options, &trust).unwrap();
    assert!(reverify(&retained, &trust).is_ok());

    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("resource.bin"))
        .unwrap();
    let modified = held.metadata().unwrap().modified().unwrap();
    held.write_all_at(b"modified\0\xff", 0).unwrap();
    held.set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(held.metadata().unwrap().modified().unwrap(), modified);
    assert!(
        reverify(&retained, &trust).is_err(),
        "same-size data changed through a pre-existing FD must still be hashed"
    );
    held.write_all_at(b"original\0\xff", 0).unwrap();
    assert!(reverify(&retained, &trust).is_ok());
    held.set_permissions(fs::Permissions::from_mode(0o600))
        .unwrap();
    assert!(reverify(&retained, &trust).is_err());
    held.set_permissions(fs::Permissions::from_mode(0o644))
        .unwrap();
    assert!(reverify(&retained, &trust).is_ok());
    fs::write(
        dir.join("unsigned.bin"),
        b"not in the authenticated inventory",
    )
    .unwrap();
    assert!(reverify(&retained, &trust).is_err());
    fs::remove_file(dir.join("unsigned.bin")).unwrap();
    assert!(reverify(&retained, &TrustStore::load_roots(&[])).is_err());
    let mut changed_contract = request.clone();
    changed_contract.resources.clear();
    sign::sign_directory(&dir, &changed_contract, &key).unwrap();
    assert!(verify_package(&dir, &options, &trust).is_ok());
    assert!(reverify(&retained, &trust).is_err());
    sign::sign_directory(&dir, &request, &key).unwrap();
    assert!(reverify(&retained, &trust).is_ok());

    let previous = root.join("previous");
    fs::rename(&dir, &previous).unwrap();
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    for name in [
        "app.json",
        "main.py",
        "resource.bin",
        crate::provenance::envelope::ENVELOPE_FILE,
    ] {
        fs::copy(previous.join(name), dir.join(name)).unwrap();
    }
    assert!(verify_package(&dir, &options, &trust).is_ok());
    assert!(
        reverify(&retained, &trust).is_err(),
        "even identical signed replacement bytes need a fresh registration"
    );
}
