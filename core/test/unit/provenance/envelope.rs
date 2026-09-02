use super::*;

use crate::provenance::envelope as env;

fn file(path: &str, digest_seed: u8) -> FileEntry {
    let mut h = crate::crypto::Sha256Stream::new();
    h.update(&[digest_seed]);
    FileEntry {
        path: path.to_string(),
        kind: NodeKind::File,
        mode: 0o644,
        size: 1,
        digest: format!("sha256:{}", h.finalize_hex()),
    }
}

fn dir(path: &str) -> FileEntry {
    FileEntry {
        path: path.to_string(),
        kind: NodeKind::Dir,
        mode: 0o755,
        size: 0,
        digest: String::new(),
    }
}

fn body(files: Vec<FileEntry>) -> PackageBody {
    let digest = content_digest(&files);
    PackageBody {
        kind: PackageKind::App,
        id: "notes".to_string(),
        version: "1.0.0".to_string(),
        manifest_schema: "cos.app-manifest/v1".to_string(),
        manifest_path: "app.json".to_string(),
        entrypoints: vec![],
        resources: vec![],
        files,
        content_digest: digest,
    }
}

#[test]
fn key_id_binds_to_key_material() {
    let a = key_id_for(&[1u8; 32]);
    let b = key_id_for(&[2u8; 32]);
    assert_ne!(a, b);
    assert!(a.starts_with("sha256:"));
    assert_eq!(a.len(), "sha256:".len() + 64);
    // Deterministic.
    assert_eq!(a, key_id_for(&[1u8; 32]));
}

#[test]
fn canonical_bytes_are_field_injective() {
    let mut left = body(vec![file("app.json", 1)]);
    left.id = "ab".to_string();
    left.version = "c".to_string();
    let mut right = left.clone();
    right.id = "a".to_string();
    right.version = "bc".to_string();
    // Naive concatenation would produce identical bytes for "ab"+"c"
    // and "a"+"bc"; length prefixes keep them distinct.
    assert_ne!(
        canonical_bytes(&left, ALG_ED25519, "sha256:00", "00"),
        canonical_bytes(&right, ALG_ED25519, "sha256:00", "00")
    );
}

#[test]
fn canonical_bytes_cover_algorithm_and_key() {
    let b = body(vec![file("app.json", 1)]);
    let base = canonical_bytes(&b, ALG_ED25519, "sha256:aa", "aa");
    assert_ne!(base, canonical_bytes(&b, "ed25519ph", "sha256:aa", "aa"));
    assert_ne!(base, canonical_bytes(&b, ALG_ED25519, "sha256:bb", "aa"));
    assert_ne!(base, canonical_bytes(&b, ALG_ED25519, "sha256:aa", "bb"));
}

#[test]
fn tree_must_be_sorted_and_unique() {
    let mut b = body(vec![file("b.py", 1), file("a.py", 2), file("app.json", 3)]);
    b.content_digest = content_digest(&b.files);
    let err = b.validate().unwrap_err();
    assert!(matches!(err, EnvelopeError::InvalidTree { .. }), "{err}");

    let mut dup = body(vec![file("app.json", 1), file("app.json", 1)]);
    dup.content_digest = content_digest(&dup.files);
    assert!(matches!(
        dup.validate().unwrap_err(),
        EnvelopeError::InvalidTree { .. }
    ));
}

#[test]
fn case_colliding_names_are_rejected() {
    let mut b = body(vec![file("APP.json", 1), file("app.json", 2)]);
    b.manifest_path = "app.json".to_string();
    b.content_digest = content_digest(&b.files);
    let err = b.validate().unwrap_err();
    assert!(format!("{err}").contains("case-collides"), "{err}");
}

#[test]
fn traversal_and_alternate_separators_are_rejected() {
    for bad in [
        "../escape",
        "/etc/passwd",
        "a\\b",
        "./x",
        "a//b",
        "a/./b",
        "trailing.",
        "trailing ",
    ] {
        assert!(
            env::validate_tree_path(bad).is_err(),
            "expected `{bad}` to be rejected"
        );
    }
    assert!(env::validate_tree_path("a/b/c.py").is_ok());
}

#[test]
fn group_or_world_writable_modes_are_rejected() {
    let mut entry = file("app.json", 1);
    entry.mode = 0o666;
    let mut b = body(vec![entry]);
    b.content_digest = content_digest(&b.files);
    let err = b.validate().unwrap_err();
    assert!(format!("{err}").contains("group/world-writable"), "{err}");
}

