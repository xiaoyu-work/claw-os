use super::*;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};

const TEST_SEED: [u8; 32] = [
    0xd4, 0x0f, 0x95, 0xd1, 0xf9, 0x6d, 0x42, 0xac, 0x5e, 0x00, 0x00, 0x4e, 0x04, 0x21,
    0xc7, 0x0d, 0xd4, 0xf2, 0x91, 0xb4, 0x71, 0x8e, 0x1a, 0x94, 0xf8, 0xe0, 0xd5, 0xee,
    0x20, 0xd5, 0x87, 0x1d,
];

fn package(mut mutate: impl FnMut(&mut serde_json::Value)) -> crate::provenance::VerifiedPackage {
    let entry = b"#!/bin/sh\nexit 0\n".to_vec();
    let content_file = SignedFile {
        path: "bin/observer".to_string(),
        sha256: crate::crypto::sha256_hex(&entry),
        size: entry.len() as u64,
        executable: true,
    };
    let mut manifest = serde_json::json!({
        "schema_version": 1,
        "identity": {
            "id": "observer",
            "version": "1.0.0",
            "content_digest": package_digest(std::slice::from_ref(&content_file))
        },
        "entry": "bin/observer",
        "protocol": {
            "min_version": 1,
            "max_version": 1,
            "required_features": ["observational-events"]
        },
        "subscriptions": ["session-start", "pre-model-call", "post-tool"],
        "requested_capabilities": [{
            "verb": "ui.notify",
            "scope": {"kind": "wild"}
        }],
        "limits": {
            "event_timeout_ms": 500,
            "queue_capacity": 4,
            "max_output_bytes": 1024,
            "max_actions_per_event": 1,
            "max_in_flight": 1
        }
    });
    mutate(&mut manifest);
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let files = vec![
        content_file,
        SignedFile {
            path: MANIFEST_FILE.to_string(),
            sha256: crate::crypto::sha256_hex(&manifest_bytes),
            size: manifest_bytes.len() as u64,
            executable: false,
        },
    ];
    let mut provenance = crate::provenance::PackageProvenance {
        schema_version: 1,
        kind: crate::provenance::PackageKind::AgentExtension,
        publisher: "claw-os-test".to_string(),
        key_id: "debug-1".to_string(),
        package_id: "observer".to_string(),
        package_version: "1.0.0".to_string(),
        package_digest: package_digest(&files),
        files,
        signature: "0".repeat(128),
    };
    provenance.signature = hex::encode(
        SigningKey::from_bytes(&TEST_SEED)
            .sign(&crate::provenance::signing_input(&provenance))
            .to_bytes(),
    );
    crate::provenance::verify_snapshot(
        &crate::provenance::PackageSnapshot {
            provenance,
            files: vec![
                crate::provenance::SnapshotFile {
                    path: "bin/observer".to_string(),
                    executable: true,
                    bytes_base64: base64::engine::general_purpose::STANDARD.encode(entry),
                },
                crate::provenance::SnapshotFile {
                    path: MANIFEST_FILE.to_string(),
                    executable: false,
                    bytes_base64: base64::engine::general_purpose::STANDARD.encode(manifest_bytes),
                },
            ],
        },
        crate::provenance::PackageKind::AgentExtension,
    )
    .unwrap()
}

#[test]
fn manifest_binds_identity_content_protocol_subscriptions_caps_and_limits() {
    let package = package(|_| {});
    let manifest = ExtensionManifest::parse_verified(&package).unwrap();
    assert_eq!(manifest.identity.id, "observer");
    assert_eq!(manifest.protocol.min_version, ABI_VERSION);
    assert_eq!(manifest.subscriptions.len(), 3);
    assert_eq!(manifest.requested_capabilities.len(), 1);
}

#[test]
fn content_manifest_and_package_drift_fail_closed() {
    let package = package(|manifest| {
        manifest["identity"]["content_digest"] = serde_json::json!("f".repeat(64));
    });
    assert!(ExtensionManifest::parse_verified(&package)
        .unwrap_err()
        .contains("content digest"));
}

#[test]
fn downgrade_unknown_feature_bad_capability_and_unbounded_limits_are_rejected() {
    for (mutator, expected) in [
        (
            Box::new(|manifest: &mut serde_json::Value| {
                manifest["protocol"]["min_version"] = serde_json::json!(0);
            }) as Box<dyn Fn(&mut serde_json::Value)>,
            "protocol range",
        ),
        (
            Box::new(|manifest: &mut serde_json::Value| {
                manifest["protocol"]["required_features"] =
                    serde_json::json!(["authorization-policy"]);
            }),
            "unsupported",
        ),
        (
            Box::new(|manifest: &mut serde_json::Value| {
                manifest["requested_capabilities"][0]["scope"] =
                    serde_json::json!({"kind": "path", "value": "/tmp"});
            }),
            "invalid or duplicate",
        ),
        (
            Box::new(|manifest: &mut serde_json::Value| {
                manifest["limits"]["queue_capacity"] = serde_json::json!(1000);
            }),
            "outside",
        ),
    ] {
        let package = package(|manifest| mutator(manifest));
        assert!(
            ExtensionManifest::parse_verified(&package)
                .unwrap_err()
                .contains(expected),
            "expected {expected}"
        );
    }
}

#[test]
fn additive_manifest_fields_are_accepted() {
    let package = package(|manifest| {
        manifest["protocol"]["max_version"] = serde_json::json!(2);
        manifest["future_optional"] = serde_json::json!({"value": true});
    });
    let manifest = ExtensionManifest::parse_verified(&package).unwrap();
    assert!(manifest.additive.contains_key("future_optional"));
}
