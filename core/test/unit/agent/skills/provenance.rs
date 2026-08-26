use super::*;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

fn tmpdir(label: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "cos-skills-prov-{label}-{}",
        Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

fn write_skill(dir: &Path, id: &str, body: &str, tools: &[&str]) -> LoadedSkill {
    let sd = dir.join(id);
    fs::create_dir_all(&sd).unwrap();
    let mp = sd.join("SKILL.md");
    let allowed = if tools.is_empty() {
        String::new()
    } else {
        format!(
            "allowed-tools:\n{}\n",
            tools
                .iter()
                .map(|t| format!("  - {t}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    fs::write(
        &mp,
        format!("---\nname: {id}\ndescription: test skill\n{allowed}---\n{body}\n"),
    )
    .unwrap();
    let doc = super::super::manifest::parse(&fs::read_to_string(&mp).unwrap()).unwrap();
    LoadedSkill {
        id: id.to_string(),
        dir: sd,
        manifest_path: mp,
        manifest: doc.manifest,
        body_bytes: doc.body.len(),
        body: doc.body,
        origin: super::super::loader::SkillOrigin::Local,
    }
}

#[test]
fn provenance_trust() {
    assert!(Provenance::Vendor.is_trusted());
    assert!(Provenance::User.is_trusted());
    assert!(!Provenance::Hub.is_trusted());
    assert!(!Provenance::Local.is_trusted());
    assert!(!Provenance::Unknown.is_trusted());
}

#[test]
fn provenance_serde_roundtrip() {
    let p = Provenance::Hub;
    let s = serde_json::to_string(&p).unwrap();
    assert_eq!(s, "\"hub\"");
    let back: Provenance = serde_json::from_str(&s).unwrap();
    assert_eq!(back, Provenance::Hub);
}

#[test]
fn usage_store_records_and_aggregates() {
    let dir = tmpdir("usage");
    let store = UsageStore::new(dir.join("usage.jsonl"));
    store
        .record(&UsageRecord {
            skill_id: "pdf-extract".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            success: true,
            duration_ms: 100,
            invoked_by: Some("agent".to_string()),
            resource_path: None,
        })
        .unwrap();
    store
        .record(&UsageRecord {
            skill_id: "pdf-extract".to_string(),
            timestamp: "2025-01-01T00:00:01Z".to_string(),
            success: false,
            duration_ms: 200,
            invoked_by: None,
            resource_path: None,
        })
        .unwrap();
    store
        .record(&UsageRecord {
            skill_id: "arxiv".to_string(),
            timestamp: "2025-01-01T00:00:02Z".to_string(),
            success: true,
            duration_ms: 50,
            invoked_by: None,
            resource_path: None,
        })
        .unwrap();

    let agg = store.aggregate();
    assert_eq!(agg.len(), 2);
    let pdf = &agg["pdf-extract"];
    assert_eq!(pdf.total, 2);
    assert_eq!(pdf.success, 1);
    assert_eq!(pdf.failure, 1);
    assert_eq!(pdf.total_duration_ms, 300);
    assert_eq!(pdf.average_duration_ms(), Some(150));

    let arxiv = &agg["arxiv"];
    assert_eq!(arxiv.total, 1);
    assert_eq!(arxiv.success, 1);

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn usage_store_skips_garbage_lines() {
    let dir = tmpdir("usage-garbage");
    let path = dir.join("usage.jsonl");
    fs::create_dir_all(&dir).unwrap();
    // Hand-write the file with garbage lines mixed in.
    fs::write(
        &path,
        "not-json\n{\"skill_id\":\"x\",\"timestamp\":\"t\",\"success\":true,\"duration_ms\":1}\n\n",
    )
    .unwrap();
    let store = UsageStore::new(&path);
    let agg = store.aggregate();
    assert_eq!(agg.len(), 1);
    assert_eq!(agg["x"].total, 1);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn usage_store_aggregate_empty_when_missing_file() {
    let store = UsageStore::new(tmpdir("missing").join("nope.jsonl"));
    assert!(store.aggregate().is_empty());
}

#[test]
fn usage_stats_average_none_when_zero() {
    let s = UsageStats::default();
    assert_eq!(s.average_duration_ms(), None);
}

#[test]
fn guard_trusted_provenance_short_circuits() {
    let dir = tmpdir("guard-trusted");
    let s = write_skill(&dir, "vendor-skill", "body", &[]);
    let g = Guard::with_default_config();
    assert_eq!(g.check(&s, Provenance::Vendor), GuardOutcome::Allow);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn guard_untrusted_with_default_config_allows() {
    let dir = tmpdir("guard-untrusted-allow");
    let s = write_skill(&dir, "hub-skill", "body", &["tool-a"]);
    let g = Guard::with_default_config();
    assert_eq!(g.check(&s, Provenance::Hub), GuardOutcome::Allow);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn guard_require_allowed_tools_denies_empty() {
    let dir = tmpdir("guard-require-tools");
    let s = write_skill(&dir, "no-tools", "body", &[]);
    let cfg = GuardConfig {
        require_allowed_tools: true,
        ..GuardConfig::default()
    };
    let g = Guard::new(cfg);
    match g.check(&s, Provenance::Hub) {
        GuardOutcome::Deny { reason } => assert!(reason.contains("allowed-tools")),
        other => panic!("expected Deny, got {other:?}"),
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn guard_oversized_sibling_requires_confirmation() {
    let dir = tmpdir("guard-oversized");
    let s = write_skill(&dir, "big-skill", "body", &["tool-a"]);
    // Drop a big file alongside SKILL.md
    let big = s.dir.join("big.bin");
    fs::write(&big, vec![0u8; 1024]).unwrap();
    let cfg = GuardConfig {
        max_file_bytes: 100,
        ..GuardConfig::default()
    };
    let g = Guard::new(cfg);
    match g.check(&s, Provenance::Hub) {
        GuardOutcome::RequireConfirmation { reason } => {
            assert!(reason.contains("big-skill"));
            assert!(reason.contains("big.bin"));
        }
        other => panic!("expected RequireConfirmation, got {other:?}"),
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn guard_can_disable_provenance_trust() {
    let dir = tmpdir("guard-no-trust");
    let s = write_skill(&dir, "vendor-skill", "body", &[]);
    let cfg = GuardConfig {
        honour_provenance_trust: false,
        require_allowed_tools: true,
        ..GuardConfig::default()
    };
    let g = Guard::new(cfg);
    // Vendor with no tools now denied because trust is off.
    assert!(matches!(
        g.check(&s, Provenance::Vendor),
        GuardOutcome::Deny { .. }
    ));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn provenance_as_str_stable() {
    assert_eq!(Provenance::Vendor.as_str(), "vendor");
    assert_eq!(Provenance::Hub.as_str(), "hub");
    assert_eq!(Provenance::User.as_str(), "user");
    assert_eq!(Provenance::Local.as_str(), "local");
    assert_eq!(Provenance::Unknown.as_str(), "unknown");
}

// ----- signature verification helpers -----

#[test]
fn parse_trusted_keys_accepts_colon_separated_hex() {
    let raw = format!("{a}:{b}", a = "aa".repeat(32), b = "bb".repeat(32));
    let keys = parse_trusted_keys(&raw).unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], [0xaa; 32]);
    assert_eq!(keys[1], [0xbb; 32]);
}

#[test]
fn parse_trusted_keys_skips_blank_entries() {
    let raw = format!(":{a}:: ", a = "11".repeat(32));
    let keys = parse_trusted_keys(&raw).unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], [0x11; 32]);
}

#[test]
fn parse_trusted_keys_rejects_wrong_length() {
    let err = parse_trusted_keys("aabb").unwrap_err();
    assert!(matches!(err, SignatureError::WrongLength { .. }));
}

#[test]
fn parse_trusted_keys_rejects_non_hex() {
    let err = parse_trusted_keys("not-hex").unwrap_err();
    assert!(matches!(err, SignatureError::InvalidHex { .. }));
}

#[test]
fn signature_config_from_env_rejects_any_malformed_trusted_key() {
    let _lock = crate::test_env::lock_env();
    let raw = format!("{}:not-hex", "aa".repeat(32));
    let _trusted_keys =
        crate::test_env::TestEnvVarGuard::set(ENV_TRUSTED_KEYS, raw);

    let err = SignatureVerifyConfig::from_env().unwrap_err();
    let message = err.to_string();

    assert!(matches!(
        err,
        SignatureConfigError::InvalidTrustedKeys(SignatureError::InvalidHex { .. })
    ));
    assert!(message.contains(ENV_TRUSTED_KEYS));
    assert!(message.contains("not valid hex"));
}

#[test]
fn signature_config_from_env_preserves_valid_and_absent_values() {
    let _lock = crate::test_env::lock_env();
    let _require_signature =
        crate::test_env::TestEnvVarGuard::set(ENV_REQUIRE_SIGNATURE, "yes");
    let _trusted_keys =
        crate::test_env::TestEnvVarGuard::set(ENV_TRUSTED_KEYS, "");

    std::env::remove_var(ENV_TRUSTED_KEYS);
    let absent = SignatureVerifyConfig::from_env().unwrap();
    assert!(absent.require_signature);
    assert!(absent.trusted_keys.is_none());

    std::env::set_var(ENV_TRUSTED_KEYS, "ab".repeat(32));
    let valid = SignatureVerifyConfig::from_env().unwrap();
    assert!(valid.require_signature);
    assert_eq!(valid.trusted_keys, Some(vec![[0xab; 32]]));
}

#[test]
fn verify_signature_passes_for_valid_block_and_canonical_input() {
    use ed25519_dalek::{Signer, SigningKey};
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let vk = signing_key.verifying_key();
    let mut manifest = SkillManifest {
        name: "sig-ok".into(),
        ..Default::default()
    };
    // Compute signature input WITHOUT a signature block attached
    // (so the signer and verifier compute the same bytes), then
    // attach the signature.
    let canonical = super::super::manifest::canonical_signing_input(&manifest);
    let mut hasher = crate::crypto::Sha256Stream::new();
    hasher.update(&canonical);
    let digest = hasher.finalize_bytes();
    let sig = signing_key.sign(&digest);
    manifest.signature = Some(super::super::manifest::ManifestSignature {
        algorithm: "ed25519".into(),
        public_key: hex::encode(vk.to_bytes()),
        value: hex::encode(sig.to_bytes()),
    });
    let res = verify_signature(&manifest, &SignatureVerifyConfig::default()).unwrap();
    match res {
        SignatureCheck::Verified { public_key_hex } => {
            assert_eq!(public_key_hex, hex::encode(vk.to_bytes()));
        }
        other => panic!("expected Verified, got {other:?}"),
    }
}

#[test]
fn verify_signature_rejects_wrong_key_length() {
    let mut manifest = SkillManifest {
        name: "x".into(),
        ..Default::default()
    };
    manifest.signature = Some(super::super::manifest::ManifestSignature {
        algorithm: "ed25519".into(),
        public_key: "aa".into(), // 1 byte, not 32
        value: "bb".repeat(64),
    });
    let err = verify_signature(&manifest, &SignatureVerifyConfig::default()).unwrap_err();
    assert!(matches!(err, SignatureError::WrongLength { field: "public_key", .. }));
}

#[test]
fn verify_signature_rejects_unsupported_algorithm() {
    let mut manifest = SkillManifest {
        name: "x".into(),
        ..Default::default()
    };
    manifest.signature = Some(super::super::manifest::ManifestSignature {
        algorithm: "rsa-sha256".into(),
        public_key: "aa".repeat(32),
        value: "bb".repeat(64),
    });
    let err = verify_signature(&manifest, &SignatureVerifyConfig::default()).unwrap_err();
    assert!(matches!(err, SignatureError::UnsupportedAlgorithm(_)));
}

#[test]
fn verify_signature_unsigned_passes_by_default_but_fails_when_required() {
    let manifest = SkillManifest {
        name: "x".into(),
        ..Default::default()
    };
    assert!(matches!(
        verify_signature(&manifest, &SignatureVerifyConfig::default()).unwrap(),
        SignatureCheck::Unsigned
    ));
    let strict = SignatureVerifyConfig {
        require_signature: true,
        trusted_keys: None,
    };
    assert!(matches!(
        verify_signature(&manifest, &strict).unwrap_err(),
        SignatureError::Required
    ));
}
