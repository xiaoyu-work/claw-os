use super::*;

use std::collections::BTreeSet;

use crate::update::tests::{fixture_manifest, scratch_root, ManifestSpec};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn store(label: &str) -> (FloorStore, std::path::PathBuf) {
    let root = scratch_root(label);
    let store = FloorStore::under_root(&root);
    store.ensure_dir().expect("create floor directory");
    (store, root)
}

fn seed(store: &FloorStore, spec: &ManifestSpec) -> Floor {
    let manifest = fixture_manifest(spec);
    let floor = Floor::bootstrap(&manifest, BTreeSet::new(), BTreeMap::new(), now());
    store.commit(&floor, "test bootstrap").expect("commit");
    floor
}

#[test]
fn an_empty_directory_reports_uninitialized_rather_than_failing() {
    let (store, _root) = store("empty");
    assert_eq!(store.load().unwrap(), FloorState::Uninitialized);
}

#[test]
fn a_committed_floor_reloads_with_a_matching_digest() {
    let (store, _root) = store("roundtrip");
    let floor = seed(&store, &ManifestSpec::default());
    let FloorState::Present {
        floor: loaded,
        history_repair_needed,
    } = store.load().unwrap()
    else {
        panic!("expected a present floor");
    };
    assert!(!history_repair_needed);
    assert_eq!(loaded.generation, 1);
    assert_eq!(loaded.digest, floor.digest);
    assert_eq!(
        loaded.packages["claw-os-agent"].version,
        floor.packages["claw-os-agent"].version
    );
}

#[test]
fn rolling_the_state_file_back_alone_is_detected() {
    let (store, _root) = store("rollback");
    let first = seed(&store, &ManifestSpec::default());
    let first_bytes = std::fs::read(store.dir().join("floor.json")).unwrap();

    let manifest = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    });
    let second = first
        .advanced(&manifest, None, BTreeMap::new(), now(), Advance::Forward)
        .expect("advance");
    store.commit(&second, "upgrade").unwrap();

    // Restore only the old state file: the history still remembers a
    // newer generation.
    std::fs::write(store.dir().join("floor.json"), &first_bytes).unwrap();
    let error = store.load().unwrap_err();
    assert!(
        matches!(error, FloorError::Rollback(_)),
        "expected a rollback refusal, got {error}"
    );
}

#[test]
fn truncating_the_history_is_detected() {
    let (store, _root) = store("truncate");
    let first = seed(&store, &ManifestSpec::default());
    let manifest = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    });
    let second = first
        .advanced(&manifest, None, BTreeMap::new(), now(), Advance::Forward)
        .unwrap();
    store.commit(&second, "upgrade").unwrap();
    let third = second
        .advanced(
            &fixture_manifest(&ManifestSpec {
                version: "1:0.2.0+git300.gcccccccccccc",
                ..ManifestSpec::default()
            }),
            None,
            BTreeMap::new(),
            now(),
            Advance::Forward,
        )
        .unwrap();
    store.commit(&third, "upgrade").unwrap();

    // Drop the last two history lines. The floor is now two
    // generations ahead, which no interrupted commit can explain.
    let history = std::fs::read_to_string(store.dir().join("history.jsonl")).unwrap();
    let first_line = history.lines().next().unwrap().to_string();
    std::fs::write(store.dir().join("history.jsonl"), format!("{first_line}\n")).unwrap();
    assert!(matches!(store.load().unwrap_err(), FloorError::Rollback(_)));
}

#[test]
fn an_interrupted_commit_is_accepted_once_and_repaired() {
    let (store, _root) = store("interrupted");
    let first = seed(&store, &ManifestSpec::default());
    let second = first
        .advanced(
            &fixture_manifest(&ManifestSpec {
                version: "1:0.2.0+git200.gbbbbbbbbbbbb",
                ..ManifestSpec::default()
            }),
            None,
            BTreeMap::new(),
            now(),
            Advance::Forward,
        )
        .unwrap();
    // Simulate a crash between the state rename and the history
    // append: write the new state without its history line.
    std::fs::write(store.dir().join("floor.json"), second.to_bytes()).unwrap();

    let FloorState::Present {
        floor,
        history_repair_needed,
    } = store.load().unwrap()
    else {
        panic!("expected a present floor");
    };
    assert!(history_repair_needed);
    assert_eq!(floor.generation, 2);
    store.repair_history(&floor, "repair").unwrap();
    let FloorState::Present {
        history_repair_needed,
        ..
    } = store.load().unwrap()
    else {
        panic!("expected a present floor");
    };
    assert!(!history_repair_needed);
}

#[test]
fn a_missing_state_file_beside_a_history_fails_closed() {
    let (store, _root) = store("missing-state");
    seed(&store, &ManifestSpec::default());
    std::fs::remove_file(store.dir().join("floor.json")).unwrap();
    assert!(matches!(store.load().unwrap_err(), FloorError::Rollback(_)));
}

