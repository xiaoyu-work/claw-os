use super::*;

use crate::update::floor::FloorStore;
use crate::update::tests::scratch_root;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn authorization() -> Authorization {
    Authorization {
        id: new_id(),
        package: "claw-os-agent".to_string(),
        security_epoch: 1,
        version: "1:0.2.0+git100.gaaaaaaaaaaaa".to_string(),
        manifest_sha256: "a".repeat(64),
        reason: "release regression".to_string(),
        created_at: now(),
        expires_at: now() + Duration::hours(2),
        created_by_uid: 0,
        floor_generation: 3,
        floor_sha256: Some("b".repeat(64)),
    }
}

fn store(label: &str) -> RecoveryStore {
    let root = scratch_root(label);
    let floor = FloorStore::under_root(&root);
    floor.ensure_dir().unwrap();
    RecoveryStore::new(&floor)
}

#[test]
fn an_authorization_round_trips_through_its_canonical_encoding() {
    let original = authorization();
    let parsed = Authorization::parse(&original.to_bytes()).unwrap();
    assert_eq!(parsed, original);
}

#[test]
fn an_authorization_only_covers_exactly_what_it_names() {
    let subject = authorization();
    assert!(subject
        .authorizes(
            "claw-os-agent",
            "1:0.2.0+git100.gaaaaaaaaaaaa",
            1,
            &"a".repeat(64),
            3,
            Some(&"b".repeat(64)),
            now()
        )
        .is_ok());

    // Wrong component.
    assert!(subject
        .authorizes(
            "claw-os-base",
            "1:0.2.0+git100.gaaaaaaaaaaaa",
            1,
            &"a".repeat(64),
            3,
            Some(&"b".repeat(64)),
            now()
        )
        .is_err());
    // Wrong version.
    assert!(subject
        .authorizes(
            "claw-os-agent",
            "1:0.2.0+git101.gaaaaaaaaaaaa",
            1,
            &"a".repeat(64),
            3,
            Some(&"b".repeat(64)),
            now()
        )
        .is_err());
    // Wrong epoch.
    assert!(subject
        .authorizes(
            "claw-os-agent",
            "1:0.2.0+git100.gaaaaaaaaaaaa",
            2,
            &"a".repeat(64),
            3,
            Some(&"b".repeat(64)),
            now()
        )
        .is_err());
    // Wrong artifact.
    assert!(subject
        .authorizes(
            "claw-os-agent",
            "1:0.2.0+git100.gaaaaaaaaaaaa",
            1,
            &"c".repeat(64),
            3,
            Some(&"b".repeat(64)),
            now()
        )
        .is_err());
    // Floor has moved on: a stored token cannot be replayed later.
    assert!(subject
        .authorizes(
            "claw-os-agent",
            "1:0.2.0+git100.gaaaaaaaaaaaa",
            1,
            &"a".repeat(64),
            4,
            Some(&"b".repeat(64)),
            now()
        )
        .is_err());
    // Expired.
    assert!(subject
        .authorizes(
            "claw-os-agent",
            "1:0.2.0+git100.gaaaaaaaaaaaa",
            1,
            &"a".repeat(64),
            3,
            Some(&"b".repeat(64)),
            now() + Duration::hours(3)
        )
        .is_err());
}

#[test]
fn consuming_an_authorization_removes_it_from_the_pending_set() {
    let store = store("recovery-consume");
    let subject = authorization();
    let path = store.write(&subject).unwrap();
    assert_eq!(store.pending().unwrap().len(), 1);

    store.consume(&path, &subject).unwrap();
    assert!(store.pending().unwrap().is_empty());
    // A replay of the same file is impossible: it is no longer there.
    assert!(store.consume(&path, &subject).is_err());
}

#[test]
fn a_second_authorization_with_the_same_id_is_refused() {
    let store = store("recovery-duplicate");
    let subject = authorization();
    store.write(&subject).unwrap();
    assert!(store.write(&subject).is_err());
}

#[test]
fn revoking_removes_a_pending_authorization() {
    let store = store("recovery-revoke");
    let subject = authorization();
    store.write(&subject).unwrap();
    assert!(store.revoke(&subject.id).unwrap());
    assert!(!store.revoke(&subject.id).unwrap());
    assert!(store.pending().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn an_authorization_readable_by_other_accounts_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let store = store("recovery-mode");
    let subject = authorization();
    let path = store.write(&subject).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.pending().is_err());
}

#[test]
fn lifetimes_are_bounded() {
    assert!(checked_lifetime(0).is_err());
    assert!(checked_lifetime(-1).is_err());
    assert!(checked_lifetime(MAX_LIFETIME_HOURS).is_ok());
    assert!(checked_lifetime(MAX_LIFETIME_HOURS + 1).is_err());
}

#[test]
fn ids_are_validated_before_they_become_file_names() {
    assert!(require_id(&new_id()).is_ok());
    assert!(require_id("../../etc/passwd").is_err());
    assert!(require_id("short").is_err());
}

#[test]
fn a_tampered_authorization_encoding_is_refused() {
    let store = store("recovery-tamper");
    let subject = authorization();
    let path = store.write(&subject).unwrap();
    std::fs::write(
        &path,
        b"{\n  \"format\": \"claw.security-recovery/v1\"\n}\n",
    )
    .unwrap();
    assert!(store.pending().is_err());
}