#[test]
fn parent_directories_must_be_declared() {
    let mut b = body(vec![file("app.json", 1), file("lib/util.py", 2)]);
    b.content_digest = content_digest(&b.files);
    let err = b.validate().unwrap_err();
    assert!(format!("{err}").contains("parent directory"), "{err}");

    let mut ok = body(vec![file("app.json", 1), dir("lib"), file("lib/util.py", 2)]);
    ok.content_digest = content_digest(&ok.files);
    ok.validate().unwrap();
}

#[test]
fn entrypoints_must_be_signed_files() {
    let mut b = body(vec![file("app.json", 1)]);
    b.entrypoints = vec!["main.py".to_string()];
    b.content_digest = content_digest(&b.files);
    let err = b.validate().unwrap_err();
    assert!(format!("{err}").contains("not a signed regular file"), "{err}");
}

#[test]
fn content_digest_must_match_tree() {
    let mut b = body(vec![file("app.json", 1)]);
    b.content_digest = "sha256:".to_string() + &"0".repeat(64);
    assert_eq!(b.validate().unwrap_err(), EnvelopeError::ContentDigestMismatch);
}

#[test]
fn envelope_cannot_describe_itself() {
    let mut b = body(vec![file(ENVELOPE_FILE, 1), file("app.json", 2)]);
    b.content_digest = content_digest(&b.files);
    let err = b.validate().unwrap_err();
    assert!(format!("{err}").contains("cannot describe itself"), "{err}");
}

fn envelope_json(schema: &str, algorithm: &str, key_id: &str, public_key: &str) -> String {
    let b = body(vec![file("app.json", 1)]);
    let value = serde_json::json!({
        "schema": schema,
        "package": b,
        "signature": {
            "algorithm": algorithm,
            "key_id": key_id,
            "public_key": public_key,
            "value": "0".repeat(128),
        }
    });
    value.to_string()
}

#[test]
fn schema_and_algorithm_confusion_is_rejected() {
    let pk = [7u8; 32];
    let pk_hex = hex::encode(pk);
    let kid = key_id_for(&pk);

    let err = Envelope::parse(&envelope_json("claw.provenance/v2", ALG_ED25519, &kid, &pk_hex))
        .unwrap_err();
    assert!(matches!(err, EnvelopeError::UnsupportedSchema(_)), "{err}");

    for alg in ["ED25519", "ed25519ph", "none", "rsa"] {
        let err = Envelope::parse(&envelope_json(SCHEMA_V1, alg, &kid, &pk_hex)).unwrap_err();
        assert!(
            matches!(err, EnvelopeError::UnsupportedAlgorithm(_)),
            "{alg}: {err}"
        );
    }
}

#[test]
fn key_id_collision_attempt_is_rejected() {
    // Claim a trusted publisher's key id while shipping a different
    // public key.
    let trusted = key_id_for(&[9u8; 32]);
    let attacker_hex = hex::encode([8u8; 32]);
    let err = Envelope::parse(&envelope_json(SCHEMA_V1, ALG_ED25519, &trusted, &attacker_hex))
        .unwrap_err();
    assert!(matches!(err, EnvelopeError::KeyIdMismatch { .. }), "{err}");
}

#[test]
fn unknown_fields_are_rejected() {
    let pk = [7u8; 32];
    let mut value: serde_json::Value =
        serde_json::from_str(&envelope_json(SCHEMA_V1, ALG_ED25519, &key_id_for(&pk), &hex::encode(pk)))
            .unwrap();
    value["package"]["surprise"] = serde_json::json!(true);
    let err = Envelope::parse(&value.to_string()).unwrap_err();
    assert!(matches!(err, EnvelopeError::Malformed(_)), "{err}");
}

#[test]
fn oversized_envelope_is_rejected_before_parsing() {
    let raw = "x".repeat(MAX_ENVELOPE_BYTES + 1);
    assert!(matches!(
        Envelope::parse(&raw).unwrap_err(),
        EnvelopeError::TooLarge { .. }
    ));
}

#[test]
fn package_kind_round_trips() {
    for kind in [PackageKind::App, PackageKind::Skill, PackageKind::Mcp] {
        assert_eq!(PackageKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(PackageKind::parse("kernel"), None);
}
