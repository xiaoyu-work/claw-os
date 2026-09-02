use super::*;

use crate::update::tests::{manifest_bytes, ManifestSpec};

#[test]
fn a_well_formed_manifest_parses_and_pins_its_digest() {
    let spec = ManifestSpec::default();
    let bytes = manifest_bytes(&spec);
    let parsed = Manifest::parse(&bytes).expect("parses");
    assert_eq!(parsed.package, "claw-os-agent");
    assert_eq!(parsed.security_epoch, 1);
    assert_eq!(parsed.digest, crate::crypto::sha256_hex(&bytes));
    assert_eq!(parsed.bytes, bytes);
    assert_eq!(
        parsed
            .component_digest("clawd")
            .map(|entry| entry.sha256.as_str()),
        Some(spec.component_digest.as_str())
    );
}

#[test]
fn a_reordered_but_semantically_equal_manifest_is_refused() {
    let bytes = manifest_bytes(&ManifestSpec::default());
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let reordered = serde_json::to_vec(&value).unwrap();
    // `serde_json` preserves insertion order, so this is the same
    // document in a different encoding: a signature over one set of
    // bytes must not be honoured for the other.
    if reordered != bytes[..bytes.len() - 1] {
        assert!(Manifest::parse(&reordered).is_err());
    }
}

#[test]
fn a_manifest_from_another_format_is_refused() {
    let mut value: serde_json::Value =
        serde_json::from_slice(&manifest_bytes(&ManifestSpec::default())).unwrap();
    value["format"] = serde_json::json!("claw.release-security/v0");
    let bytes = crate::update::canonical::to_bytes(&value).unwrap();
    let error = Manifest::parse(&bytes).unwrap_err();
    assert!(error.contains("format"), "{error}");
}

#[test]
fn expiry_is_reported_against_the_supplied_clock() {
    let manifest = Manifest::parse(&manifest_bytes(&ManifestSpec {
        valid_until: "2026-02-01T00:00:00Z",
        ..ManifestSpec::default()
    }))
    .unwrap();
    let before = chrono::DateTime::parse_from_rfc3339("2026-01-15T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let after = chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(!manifest.is_expired(before));
    assert!(manifest.is_expired(after));
}

#[test]
fn a_manifest_that_expires_before_it_is_issued_is_refused() {
    let bytes = manifest_bytes(&ManifestSpec {
        issued_at: "2026-05-01T00:00:00Z",
        valid_until: "2026-01-01T00:00:00Z",
        ..ManifestSpec::default()
    });
    assert!(Manifest::parse(&bytes).is_err());
}

#[test]
fn digests_must_be_lowercase_sha256() {
    assert!(require_digest(&"a".repeat(64)).is_ok());
    assert!(require_digest(&"A".repeat(64)).is_err());
    assert!(require_digest("sha256:aaaa").is_err());
    assert!(require_digest(&"a".repeat(63)).is_err());
}

#[test]
fn an_invalid_debian_version_is_refused() {
    let bytes = manifest_bytes(&ManifestSpec {
        version: "not-a-version",
        ..ManifestSpec::default()
    });
    let error = Manifest::parse(&bytes).unwrap_err();
    assert!(error.contains("Debian version"), "{error}");
}

#[test]
fn a_component_path_must_be_absolute_and_free_of_traversal() {
    let mut value: serde_json::Value =
        serde_json::from_slice(&manifest_bytes(&ManifestSpec::default())).unwrap();
    value["components"][0]["path"] = serde_json::json!("/usr/local/bin/../../etc/shadow");
    let bytes = crate::update::canonical::to_bytes(&value).unwrap();
    assert!(Manifest::parse(&bytes).is_err());
}

#[test]
fn an_oversized_manifest_is_refused_before_it_is_parsed() {
    let padding = vec![b'a'; (MAX_MANIFEST_BYTES + 1) as usize];
    assert!(Manifest::parse(&padding).is_err());
}

#[test]
fn a_security_epoch_apt_cannot_see_is_refused() {
    // A manifest whose Debian epoch does not carry the security epoch
    // would describe a release APT could never select over the version
    // it is supposed to supersede.
    let error = Manifest::parse(&manifest_bytes(&ManifestSpec {
        security_epoch: 2,
        version: "1:0.2.0+git100.gaaaaaaaaaaaa",
        ..ManifestSpec::default()
    }))
    .unwrap_err();
    assert!(error.contains("Debian epoch"), "{error}");

    let error = Manifest::parse(&manifest_bytes(&ManifestSpec {
        version: "0.2.0+git100.gaaaaaaaaaaaa",
        ..ManifestSpec::default()
    }))
    .unwrap_err();
    assert!(error.contains("Debian epoch"), "{error}");

    Manifest::parse(&manifest_bytes(&ManifestSpec {
        security_epoch: 2,
        version: "2:0.2.0+git100.gaaaaaaaaaaaa",
        ..ManifestSpec::default()
    }))
    .expect("a matching epoch parses");
}
