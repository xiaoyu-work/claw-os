use super::*;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};

const TEST_SEED: [u8; 32] = [
    0xd4, 0x0f, 0x95, 0xd1, 0xf9, 0x6d, 0x42, 0xac, 0x5e, 0x00, 0x00, 0x4e, 0x04, 0x21,
    0xc7, 0x0d, 0xd4, 0xf2, 0x91, 0xb4, 0x71, 0x8e, 0x1a, 0x94, 0xf8, 0xe0, 0xd5, 0xee,
    0x20, 0xd5, 0x87, 0x1d,
];

fn signed_snapshot() -> PackageSnapshot {
    let files = vec![
        (
            "bin/observer".to_string(),
            b"#!/bin/sh\nexit 0\n".to_vec(),
            true,
        ),
        (
            "extension.json".to_string(),
            br#"{"identity":{"id":"observer","version":"1.0.0","content_digest":"placeholder"}}"#
                .to_vec(),
            false,
        ),
    ];
    let signed_files = files
        .iter()
        .map(|(path, bytes, executable)| SignedFile {
            path: path.clone(),
            sha256: crate::crypto::sha256_hex(bytes),
            size: bytes.len() as u64,
            executable: *executable,
        })
        .collect::<Vec<_>>();
    let mut provenance = PackageProvenance {
        schema_version: PROVENANCE_SCHEMA_VERSION,
        kind: PackageKind::AgentExtension,
        publisher: "claw-os-test".to_string(),
        key_id: "debug-1".to_string(),
        package_id: "observer".to_string(),
        package_version: "1.0.0".to_string(),
        package_digest: package_digest(&signed_files),
        files: signed_files,
        signature: "0".repeat(128),
    };
    let key = SigningKey::from_bytes(&TEST_SEED);
    provenance.signature = hex::encode(key.sign(&signing_input(&provenance)).to_bytes());
    PackageSnapshot {
        provenance,
        files: files
            .into_iter()
            .map(|(path, bytes, executable)| SnapshotFile {
                path,
                executable,
                bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
            .collect(),
    }
}

#[test]
fn verified_snapshot_is_content_complete_and_immutable() {
    let snapshot = signed_snapshot();
    let verified = verify_snapshot(&snapshot, PackageKind::AgentExtension).unwrap();
    assert_eq!(verified.id(), "observer");
    assert_eq!(verified.version(), "1.0.0");
    assert_eq!(verified.publisher(), "claw-os-test");
    assert_eq!(
        verified.file_bytes("bin/observer"),
        Some(b"#!/bin/sh\nexit 0\n".as_slice())
    );
    assert!(verified.file_is_executable("bin/observer"));
    assert_eq!(verified.snapshot(), snapshot);
}

#[test]
fn unsigned_unknown_and_mutated_snapshots_fail_closed() {
    let mut unsigned = signed_snapshot();
    unsigned.provenance.signature = "0".repeat(128);
    assert!(verify_snapshot(&unsigned, PackageKind::AgentExtension)
        .unwrap_err()
        .contains("signature verification"));

    let mut unknown = signed_snapshot();
    unknown.provenance.publisher = "attacker".to_string();
    assert!(verify_snapshot(&unknown, PackageKind::AgentExtension)
        .unwrap_err()
        .contains("not trusted"));

    let mut mutated = signed_snapshot();
    mutated.files[0].bytes_base64 =
        base64::engine::general_purpose::STANDARD.encode(b"substituted");
    assert!(verify_snapshot(&mutated, PackageKind::AgentExtension)
        .unwrap_err()
        .contains("inventory"));
}

#[test]
fn manifest_inventory_drift_and_traversal_are_rejected() {
    let mut drift = signed_snapshot();
    drift.provenance.files[0].executable = false;
    assert!(verify_snapshot(&drift, PackageKind::AgentExtension)
        .unwrap_err()
        .contains("inventory"));

    let mut traversal = signed_snapshot();
    traversal.files[0].path = "../observer".to_string();
    assert!(verify_snapshot(&traversal, PackageKind::AgentExtension)
        .unwrap_err()
        .contains("traversal"));
}

#[test]
fn filesystem_verification_snapshots_before_source_mutation() {
    use std::os::unix::fs::PermissionsExt;

    let snapshot = signed_snapshot();
    let directory = tempfile::tempdir().unwrap();
    for file in &snapshot.files {
        let path = directory.path().join(&file.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&file.bytes_base64)
            .unwrap();
        std::fs::write(&path, bytes).unwrap();
        if file.executable {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    std::fs::write(
        directory.path().join(PROVENANCE_FILE),
        serde_json::to_vec(&snapshot.provenance).unwrap(),
    )
    .unwrap();
    let roots = trust_roots();
    let verified = verify_path_with_roots(
        directory.path(),
        PackageKind::AgentExtension,
        &roots,
        false,
    )
    .unwrap();
    std::fs::write(directory.path().join("bin/observer"), b"changed").unwrap();
    assert_eq!(
        verified.file_bytes("bin/observer"),
        Some(b"#!/bin/sh\nexit 0\n".as_slice())
    );
}

#[test]
fn filesystem_symlinks_are_never_followed() {
    use std::os::unix::fs::symlink;

    let snapshot = signed_snapshot();
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("bin")).unwrap();
    std::fs::write(directory.path().join("target"), b"payload").unwrap();
    symlink(
        directory.path().join("target"),
        directory.path().join("bin/observer"),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("extension.json"),
        base64::engine::general_purpose::STANDARD
            .decode(&snapshot.files[1].bytes_base64)
            .unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.path().join(PROVENANCE_FILE),
        serde_json::to_vec(&snapshot.provenance).unwrap(),
    )
    .unwrap();
    assert!(verify_path_with_roots(
        directory.path(),
        PackageKind::AgentExtension,
        &trust_roots(),
        false,
    )
    .unwrap_err()
    .contains("symlink"));
}
