use super::*;

use std::collections::BTreeSet;

use crate::update::floor::{Floor, FloorState, FloorStore};
use crate::update::manifest::Manifest;
use crate::update::signature::Signature;
use crate::update::tests::{fixture_manifest, manifest_bytes, scratch_root, ManifestSpec};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn present(manifest: &Manifest, trusted: &[&str]) -> FloorState {
    let trusted_keys = trusted
        .iter()
        .map(|key| (*key).to_string())
        .collect::<BTreeSet<_>>();
    FloorState::Present {
        floor: Box::new(Floor::bootstrap(
            manifest,
            trusted_keys,
            BTreeMap::new(),
            now(),
        )),
        history_repair_needed: false,
    }
}

fn candidate(spec: &ManifestSpec, signature: Signature) -> Candidate {
    let manifest = fixture_manifest(spec);
    Candidate {
        package: manifest.package.clone(),
        version: manifest.version.clone(),
        manifest,
        signature,
        operation: Operation::Upgrade,
        installed: BTreeMap::new(),
    }
}

fn verified() -> Signature {
    Signature::Verified {
        key_id: "ABCDEF0123456789".to_string(),
        keyring: std::path::PathBuf::from("/usr/share/keyrings/claw-os-archive-keyring.gpg"),
    }
}

#[test]
fn a_first_install_bootstraps_the_floor() {
    let decision = evaluate(
        &candidate(&ManifestSpec::default(), Signature::Absent),
        &FloorState::Uninitialized,
        None,
        now(),
    );
    assert!(decision.allowed);
    assert_eq!(decision.class, class::ALLOWED_BOOTSTRAP);
}

#[test]
fn an_older_but_correctly_signed_release_is_refused() {
    let installed = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    });
    let state = present(&installed, &["ABCDEF0123456789"]);
    let decision = evaluate(
        &candidate(
            &ManifestSpec {
                version: "1:0.2.0+git100.gaaaaaaaaaaaa",
                ..ManifestSpec::default()
            },
            verified(),
        ),
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::VERSION_REGRESSION);
}

#[test]
fn a_lower_epoch_with_a_higher_version_is_refused() {
    let installed = fixture_manifest(&ManifestSpec {
        security_epoch: 3,
        version: "3:0.2.0+git100.gaaaaaaaaaaaa",
        ..ManifestSpec::default()
    });
    let state = present(&installed, &[]);
    let decision = evaluate(
        &candidate(
            &ManifestSpec {
                security_epoch: 2,
                version: "2:0.2.0+git900.gzzzzzzzzzzzz",
                ..ManifestSpec::default()
            },
            Signature::Absent,
        ),
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::EPOCH_REGRESSION);
}

#[test]
fn a_higher_epoch_with_a_lower_version_supersedes_semver_ordering() {
    let installed = fixture_manifest(&ManifestSpec {
        security_epoch: 1,
        version: "1:0.2.0+git900.gzzzzzzzzzzzz",
        ..ManifestSpec::default()
    });
    let state = present(&installed, &[]);
    let decision = evaluate(
        &candidate(
            &ManifestSpec {
                security_epoch: 2,
                version: "2:0.2.0+git100.gaaaaaaaaaaaa",
                ..ManifestSpec::default()
            },
            Signature::Absent,
        ),
        &state,
        None,
        now(),
    );
    assert!(decision.allowed, "{}", decision.message);
    assert_eq!(decision.class, class::ALLOWED);
}

#[test]
fn the_same_version_with_a_different_artifact_is_refused() {
    let installed = fixture_manifest(&ManifestSpec::default());
    let state = present(&installed, &[]);
    let decision = evaluate(
        &candidate(
            &ManifestSpec {
                component_digest: "b".repeat(64),
                ..ManifestSpec::default()
            },
            Signature::Absent,
        ),
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::ARTIFACT_MISMATCH);
}

#[test]
fn reinstalling_the_identical_release_is_allowed() {
    let installed = fixture_manifest(&ManifestSpec::default());
    let state = present(&installed, &[]);
    let decision = evaluate(
        &candidate(&ManifestSpec::default(), Signature::Absent),
        &state,
        None,
        now(),
    );
    assert!(decision.allowed);
    assert_eq!(decision.class, class::ALLOWED_SAME_RELEASE);
}

#[test]
fn an_expired_manifest_is_refused_even_when_it_is_newer() {
    let installed = fixture_manifest(&ManifestSpec::default());
    let state = present(&installed, &[]);
    let decision = evaluate(
        &candidate(
            &ManifestSpec {
                version: "1:0.2.0+git900.gzzzzzzzzzzzz",
                valid_until: "2026-02-01T00:00:00Z",
                ..ManifestSpec::default()
            },
            Signature::Absent,
        ),
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::MANIFEST_EXPIRED);
}

