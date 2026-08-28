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
    // A skill only exists once its package authenticates, so the
    // fixture signs itself with the process-wide test publisher key.
    crate::test_env::sign_test_package(&sd, crate::provenance::PackageKind::Skill, id);
    let doc = super::super::manifest::parse(&fs::read_to_string(&mp).unwrap()).unwrap();
    let verified = super::super::loader::verify_skill_dir(id, &sd)
        .unwrap_or_else(|e| panic!("verify test skill {id}: {e}"));
    LoadedSkill {
        id: id.to_string(),
        dir: sd,
        manifest_path: mp,
        manifest: doc.manifest,
        body_bytes: doc.body.len(),
        body: doc.body,
        origin: super::super::loader::SkillOrigin::Local,
        provenance: verified,
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

// The ed25519 stack that used to live here (manifest-only signatures,
// `COS_SKILLS_REQUIRE_SIGNATURE`, `COS_SKILLS_TRUSTED_KEYS`) is gone.
// Skills authenticate through `crate::provenance`, which is covered by
// `test/unit/provenance/*`. What remains here is the mapping from a
// verified trust source onto the guard's provenance classes.

#[test]
fn trust_source_maps_onto_guard_provenance() {
    use crate::provenance::TrustSource;
    assert_eq!(
        Provenance::from_trust_source(&TrustSource::Vendor),
        Provenance::Vendor
    );
    assert_eq!(
        Provenance::from_trust_source(&TrustSource::Developer),
        Provenance::Local
    );
    assert_eq!(
        Provenance::from_trust_source(&TrustSource::Publisher {
            key_id: "sha256:aa".to_string()
        }),
        Provenance::Hub
    );
    // Developer-trusted content is never treated as trusted by the
    // guard: it goes through the full check tree.
    assert!(!Provenance::from_trust_source(&TrustSource::Developer).is_trusted());
}

#[test]
fn signed_fixture_reports_its_publisher() {
    let dir = tmpdir("publisher");
    let skill = write_skill(&dir, "signed", "body", &[]);
    assert_eq!(skill.trust_label(), "publisher");
    assert!(skill.publisher_key_id().is_some());
    assert!(skill.content_digest().starts_with("sha256:"));
    let _ = fs::remove_dir_all(&dir);
}