#[test]
fn a_corrupt_state_file_fails_closed() {
    let (store, _root) = store("corrupt");
    seed(&store, &ManifestSpec::default());
    std::fs::write(store.dir().join("floor.json"), b"{\"format\":\"x\"}\n").unwrap();
    assert!(matches!(store.load().unwrap_err(), FloorError::Corrupt(_)));
}

#[cfg(unix)]
#[test]
fn a_symlinked_state_file_is_refused() {
    let (store, root) = store("symlink");
    seed(&store, &ManifestSpec::default());
    let real = root.join("elsewhere.json");
    std::fs::copy(store.dir().join("floor.json"), &real).unwrap();
    std::fs::remove_file(store.dir().join("floor.json")).unwrap();
    std::os::unix::fs::symlink(&real, store.dir().join("floor.json")).unwrap();
    let error = store.load().unwrap_err();
    assert!(
        matches!(error, FloorError::Unreadable(_) | FloorError::Insecure(_)),
        "expected a refusal, got {error}"
    );
}

#[cfg(unix)]
#[test]
fn a_hardlinked_state_file_is_refused() {
    let (store, root) = store("hardlink");
    seed(&store, &ManifestSpec::default());
    let extra = root.join("second-link.json");
    std::fs::hard_link(store.dir().join("floor.json"), &extra).unwrap();
    let error = store.load().unwrap_err();
    assert!(
        matches!(error, FloorError::Insecure(_)),
        "expected an insecure-state refusal, got {error}"
    );
}

#[cfg(unix)]
#[test]
fn a_world_writable_state_directory_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let (store, _root) = store("world-writable");
    seed(&store, &ManifestSpec::default());
    std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let error = store.load().unwrap_err();
    std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        matches!(error, FloorError::Insecure(_)),
        "expected an insecure-state refusal, got {error}"
    );
}

#[cfg(unix)]
#[test]
fn a_world_writable_state_file_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let (store, _root) = store("writable-file");
    seed(&store, &ManifestSpec::default());
    std::fs::set_permissions(
        store.dir().join("floor.json"),
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert!(matches!(store.load().unwrap_err(), FloorError::Insecure(_)));
}

#[test]
fn advancing_refuses_to_record_a_lower_release() {
    let manifest = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    });
    let floor = Floor::bootstrap(&manifest, BTreeSet::new(), BTreeMap::new(), now());
    let older = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git100.gaaaaaaaaaaaa",
        ..ManifestSpec::default()
    });
    let error = floor
        .advanced(&older, None, BTreeMap::new(), now(), Advance::Forward)
        .unwrap_err();
    assert!(error.contains("below the recorded floor"), "{error}");
}

#[test]
fn an_authorized_recovery_may_record_an_older_release() {
    let manifest = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    });
    let floor = Floor::bootstrap(&manifest, BTreeSet::new(), BTreeMap::new(), now());
    let older = fixture_manifest(&ManifestSpec {
        version: "1:0.2.0+git100.gaaaaaaaaaaaa",
        ..ManifestSpec::default()
    });
    let next = floor
        .advanced(
            &older,
            None,
            BTreeMap::new(),
            now(),
            Advance::AuthorizedRecovery,
        )
        .expect("an operator authorization permits the regression");
    assert_eq!(
        next.packages["claw-os-agent"].version,
        "1:0.2.0+git100.gaaaaaaaaaaaa"
    );
    // The generation still moves forward, so the recovery is itself
    // recorded rather than hidden.
    assert_eq!(next.generation, floor.generation + 1);
}