#[test]
fn a_system_installed_from_a_signed_release_refuses_an_unsigned_candidate() {
    let installed = fixture_manifest(&ManifestSpec::default());
    let state = present(&installed, &["ABCDEF0123456789"]);
    let decision = evaluate(
        &candidate(
            &ManifestSpec {
                version: "1:0.2.0+git900.gzzzzzzzzzzzz",
                ..ManifestSpec::default()
            },
            Signature::Absent,
        ),
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::MANIFEST_UNSIGNED);
}

#[test]
fn a_signature_from_an_untrusted_key_is_refused() {
    let installed = fixture_manifest(&ManifestSpec::default());
    let state = present(&installed, &["ABCDEF0123456789"]);
    let decision = evaluate(
        &candidate(
            &ManifestSpec {
                version: "1:0.2.0+git900.gzzzzzzzzzzzz",
                ..ManifestSpec::default()
            },
            Signature::Verified {
                key_id: "0123456789ABCDEF".to_string(),
                keyring: std::path::PathBuf::from("/tmp/other.gpg"),
            },
        ),
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::MANIFEST_UNTRUSTED);
}

#[test]
fn an_unsigned_developer_system_still_enforces_ordering() {
    let installed = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git900.gzzzzzzzzzzzz",
        ..ManifestSpec::default()
    });
    let state = present(&installed, &[]);
    let decision = evaluate(
        &candidate(&ManifestSpec::default(), Signature::Absent),
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::VERSION_REGRESSION);
    assert!(!decision.signature_verified);
}

#[test]
fn a_revoked_release_digest_is_refused() {
    let newer_bytes = manifest_bytes(&ManifestSpec {
        version: "1:0.2.0+git900.gzzzzzzzzzzzz",
        ..ManifestSpec::default()
    });
    let newer_digest = crate::crypto::sha256_hex(&newer_bytes);
    let installed = fixture_manifest(&ManifestSpec {
        revoked_digests: vec![newer_digest],
        ..ManifestSpec::default()
    });
    let state = present(&installed, &[]);
    let decision = evaluate(
        &candidate(
            &ManifestSpec {
                version: "1:0.2.0+git900.gzzzzzzzzzzzz",
                ..ManifestSpec::default()
            },
            Signature::Absent,
        ),
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::DIGEST_REVOKED);
}

#[test]
fn a_manifest_that_does_not_describe_the_candidate_is_refused() {
    let mut subject = candidate(&ManifestSpec::default(), Signature::Absent);
    subject.version = "1:0.2.0+git900.gzzzzzzzzzzzz".to_string();
    let decision = evaluate(&subject, &FloorState::Uninitialized, None, now());
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::MANIFEST_INVALID);
}

#[test]
fn a_release_published_for_another_suite_is_refused() {
    let installed = fixture_manifest(&ManifestSpec::default());
    let state = present(&installed, &[]);
    let decision = evaluate(
        &candidate(
            &ManifestSpec {
                version: "1:0.2.0+git900.gzzzzzzzzzzzz",
                suite: "forky",
                ..ManifestSpec::default()
            },
            Signature::Absent,
        ),
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::SUITE_MISMATCH);
}

#[test]
fn an_incompatible_installed_sibling_blocks_the_candidate() {
    let installed = fixture_manifest(&ManifestSpec::default());
    let state = present(&installed, &[]);
    let mut subject = candidate(
        &ManifestSpec {
            version: "1:0.2.0+git900.gzzzzzzzzzzzz",
            ..ManifestSpec::default()
        },
        Signature::Absent,
    );
    subject
        .installed
        .insert("claw-os-base".to_string(), "0.1.0".to_string());
    let decision = evaluate(&subject, &state, None, now());
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::SET_INCOMPATIBLE);
}

#[test]
fn a_lower_protocol_epoch_is_refused_even_at_a_higher_version() {
    let mut installed_spec = ManifestSpec::default();
    installed_spec
        .protocols
        .insert("agentd_worker".to_string(), 5);
    let state = present(&fixture_manifest(&installed_spec), &[]);

    let mut candidate_spec = ManifestSpec {
        version: "1:0.2.0+git900.gzzzzzzzzzzzz",
        ..ManifestSpec::default()
    };
    candidate_spec
        .protocols
        .insert("agentd_worker".to_string(), 4);
    let decision = evaluate(
        &candidate(&candidate_spec, Signature::Absent),
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::ABI_INCOMPATIBLE);
}

