//! Fresh-process verification for transported extension package snapshots.

#![cfg(unix)]

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};

const CHILD_ENV: &str = "COS_EXTENSION_PROVENANCE_CHILD";
const SNAPSHOT_ENV: &str = "COS_EXTENSION_PROVENANCE_SNAPSHOT";
const TEST_SEED: [u8; 32] = [
    0xd4, 0x0f, 0x95, 0xd1, 0xf9, 0x6d, 0x42, 0xac, 0x5e, 0x00, 0x00, 0x4e, 0x04, 0x21, 0xc7, 0x0d,
    0xd4, 0xf2, 0x91, 0xb4, 0x71, 0x8e, 0x1a, 0x94, 0xf8, 0xe0, 0xd5, 0xee, 0x20, 0xd5, 0x87, 0x1d,
];

fn snapshot() -> cos::provenance::PackageSnapshot {
    let files = vec![
        ("bin/observer".to_string(), b"binary".to_vec(), true),
        (
            "extension.json".to_string(),
            br#"{"schema_version":1}"#.to_vec(),
            false,
        ),
    ];
    let signed = files
        .iter()
        .map(|(path, bytes, executable)| cos::provenance::SignedFile {
            path: path.clone(),
            sha256: cos::crypto::sha256_hex(bytes),
            size: bytes.len() as u64,
            executable: *executable,
        })
        .collect::<Vec<_>>();
    let mut provenance = cos::provenance::PackageProvenance {
        schema_version: 1,
        kind: cos::provenance::PackageKind::AgentExtension,
        publisher: "claw-os-test".to_string(),
        key_id: "debug-1".to_string(),
        package_id: "observer".to_string(),
        package_version: "1.0.0".to_string(),
        package_digest: cos::provenance::package_digest(&signed),
        files: signed,
        signature: "0".repeat(128),
    };
    provenance.signature = hex::encode(
        SigningKey::from_bytes(&TEST_SEED)
            .sign(&cos::provenance::signing_input(&provenance))
            .to_bytes(),
    );
    cos::provenance::PackageSnapshot {
        provenance,
        files: files
            .into_iter()
            .map(|(path, bytes, executable)| cos::provenance::SnapshotFile {
                path,
                executable,
                bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
            .collect(),
    }
}

#[test]
fn verified_snapshot_survives_transport_and_mutation_fails_in_fresh_process() {
    if let Ok(mode) = std::env::var(CHILD_ENV) {
        let path = std::env::var(SNAPSHOT_ENV).unwrap();
        let snapshot: cos::provenance::PackageSnapshot =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let result = cos::provenance::verify_snapshot(
            &snapshot,
            cos::provenance::PackageKind::AgentExtension,
        );
        if mode == "valid" {
            assert_eq!(result.unwrap().id(), "observer");
        } else {
            assert!(result.unwrap_err().contains("inventory"));
        }
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("snapshot.json");
    let valid = snapshot();
    std::fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
    for mode in ["valid", "mutated"] {
        if mode == "mutated" {
            let mut changed = valid.clone();
            changed.files[0].bytes_base64 =
                base64::engine::general_purpose::STANDARD.encode(b"substituted");
            std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "verified_snapshot_survives_transport_and_mutation_fails_in_fresh_process",
                "--nocapture",
            ])
            .env(CHILD_ENV, mode)
            .env(SNAPSHOT_ENV, &path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode} child failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
