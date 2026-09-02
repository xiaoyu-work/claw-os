use super::*;

use std::collections::{BTreeMap, BTreeSet};

use crate::update::floor::{ComponentFloor, Floor, FloorStore};
use crate::update::projection::ProjectionStore;
use crate::update::tests::{fixture_manifest, scratch_root, ManifestSpec};

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

struct Fixture {
    root: std::path::PathBuf,
    store: FloorStore,
    projection: ProjectionStore,
}

/// Install a fake `clawd` under `root`, record it in a fresh floor, and
/// publish the matching runtime projection — the state a configured
/// package leaves behind.
fn seeded(label: &str, epoch: u64, contents: &[u8]) -> Fixture {
    let root = scratch_root(label);
    let binary = root.join("usr/local/bin/clawd");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, contents).unwrap();

    let mut components = BTreeMap::new();
    components.insert(
        "clawd".to_string(),
        ComponentFloor {
            path: "/usr/local/bin/clawd".to_string(),
            sha256: crate::crypto::sha256_hex(contents),
            size: contents.len() as u64,
            dev: 0,
            ino: 0,
        },
    );

    // The Debian epoch always mirrors the security epoch, so a
    // fixture that varies one has to vary the other.
    let version: &str = Box::leak(format!("{epoch}:0.2.0+git100.gaaaaaaaaaaaa").into_boxed_str());
    let manifest = fixture_manifest(&ManifestSpec {
        security_epoch: epoch,
        version,
        ..ManifestSpec::default()
    });
    let store = FloorStore::under_root(&root);
    store.ensure_dir().unwrap();
    let floor = Floor::bootstrap(&manifest, BTreeSet::new(), components, now());
    store.commit(&floor, "test").unwrap();
    let projection = ProjectionStore::under_root(&root);
    projection.publish(&floor).unwrap();
    Fixture {
        root,
        store,
        projection,
    }
}

#[test]
fn a_system_with_no_floor_does_not_block_startup() {
    let root = scratch_root("runtime-nofloor");
    let store = FloorStore::under_root(&root);
    let projection = ProjectionStore::under_root(&root);
    assert!(enforce_startup_in(&projection, &root, Scope::CriticalComponents).is_ok());
    assert!(enforce_broker_startup_in(&store, &projection, &root).is_ok());
}

#[test]
fn a_build_below_the_recorded_epoch_refuses_to_run() {
    let fixture = seeded("runtime-epoch", crate::update::SECURITY_EPOCH + 1, b"clawd");
    let refusal =
        enforce_startup_in(&fixture.projection, &fixture.root, Scope::CompiledEpoch).unwrap_err();
    assert_eq!(
        refusal.class,
        crate::update::decide::class::EPOCH_REGRESSION
    );
    assert!(refusal.message.contains("superseded build"), "{refusal}");

    let broker =
        enforce_broker_startup_in(&fixture.store, &fixture.projection, &fixture.root).unwrap_err();
    assert_eq!(broker.class, crate::update::decide::class::EPOCH_REGRESSION);
}

#[test]
fn a_build_at_the_recorded_epoch_starts() {
    let fixture = seeded("runtime-current", crate::update::SECURITY_EPOCH, b"clawd");
    assert!(enforce_startup_in(
        &fixture.projection,
        &fixture.root,
        Scope::CriticalComponents
    )
    .is_ok());
    assert!(enforce_broker_startup_in(&fixture.store, &fixture.projection, &fixture.root).is_ok());
}

#[test]
fn a_replaced_critical_component_refuses_to_run() {
    let fixture = seeded("runtime-replaced", crate::update::SECURITY_EPOCH, b"clawd");
    std::fs::write(fixture.root.join("usr/local/bin/clawd"), b"older clawd").unwrap();

    let refusal = enforce_startup_in(
        &fixture.projection,
        &fixture.root,
        Scope::CriticalComponents,
    )
    .unwrap_err();
    assert_eq!(
        refusal.class,
        crate::update::decide::class::ARTIFACT_MISMATCH
    );

    let broker =
        enforce_broker_startup_in(&fixture.store, &fixture.projection, &fixture.root).unwrap_err();
    assert_eq!(
        broker.class,
        crate::update::decide::class::ARTIFACT_MISMATCH
    );
}

#[test]
fn a_replaced_component_is_ignored_when_only_the_epoch_is_checked() {
    let fixture = seeded(
        "runtime-epoch-only",
        crate::update::SECURITY_EPOCH,
        b"clawd",
    );
    std::fs::write(fixture.root.join("usr/local/bin/clawd"), b"older clawd").unwrap();
    assert!(enforce_startup_in(&fixture.projection, &fixture.root, Scope::CompiledEpoch).is_ok());
}