#[test]
fn prerm_refuses_an_older_incoming_version() {
    let installed = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    });
    let state = present(&installed, &[]);
    let decision = evaluate_incoming_version(
        "claw-os-agent",
        "1:0.2.0+git100.gaaaaaaaaaaaa",
        &state,
        None,
        now(),
    );
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::VERSION_REGRESSION);
    assert!(
        decision.message.contains("recover authorize"),
        "{}",
        decision.message
    );
}

#[test]
fn prerm_allows_a_newer_incoming_version() {
    let installed = fixture_manifest(&ManifestSpec::default());
    let state = present(&installed, &[]);
    let decision = evaluate_incoming_version(
        "claw-os-agent",
        "1:0.2.0+git900.gzzzzzzzzzzzz",
        &state,
        None,
        now(),
    );
    assert!(decision.allowed);
}

#[test]
fn a_matching_recovery_authorization_permits_one_downgrade() {
    let root = scratch_root("decide-recovery");
    let store = FloorStore::under_root(&root);
    store.ensure_dir().unwrap();

    let installed = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    });
    let floor = Floor::bootstrap(&installed, BTreeSet::new(), BTreeMap::new(), now());
    store.commit(&floor, "seed").unwrap();
    let state = store.load().unwrap();
    let FloorState::Present { floor, .. } = &state else {
        panic!("expected a floor");
    };

    let older = candidate(
        &ManifestSpec {
            version: "1:0.2.0+git100.gaaaaaaaaaaaa",
            ..ManifestSpec::default()
        },
        Signature::Absent,
    );
    let recovery_store = RecoveryStore::new(&store);
    let authorization = crate::update::recovery::Authorization {
        id: crate::update::recovery::new_id(),
        package: "claw-os-agent".to_string(),
        security_epoch: older.manifest.security_epoch,
        version: older.version.clone(),
        manifest_sha256: older.manifest.digest.clone(),
        reason: "regression in the newer release".to_string(),
        created_at: now(),
        expires_at: now() + chrono::Duration::hours(2),
        created_by_uid: 0,
        floor_generation: floor.generation,
        floor_sha256: Some(floor.digest.clone()),
    };
    recovery_store.write(&authorization).unwrap();

    let decision = evaluate(&older, &state, Some(&recovery_store), now());
    assert!(decision.allowed, "{}", decision.message);
    assert_eq!(decision.class, class::ALLOWED_RECOVERY);

    // The same authorization cannot cover a different version.
    let other = candidate(
        &ManifestSpec {
            version: "1:0.2.0+git150.gdddddddddddd",
            ..ManifestSpec::default()
        },
        Signature::Absent,
    );
    let refused = evaluate(&other, &state, Some(&recovery_store), now());
    assert!(!refused.allowed);
}

#[test]
fn an_expired_recovery_authorization_does_not_permit_a_downgrade() {
    let root = scratch_root("decide-recovery-expired");
    let store = FloorStore::under_root(&root);
    store.ensure_dir().unwrap();
    let installed = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    });
    let floor = Floor::bootstrap(&installed, BTreeSet::new(), BTreeMap::new(), now());
    store.commit(&floor, "seed").unwrap();
    let state = store.load().unwrap();
    let FloorState::Present { floor, .. } = &state else {
        panic!("expected a floor");
    };

    let older = candidate(
        &ManifestSpec {
            version: "1:0.2.0+git100.gaaaaaaaaaaaa",
            ..ManifestSpec::default()
        },
        Signature::Absent,
    );
    let recovery_store = RecoveryStore::new(&store);
    recovery_store
        .write(&crate::update::recovery::Authorization {
            id: crate::update::recovery::new_id(),
            package: "claw-os-agent".to_string(),
            security_epoch: older.manifest.security_epoch,
            version: older.version.clone(),
            manifest_sha256: older.manifest.digest.clone(),
            reason: "stale authorization".to_string(),
            created_at: now() - chrono::Duration::hours(10),
            expires_at: now() - chrono::Duration::hours(1),
            created_by_uid: 0,
            floor_generation: floor.generation,
            floor_sha256: Some(floor.digest.clone()),
        })
        .unwrap();

    let decision = evaluate(&older, &state, Some(&recovery_store), now());
    assert!(!decision.allowed);
    assert_eq!(decision.class, class::VERSION_REGRESSION);
}

#[test]
fn installed_set_compatibility_is_reported_for_the_service_gate() {
    let manifest = fixture_manifest(&ManifestSpec::default());
    let mut installed = BTreeMap::new();
    installed.insert("claw-os-base".to_string(), "0.1.0".to_string());
    assert!(installed_set_is_compatible(&manifest, &installed).is_err());
    installed.insert("claw-os-base".to_string(), "1:0.2.0".to_string());
    assert!(installed_set_is_compatible(&manifest, &installed).is_ok());
}
