use super::*;

use std::collections::{BTreeMap, BTreeSet};

use crate::update::floor::{ComponentFloor, Floor};
use crate::update::tests::{fixture_manifest, scratch_root, ManifestSpec};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn seeded_floor() -> Floor {
    let mut components = BTreeMap::new();
    components.insert(
        "clawd".to_string(),
        ComponentFloor {
            path: "/usr/local/bin/clawd".to_string(),
            sha256: "a".repeat(64),
            size: 12,
            dev: 7,
            ino: 99,
        },
    );
    Floor::bootstrap(
        &fixture_manifest(&ManifestSpec::default()),
        BTreeSet::from(["ABCDEF0123456789".to_string()]),
        components,
        now(),
    )
}

#[test]
fn an_unprotected_machine_reports_no_projection() {
    let root = scratch_root("projection-absent");
    let store = ProjectionStore::under_root(&root);
    assert!(!store.is_established());
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn publishing_produces_a_readable_projection_that_matches_the_floor() {
    let root = scratch_root("projection-publish");
    let store = ProjectionStore::under_root(&root);
    let floor = seeded_floor();
    store.publish(&floor).unwrap();

    assert!(store.is_established());
    let published = store.load().unwrap().expect("a projection");
    assert!(published.matches(&floor));
    assert_eq!(published.security_epoch, floor.security_epoch);
    assert_eq!(published.abi, floor.abi);
    assert_eq!(published.components["clawd"].sha256, "a".repeat(64));
    assert_eq!(
        published.packages["claw-os-agent"].1,
        floor.packages["claw-os-agent"].version
    );
}

#[test]
fn the_projection_carries_no_recovery_or_trust_material() {
    let root = scratch_root("projection-minimal");
    let store = ProjectionStore::under_root(&root);
    store.publish(&seeded_floor()).unwrap();
    let raw = std::fs::read_to_string(store.path()).unwrap();
    for forbidden in [
        "trusted_keys",
        "revoked_digests",
        "recovery",
        "previous_sha256",
        "ABCDEF0123456789",
    ] {
        assert!(
            !raw.contains(forbidden),
            "the runtime projection must not expose `{forbidden}`: {raw}"
        );
    }
}

#[cfg(unix)]
#[test]
fn the_projection_is_world_readable_but_never_group_writable() {
    use std::os::unix::fs::PermissionsExt;

    // Every Claw OS binary that publishes this file runs with a private
    // umask, so the mode has to be asserted after a real write rather
    // than assumed from the open flags.
    let previous = unsafe { libc::umask(0o077) };
    let root = scratch_root("projection-modes");
    let store = ProjectionStore::under_root(&root);
    store.publish(&seeded_floor()).unwrap();
    unsafe { libc::umask(previous) };

    let dir_mode = std::fs::metadata(store.dir()).unwrap().permissions().mode() & 0o7777;
    assert_eq!(dir_mode, 0o755, "the runtime directory must be traversable");
    let file_mode = std::fs::metadata(store.path())
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(file_mode, 0o644, "the projection must be world readable");
    assert_eq!(file_mode & 0o022, 0);
}

#[test]
fn a_missing_projection_in_an_established_directory_fails_closed() {
    let root = scratch_root("projection-missing");
    let store = ProjectionStore::under_root(&root);
    store.publish(&seeded_floor()).unwrap();
    std::fs::remove_file(store.path()).unwrap();
    let error = store.load().unwrap_err();
    assert!(
        matches!(error, ProjectionError::Unreadable(_)),
        "expected a fail-closed refusal, got {error}"
    );
}

#[test]
fn a_corrupt_projection_fails_closed() {
    let root = scratch_root("projection-corrupt");
    let store = ProjectionStore::under_root(&root);
    store.publish(&seeded_floor()).unwrap();
    std::fs::write(store.path(), b"{\"format\":\"nope\"}\n").unwrap();
    assert!(matches!(
        store.load().unwrap_err(),
        ProjectionError::Corrupt(_)
    ));
}

#[test]
fn a_non_canonical_projection_is_refused() {
    let root = scratch_root("projection-noncanonical");
    let store = ProjectionStore::under_root(&root);
    store.publish(&seeded_floor()).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(store.path()).unwrap()).unwrap();
    std::fs::write(
        store.path(),
        serde_json::to_string_pretty(&value).unwrap().as_bytes(),
    )
    .unwrap();
    assert!(matches!(
        store.load().unwrap_err(),
        ProjectionError::Corrupt(_)
    ));
}