#[test]
fn an_unreadable_authority_fails_the_broker_closed() {
    let fixture = seeded("runtime-corrupt", crate::update::SECURITY_EPOCH, b"clawd");
    std::fs::write(
        fixture.store.dir().join("floor.json"),
        b"{\"format\":\"nope\"}\n",
    )
    .unwrap();
    let refusal =
        enforce_broker_startup_in(&fixture.store, &fixture.projection, &fixture.root).unwrap_err();
    assert_eq!(
        refusal.class,
        crate::update::decide::class::FLOOR_UNAVAILABLE
    );
}

#[test]
fn an_unreadable_projection_fails_unprivileged_callers_closed() {
    let fixture = seeded(
        "runtime-projection-corrupt",
        crate::update::SECURITY_EPOCH,
        b"clawd",
    );
    std::fs::write(fixture.projection.path(), b"{\"format\":\"nope\"}\n").unwrap();
    let refusal =
        enforce_startup_in(&fixture.projection, &fixture.root, Scope::CompiledEpoch).unwrap_err();
    assert_eq!(
        refusal.class,
        crate::update::decide::class::FLOOR_UNAVAILABLE
    );
}

#[test]
fn a_deleted_projection_fails_unprivileged_callers_closed() {
    let fixture = seeded(
        "runtime-projection-missing",
        crate::update::SECURITY_EPOCH,
        b"clawd",
    );
    std::fs::remove_file(fixture.projection.path()).unwrap();
    assert!(enforce_startup_in(&fixture.projection, &fixture.root, Scope::CompiledEpoch).is_err());
}

#[test]
fn the_broker_repairs_a_stale_projection() {
    let fixture = seeded("runtime-stale", crate::update::SECURITY_EPOCH, b"clawd");
    let stale = std::fs::read(fixture.projection.path()).unwrap();

    // Advance the authority without republishing: exactly what an
    // interrupted commit leaves behind.
    let FloorState::Present { floor, .. } = fixture.store.load().unwrap() else {
        panic!("expected a floor");
    };
    let next = floor
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
    fixture.store.commit(&next, "upgrade").unwrap();
    assert_eq!(std::fs::read(fixture.projection.path()).unwrap(), stale);

    assert!(enforce_broker_startup_in(&fixture.store, &fixture.projection, &fixture.root).is_ok());
    assert_ne!(
        std::fs::read(fixture.projection.path()).unwrap(),
        stale,
        "the broker must republish a stale runtime view"
    );
    assert!(fixture
        .projection
        .load()
        .unwrap()
        .expect("a projection")
        .matches(&next));
}

#[test]
fn a_projection_without_an_authority_fails_the_broker_closed() {
    let fixture = seeded("runtime-orphan", crate::update::SECURITY_EPOCH, b"clawd");
    std::fs::remove_file(fixture.store.dir().join("floor.json")).unwrap();
    std::fs::remove_file(fixture.store.dir().join("history.jsonl")).unwrap();
    let refusal =
        enforce_broker_startup_in(&fixture.store, &fixture.projection, &fixture.root).unwrap_err();
    assert_eq!(
        refusal.class,
        crate::update::decide::class::FLOOR_UNAVAILABLE
    );
}

#[test]
fn a_component_the_floor_does_not_record_is_not_treated_as_a_downgrade() {
    let fixture = seeded(
        "runtime-unrecorded",
        crate::update::SECURITY_EPOCH,
        b"clawd",
    );
    // `cos` is a critical component but was never measured here.
    assert!(enforce_startup_in(
        &fixture.projection,
        &fixture.root,
        Scope::CriticalComponents
    )
    .is_ok());
}

#[test]
fn a_worker_binary_that_does_not_match_the_floor_is_refused() {
    let root = scratch_root("runtime-worker");
    let worker = root.join("usr/local/bin/claw-agentd");
    std::fs::create_dir_all(worker.parent().unwrap()).unwrap();
    std::fs::write(&worker, b"worker").unwrap();

    let mut components = BTreeMap::new();
    components.insert(
        "claw-agentd".to_string(),
        ComponentFloor {
            path: "/usr/local/bin/claw-agentd".to_string(),
            sha256: crate::crypto::sha256_hex(b"worker"),
            size: 6,
            dev: 0,
            ino: 0,
        },
    );
    let store = FloorStore::under_root(&root);
    store.ensure_dir().unwrap();
    let floor = Floor::bootstrap(
        &fixture_manifest(&ManifestSpec::default()),
        BTreeSet::new(),
        components,
        now(),
    );
    store.commit(&floor, "test").unwrap();

    // A development worker outside the packaged path is not measured.
    assert!(enforce_worker_binary_in(&store, &worker).is_ok());
}

#[test]
fn a_peer_reporting_another_epoch_is_refused() {
    assert!(check_peer_epoch("claw-agentd", crate::update::SECURITY_EPOCH).is_ok());
    let refusal = check_peer_epoch("claw-agentd", crate::update::SECURITY_EPOCH + 1).unwrap_err();
    assert_eq!(
        refusal.class,
        crate::update::decide::class::EPOCH_REGRESSION
    );
    assert!(
        refusal.message.contains("reinstall claw-os-agent"),
        "{refusal}"
    );
}