#[test]
fn advancing_never_lowers_the_epoch_or_a_protocol() {
    let mut spec = ManifestSpec {
        security_epoch: 5,
        version: "5:0.2.0+git100.gaaaaaaaaaaaa",
        ..ManifestSpec::default()
    };
    spec.protocols.insert("agentd_worker".to_string(), 9);
    let floor = Floor::bootstrap(
        &fixture_manifest(&spec),
        BTreeSet::new(),
        BTreeMap::new(),
        now(),
    );

    let mut lower = ManifestSpec {
        security_epoch: 5,
        version: "5:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    };
    lower.protocols.insert("agentd_worker".to_string(), 4);
    let next = floor
        .advanced(
            &fixture_manifest(&lower),
            None,
            BTreeMap::new(),
            now(),
            Advance::Forward,
        )
        .unwrap();
    assert_eq!(next.security_epoch, 5);
    assert_eq!(next.protocols["agentd_worker"], 9);
}

#[test]
fn a_revoked_signing_key_is_dropped_from_the_trusted_set() {
    let manifest = fixture_manifest(&ManifestSpec::default());
    let mut trusted = BTreeSet::new();
    trusted.insert("ABCDEF0123456789".to_string());
    let floor = Floor::bootstrap(&manifest, trusted, BTreeMap::new(), now());

    let mut value: serde_json::Value =
        serde_json::from_slice(&crate::update::tests::manifest_bytes(&ManifestSpec {
            version: "1:0.2.0+git200.gbbbbbbbbbbbb",
            ..ManifestSpec::default()
        }))
        .unwrap();
    value["revoked_keys"] = serde_json::json!(["ABCDEF0123456789"]);
    let bytes = crate::update::canonical::to_bytes(&value).unwrap();
    let rotated = Manifest::parse(&bytes).unwrap();

    let next = floor
        .advanced(
            &rotated,
            Some("FEDCBA9876543210"),
            BTreeMap::new(),
            now(),
            Advance::Forward,
        )
        .unwrap();
    assert!(!next.trusted_keys.contains("ABCDEF0123456789"));
    assert!(next.trusted_keys.contains("FEDCBA9876543210"));
}

#[test]
fn measuring_a_component_records_the_installed_path_not_the_staged_one() {
    let root = scratch_root("measure");
    let staged = root.join("usr/local/bin/clawd");
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    std::fs::write(&staged, b"binary").unwrap();
    let measured = measure_component("clawd", &staged).unwrap();
    assert_eq!(measured.path, "/usr/local/bin/clawd");
    assert_eq!(measured.sha256, crate::crypto::sha256_hex(b"binary"));
    assert_eq!(measured.size, 6);
}

#[cfg(unix)]
#[test]
fn measuring_refuses_a_symlinked_component() {
    let root = scratch_root("measure-symlink");
    let real = root.join("real");
    std::fs::write(&real, b"binary").unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(measure_component("clawd", &link).is_err());
}

#[cfg(unix)]
#[test]
fn concurrent_commits_cannot_lose_an_advance() {
    // Two package scripts configuring at once both read generation N
    // and both prepare generation N+1. Without serialization the
    // second rename would erase the first advance while both callers
    // reported success — a silently lowered floor.
    let (store, _root) = store("concurrent");
    let base = seed(&store, &ManifestSpec::default());

    let first = base
        .advanced(
            &fixture_manifest(&ManifestSpec {
                version: "1:0.2.0+git200.gbbbbbbbbbbbb",
                ..ManifestSpec::default()
            }),
            None,
            BTreeMap::new(),
            now(),
            Advance::Forward,
        )
        .unwrap();
    let second = base
        .advanced(
            &fixture_manifest(&ManifestSpec {
                version: "1:0.2.0+git300.gcccccccccccc",
                ..ManifestSpec::default()
            }),
            None,
            BTreeMap::new(),
            now(),
            Advance::Forward,
        )
        .unwrap();

    let left = store.clone();
    let right = store.clone();
    let a = std::thread::spawn(move || left.commit(&first, "upgrade"));
    let b = std::thread::spawn(move || right.commit(&second, "upgrade"));
    let results = [a.join().unwrap(), b.join().unwrap()];

    let winners = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(winners, 1, "exactly one commit may win: {results:?}");
    let loser = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .unwrap();
    assert!(
        matches!(loser, FloorError::Conflict(_)),
        "the loser must be told to retry, not silently dropped: {loser}"
    );

    // The winner survives intact, and the state still validates.
    let FloorState::Present { floor, .. } = store.load().unwrap() else {
        panic!("expected a present floor");
    };
    assert_eq!(floor.generation, 2);
}

#[cfg(unix)]
#[test]
fn a_commit_prepared_from_a_superseded_floor_is_refused() {
    let (store, _root) = store("stale-advance");
    let base = seed(&store, &ManifestSpec::default());
    let winner = base
        .advanced(
            &fixture_manifest(&ManifestSpec {
                version: "1:0.2.0+git200.gbbbbbbbbbbbb",
                ..ManifestSpec::default()
            }),
            None,
            BTreeMap::new(),
            now(),
            Advance::Forward,
        )
        .unwrap();
    store.commit(&winner, "upgrade").unwrap();

    let stale = base
        .advanced(
            &fixture_manifest(&ManifestSpec {
                version: "1:0.2.0+git300.gcccccccccccc",
                ..ManifestSpec::default()
            }),
            None,
            BTreeMap::new(),
            now(),
            Advance::Forward,
        )
        .unwrap();
    let error = store.commit(&stale, "upgrade").unwrap_err();
    assert!(matches!(error, FloorError::Conflict(_)), "{error}");
    let FloorState::Present { floor, .. } = store.load().unwrap() else {
        panic!("expected a present floor");
    };
    assert_eq!(
        floor.packages["claw-os-agent"].version,
        "1:0.2.0+git200.gbbbbbbbbbbbb"
    );
}
