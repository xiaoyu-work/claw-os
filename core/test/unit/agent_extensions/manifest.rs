use super::*;
use crate::provenance::sign::{self, SignRequest, SigningKeyFile};
use crate::provenance::trust::{
    TrustRootSpec, TrustStore, TrustTier, TRUST_SCHEMA_V1, USAGE_PACKAGE_SIGNING,
};
use crate::provenance::verify::{verify_package, MAX_PACKAGE_BYTES};

fn package(mut mutate: impl FnMut(&mut serde_json::Value)) -> crate::provenance::VerifiedPackage {
    let root = crate::test_env::secure_scratch_dir(&format!(
        "agent-extension-manifest-{}",
        uuid::Uuid::new_v4()
    ));
    let package_dir = root.join("observer");
    let trust_dir = root.join("trust");
    std::fs::create_dir_all(package_dir.join("bin")).unwrap();
    std::fs::create_dir_all(&trust_dir).unwrap();
    let entry = b"#!/bin/sh\nexit 0\n";
    std::fs::write(package_dir.join("bin/observer"), entry).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            package_dir.join("bin/observer"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let content_file = FileEntry {
        path: "bin/observer".to_string(),
        kind: NodeKind::File,
        mode: 0o755,
        size: entry.len() as u64,
        digest: format!("sha256:{}", crate::crypto::sha256_hex(entry)),
    };
    let content_entries = vec![
        FileEntry {
            path: "bin".to_string(),
            kind: NodeKind::Dir,
            mode: 0o755,
            size: 0,
            digest: String::new(),
        },
        content_file,
    ];
    let mut manifest = serde_json::json!({
        "schema_version": 1,
        "identity": {
            "id": "observer",
            "version": "1.0.0",
            "content_digest": tree_content_digest(&content_entries)
        },
        "entry": "bin/observer",
        "protocol": {
            "min_version": 2,
            "max_version": 2,
            "required_features": ["observational-events", "proposed-actions"]
        },
        "subscriptions": ["session-start", "pre-model-call", "post-tool"],
        "requested_capabilities": [{
            "verb": "sys.observe",
            "scope": {"kind": "name", "value": "time"}
        }],
        "action_policies": [{
            "requested_index": 0,
            "tool": "now",
            "policy_id": "builtin.now/v1"
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
    std::fs::write(
        package_dir.join(MANIFEST_FILE),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let key = SigningKeyFile::generate(None).unwrap();
    sign::sign_directory(
        &package_dir,
        &SignRequest {
            kind: crate::provenance::PackageKind::AgentExtension,
            id: "observer".to_string(),
            version: "1.0.0".to_string(),
            manifest_schema: "claw.agent-extension/v1".to_string(),
            manifest_path: MANIFEST_FILE.to_string(),
            entrypoints: vec!["bin/observer".to_string()],
            resources: Vec::new(),
        },
        &key,
    )
    .unwrap();
    let trust_file = serde_json::json!({
        "schema": TRUST_SCHEMA_V1,
        "keys": [{
            "key_id": key.key_id,
            "algorithm": "ed25519",
            "public_key": key.public_key,
            "usages": [USAGE_PACKAGE_SIGNING],
            "kinds": ["extension"],
            "status": "active",
        }]
    });
    let trust_path = trust_dir.join("keys.json");
    std::fs::write(&trust_path, serde_json::to_vec_pretty(&trust_file).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&trust_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let roots = vec![TrustRootSpec {
        path: trust_dir,
        tier: TrustTier::User,
        allowed_uids: vec![crate::provenance::fsec::effective_uid()],
        domain: crate::provenance::state::TrustDomain::Owner(
            crate::provenance::fsec::effective_uid(),
        ),
    }];
    crate::test_env::record_trust_state(&roots);
    let trust = TrustStore::load_roots(&roots);
    verify_package(
        &package_dir,
        &crate::provenance::VerifyOptions {
            kind: crate::provenance::PackageKind::AgentExtension,
            expect_id: Some("observer".to_string()),
            allow_vendor: false,
            allow_developer: false,
            max_bytes: MAX_PACKAGE_BYTES,
        },
        &trust,
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