#[cfg(unix)]
#[test]
fn a_symlinked_projection_is_refused() {
    let root = scratch_root("projection-symlink");
    let store = ProjectionStore::under_root(&root);
    store.publish(&seeded_floor()).unwrap();
    let elsewhere = root.join("elsewhere.json");
    std::fs::copy(store.path(), &elsewhere).unwrap();
    std::fs::remove_file(store.path()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, store.path()).unwrap();
    assert!(store.load().is_err());
}

#[cfg(unix)]
#[test]
fn a_hardlinked_projection_is_refused() {
    let root = scratch_root("projection-hardlink");
    let store = ProjectionStore::under_root(&root);
    store.publish(&seeded_floor()).unwrap();
    std::fs::hard_link(store.path(), root.join("second-link.json")).unwrap();
    assert!(matches!(
        store.load().unwrap_err(),
        ProjectionError::Insecure(_)
    ));
}

#[cfg(unix)]
#[test]
fn a_world_writable_projection_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let root = scratch_root("projection-writable");
    let store = ProjectionStore::under_root(&root);
    store.publish(&seeded_floor()).unwrap();
    std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(matches!(
        store.load().unwrap_err(),
        ProjectionError::Insecure(_)
    ));
}

#[test]
fn a_stale_projection_does_not_match_a_newer_floor() {
    let root = scratch_root("projection-stale");
    let store = ProjectionStore::under_root(&root);
    let first = seeded_floor();
    store.publish(&first).unwrap();

    let second = first
        .advanced(
            &fixture_manifest(&ManifestSpec {
                version: "1:0.2.0+git900.gzzzzzzzzzzzz",
                ..ManifestSpec::default()
            }),
            None,
            BTreeMap::new(),
            now(),
            crate::update::floor::Advance::Forward,
        )
        .unwrap();

    let published = store.load().unwrap().expect("a projection");
    assert!(published.matches(&first));
    assert!(
        !published.matches(&second),
        "a projection left behind by a newer commit must not look current"
    );

    // Republishing repairs it.
    store.publish(&second).unwrap();
    assert!(store
        .load()
        .unwrap()
        .expect("a projection")
        .matches(&second));
}

#[test]
fn an_edited_projection_does_not_match_even_at_the_same_generation() {
    let root = scratch_root("projection-edited");
    let store = ProjectionStore::under_root(&root);
    let floor = seeded_floor();
    store.publish(&floor).unwrap();

    // Raise the epoch but leave the generation and floor digest alone:
    // a projection comparison that only looked at those would call this
    // current.
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(store.path()).unwrap()).unwrap();
    value["security_epoch"] = serde_json::json!(99);
    std::fs::write(
        store.path(),
        crate::update::canonical::to_bytes(&value).unwrap(),
    )
    .unwrap();

    let published = store.load().unwrap().expect("a projection");
    assert_eq!(published.floor_generation, floor.generation);
    assert_eq!(published.floor_sha256, floor.digest);
    assert!(
        !published.matches(&floor),
        "an edited projection must not look current"
    );
}

#[test]
fn republishing_is_idempotent() {
    let root = scratch_root("projection-idempotent");
    let store = ProjectionStore::under_root(&root);
    let floor = seeded_floor();
    store.publish(&floor).unwrap();
    let first = std::fs::read(store.path()).unwrap();
    store.publish(&floor).unwrap();
    assert_eq!(std::fs::read(store.path()).unwrap(), first);
    let stray = std::fs::read_dir(store.dir())
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".new."))
        .count();
    assert_eq!(stray, 0, "no temporary file may be left behind");
}
