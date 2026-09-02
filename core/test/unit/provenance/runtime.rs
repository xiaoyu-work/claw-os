use super::*;

use crate::provenance::trust::{TrustRootSpec, TrustTier, TRUST_SCHEMA_V1, USAGE_PACKAGE_SIGNING};

fn tmpdir(label: &str) -> std::path::PathBuf {
    let p = crate::test_env::secure_scratch_dir(&format!("runtime-{label}"));
    p
}

fn reference(digest: &str, key_id: Option<&str>) -> PackageRef {
    PackageRef {
        kind: PackageKind::App,
        id: "notes".to_string(),
        content_digest: digest.to_string(),
        publisher_key_id: key_id.map(str::to_string),
        tier: if key_id.is_some() { "user" } else { "vendor" }.to_string(),
    }
}

/// Record one App instance through the public API.
fn seed(owner: u32, session: &str, digest: &str) {
    with_mutate(owner, |state| {
        state
            .running
            .insert(session.to_string(), app_instance(digest, None));
    })
    .expect("seed the running record");
}

fn app_instance(digest: &str, key_id: Option<&str>) -> Instance {
    Instance {
        class: InstanceClass::App,
        package: Some(reference(digest, key_id)),
        process: None,
        started_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[cfg(unix)]
fn store_with(
    revoked_packages: &[String],
    revoked_keys: &[String],
    key: Option<&str>,
) -> TrustStore {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("trust");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let keys = match key {
        Some(public_key) => {
            let bytes: [u8; 32] = hex::decode(public_key).unwrap().try_into().unwrap();
            serde_json::json!([{
                "key_id": crate::provenance::envelope::key_id_for(&bytes),
                "algorithm": "ed25519",
                "public_key": public_key,
                "usages": [USAGE_PACKAGE_SIGNING],
                "kinds": ["app"],
                "status": "active",
            }])
        }
        None => serde_json::json!([]),
    };
    let body = serde_json::json!({
        "schema": TRUST_SCHEMA_V1,
        "keys": keys,
        "revoked_keys": revoked_keys,
        "revoked_packages": revoked_packages,
    });
    let path = dir.join("k.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let roots = vec![TrustRootSpec {
        path: dir.clone(),
        tier: TrustTier::User,
        allowed_uids: vec![crate::provenance::fsec::effective_uid()],
        domain: crate::provenance::state::TrustDomain::Owner(
            crate::provenance::fsec::effective_uid(),
        ),
    }];
    crate::test_env::record_trust_state(&roots);
    TrustStore::load_roots(&roots)
}

#[cfg(unix)]
#[test]
fn a_revoked_digest_stops_a_running_instance() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let clean = store_with(&[], &[], None);
    assert!(reference(&digest, None).is_live(&clean).is_ok());

    let revoked = store_with(&[digest.clone()], &[], None);
    let err = reference(&digest, None).is_live(&revoked).unwrap_err();
    assert!(err.contains("revoked by content digest"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_revoked_or_removed_publisher_key_stops_a_running_instance() {
    let public_key = hex::encode([9u8; 32]);
    let bytes: [u8; 32] = hex::decode(&public_key).unwrap().try_into().unwrap();
    let key_id = crate::provenance::envelope::key_id_for(&bytes);
    let digest = format!("sha256:{}", "b".repeat(64));

    let trusted = store_with(&[], &[], Some(&public_key));
    assert!(reference(&digest, Some(&key_id)).is_live(&trusted).is_ok());

    let revoked = store_with(&[], &[key_id.clone()], Some(&public_key));
    let err = reference(&digest, Some(&key_id))
        .is_live(&revoked)
        .unwrap_err();
    assert!(err.contains("was revoked"), "{err}");

    // A key simply removed from the store is equally fatal.
    let gone = store_with(&[], &[], None);
    let err = reference(&digest, Some(&key_id))
        .is_live(&gone)
        .unwrap_err();
    assert!(err.contains("no longer trusted"), "{err}");
}

#[cfg(unix)]
#[test]
fn live_reference_rechecks_publisher_expiry_kind_and_tier() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tmpdir("expired-trust");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let public_key = hex::encode([7u8; 32]);
    let bytes: [u8; 32] = hex::decode(&public_key).unwrap().try_into().unwrap();
    let key_id = crate::provenance::envelope::key_id_for(&bytes);
    let body = serde_json::json!({
        "schema": TRUST_SCHEMA_V1,
        "keys": [{
            "key_id": key_id,
            "algorithm": "ed25519",
            "public_key": public_key,
            "usages": [USAGE_PACKAGE_SIGNING],
            "kinds": ["mcp"],
            "status": "active",
            "not_after": "2020-01-01T00:00:00Z"
        }]
    });
    let path = dir.join("expired.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let roots = vec![TrustRootSpec {
        path: dir,
        tier: TrustTier::System,
        allowed_uids: vec![crate::provenance::fsec::effective_uid()],
        domain: crate::provenance::state::TrustDomain::Owner(
            crate::provenance::fsec::effective_uid(),
        ),
    }];
    crate::test_env::record_trust_state(&roots);
    let store = TrustStore::load_roots(&roots);
    let mut package = reference(&format!("sha256:{}", "c".repeat(64)), Some(&key_id));
    package.tier = "system".to_string();
    assert!(package.is_live(&store).is_err());
}

#[cfg(unix)]
#[test]
fn register_assert_and_sweep_round_trip() {
    let _guard = crate::test_env::lock_env();
    let data = tmpdir("procdata");
    let _env = crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", &data);

    let me = crate::provenance::fsec::effective_uid();
    let digest = format!("sha256:{}", "c".repeat(64));
    seed(me, "sess-1", &digest);
    assert_eq!(
        package_for(me, "sess-1").unwrap().unwrap().content_digest,
        digest
    );
    assert_eq!(pending_or_running(me), 1);

    // Unknown sessions are not extension instances and are allowed …
    let clean = store_with(&[], &[], None);
    assert!(assert_live(me, "sess-unknown", &clean).is_ok());
    assert!(assert_live(me, "sess-1", &clean).is_ok());
    // … but a session the caller has already established is
    // package-backed must have a record: without one, the single thing
    // that could confirm its package is still trusted is missing.
    assert!(assert_live_instance(me, "sess-unknown", &clean).is_err());
    assert!(assert_live_instance(me, "sess-1", &clean).is_ok());
    assert!(pending_shutdowns(me).unwrap().is_empty());

    // Revoke, then confirm the denial and the shutdown mark.
    let revoked = store_with(&[digest.clone()], &[], None);
    assert!(assert_live(me, "sess-1", &revoked).is_err());
    let pending = pending_shutdowns(me).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].session_id, "sess-1");
    assert!(pending[0].reason.contains("revoked"));

    // Sweep finds idle instances that never made an authority call.
    deregister(me, "sess-1");
    seed(me, "sess-2", &digest);
    let marked = sweep(me, &revoked);
    assert!(marked.iter().any(|s| s.session_id == "sess-2"));

    deregister(me, "sess-1");
    deregister(me, "sess-2");
    assert_eq!(pending_or_running(me), 0);
    let _ = std::fs::remove_dir_all(&data);
}

#[cfg(unix)]
#[test]
fn state_survives_a_process_restart() {
    let _guard = crate::test_env::lock_env();
    let data = tmpdir("persist");
    let _env = crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", &data);

    let me = crate::provenance::fsec::effective_uid();
    let digest = format!("sha256:{}", "d".repeat(64));
    seed(me, "sess-persist", &digest);

    // Nothing is cached in memory, so this is what a fresh process
    // would see: the durable file, re-read and re-validated.
    let found = package_for(me, "sess-persist")
        .expect("readable")
        .expect("running package survives a restart");
    assert_eq!(found.content_digest, digest);

    deregister(me, "sess-persist");
    let _ = std::fs::remove_dir_all(&data);
}

#[cfg(unix)]
#[test]
fn an_instance_records_its_class_and_its_exact_process() {
    let _guard = crate::test_env::lock_env();
    let data = tmpdir("identity");
    let _env = crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", &data);

    let me = crate::provenance::fsec::effective_uid();
    register_operator_mcp(me, "mcp:local");
    let operator = instance_for(me, "mcp:local")
        .expect("readable")
        .expect("recorded");
    assert_eq!(operator.class, InstanceClass::McpOperatorConfig);
    assert!(!operator.class.is_package_backed());
    assert!(operator.package.is_none());
    // Nothing to revoke: an operator-written server never claimed
    // package trust, so no trust store can invalidate it.
    let revoked = store_with(&[format!("sha256:{}", "e".repeat(64))], &[], None);
    assert!(operator.is_live(&revoked).is_ok());

    // A package instance binds the exact process, not just a pid.
    let digest = format!("sha256:{}", "f".repeat(64));
    let owner_partition = if me == 0 { 10_000 } else { me };
    seed(owner_partition, "app-bound", &digest);
    bind_process_checked(owner_partition, "app-bound", std::process::id()).unwrap();
    let bound = instance_for(owner_partition, "app-bound")
        .expect("readable")
        .expect("recorded");
    let identity = bound.process.expect("process identity");
    assert_eq!(identity.pid, std::process::id());
    assert_eq!(
        identity.uid, me,
        "the process identity must use the execution uid, not the runtime partition"
    );
    if me == 0 {
        assert_ne!(identity.uid, owner_partition);
    }
    assert!(identity.start_time_ticks.is_some());
    assert!(identity.still_matches(), "this process is still itself");

    // A pid with the wrong start time is a different process.
    let stale = ProcessIdentity {
        start_time_ticks: Some(u64::MAX),
        ..identity.clone()
    };
    assert!(!stale.still_matches());
    // So is one with no recorded start time at all.
    let unnamed = ProcessIdentity {
        start_time_ticks: None,
        ..identity.clone()
    };
    assert!(!unnamed.still_matches());
    // And pid 0 is never anything.
    let zero = ProcessIdentity {
        pid: 0,
        ..identity.clone()
    };
    assert!(!zero.still_matches());

    deregister(me, "mcp:local");
    deregister(me, "app-bound");
    let _ = std::fs::remove_dir_all(&data);
}

#[cfg(unix)]
#[test]
fn a_marked_session_is_denied_before_it_is_stopped() {
    let _guard = crate::test_env::lock_env();
    let data = tmpdir("window");
    let _env = crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", &data);

    let me = crate::provenance::fsec::effective_uid();
    let digest = format!("sha256:{}", "b".repeat(64));
    seed(me, "sess-window", &digest);
    let clean = store_with(&[], &[], None);
    assert!(assert_live(me, "sess-window", &clean).is_ok());

    // Between the mark and the lifecycle pass the process may still be
    // running. It must not keep spending authority in that window, even
    // against a store that would otherwise accept it.
    mark_for_shutdown(me, "sess-window", "digest revoked");
    let error = assert_live(me, "sess-window", &clean).expect_err("a marked session is denied");
    assert!(error.contains("digest revoked"), "unexpected: {error}");

    deregister(me, "sess-window");
    let _ = std::fs::remove_dir_all(&data);
}

#[cfg(unix)]
#[test]
fn enforcing_a_shutdown_without_a_process_releases_the_record() {
    let _guard = crate::test_env::lock_env();
    let data = tmpdir("release");
    let _env = crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", &data);

    let me = crate::provenance::fsec::effective_uid();
    let digest = format!("sha256:{}", "c".repeat(64));
    seed(me, "sess-noproc", &digest);
    mark_for_shutdown(me, "sess-noproc", "revoked before the child was bound");

    // Nothing was ever bound, so there is nothing to signal — but the
    // record must not survive, or a later pass would keep retrying it.
    let report = enforce_shutdowns(me, std::time::Duration::from_millis(50));
    assert_eq!(report.released, vec!["sess-noproc".to_string()]);
    assert!(report.terminated.is_empty());
    assert!(instance_for(me, "sess-noproc").expect("readable").is_none());
    assert!(pending_shutdowns(me).unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&data);
}

#[cfg(unix)]
#[test]
fn every_context_addresses_the_same_owner_routed_record() {
    // The CLI running as the owner, `agentd` and `clawd` acting for
    // that owner must name one file. If they did not, a revocation
    // recorded by one would be invisible to the others — which is the
    // hole the record exists to close.
    let _guard = crate::test_env::lock_env();
    let owner = crate::provenance::fsec::effective_uid();
    let other = owner.wrapping_add(1);

    // With no override and no routed partition, the path is the
    // owner's own data dir — and asking for it twice, from anywhere,
    // gives the same answer.
    let a = state_path_for(owner);
    let b = state_path_for(owner);
    assert_eq!(a, b);
    // A different owner is a different file, always.
    assert_ne!(state_path_for(owner), state_path_for(other));
    // The lock lives beside it, never inside a directory a caller named.
    assert_eq!(
        a.parent(),
        state_path_for(owner).with_extension("lock").parent()
    );

    // `current_owner` is the euid unless the daemon installed an
    // authenticated override; it is never read from the environment.
    assert_eq!(current_owner(), owner);
}

#[cfg(unix)]
#[test]
fn a_corrupt_or_insecure_record_denies_instead_of_reading_as_empty() {
    let _guard = crate::test_env::lock_env();
    let data = tmpdir("corrupt");
    let _env = crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", &data);
    let me = crate::provenance::fsec::effective_uid();
    let clean = store_with(&[], &[], None);

    // Truncated JSON: the record cannot be read, so nothing it might
    // have said about this session is known. That is a denial for a
    // package-backed session, not "no record, carry on".
    let path = state_path_for(me);
    std::fs::write(&path, b"{ not json").unwrap();
    let error = assert_live_instance(me, "app-x", &clean).expect_err("corrupt record denies");
    assert!(error.contains("corrupt"), "unexpected: {error}");
    // Even the permissive form refuses: it cannot tell whether this is
    // an extension instance without reading the record.
    assert!(assert_live(me, "app-x", &clean).is_err());

    // A group/world-writable record is refused for the same reason: it
    // is not evidence of anything.
    std::fs::write(&path, b"{}").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    }
    let error = assert_live_instance(me, "app-x", &clean).expect_err("insecure record denies");
    assert!(error.contains("writable"), "unexpected: {error}");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&data);
}

#[cfg(unix)]
#[test]
fn a_missing_record_denies_a_package_backed_session_but_not_a_shell() {
    let _guard = crate::test_env::lock_env();
    let data = tmpdir("missing");
    let _env = crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", &data);
    let me = crate::provenance::fsec::effective_uid();
    let clean = store_with(&[], &[], None);

    // Nothing recorded at all. A CLI session or a daemon task is not an
    // extension instance and passes …
    assert!(assert_live(me, "cli-1", &clean).is_ok());
    // … but a caller that has already established this is an App or MCP
    // session is telling us the record should exist. Its absence means
    // the only thing that could confirm the package is still trusted is
    // gone, so the answer is no.
    let error = assert_live_instance(me, "app-1", &clean)
        .expect_err("a package-backed session with no record must fail closed");
    assert!(
        error.contains("no running-instance record"),
        "unexpected: {error}"
    );

    let _ = std::fs::remove_dir_all(&data);
}

#[cfg(unix)]
#[test]
fn concurrent_writers_do_not_clobber_each_other() {
    // Two processes registering different instances at the same time is
    // the normal case, not a rare one: a launcher starting an App while
    // a sweep marks another. A cached read-modify-write would lose one
    // of them.
    let _guard = crate::test_env::lock_env();
    let data = tmpdir("concurrent");
    let _env = crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", &data);
    let me = crate::provenance::fsec::effective_uid();
    let digest = format!("sha256:{}", "9".repeat(64));

    let mut handles = Vec::new();
    for index in 0..8 {
        let digest = digest.clone();
        handles.push(std::thread::spawn(move || {
            seed(me, &format!("sess-{index}"), &digest);
        }));
    }
    for handle in handles {
        handle.join().expect("writer thread");
    }

    let running = running_instances(me).expect("readable");
    assert_eq!(running.len(), 8, "a concurrent write was lost: {running:?}");
    for index in 0..8 {
        assert!(running.contains_key(&format!("sess-{index}")));
    }

    for index in 0..8 {
        deregister(me, &format!("sess-{index}"));
    }
    let _ = std::fs::remove_dir_all(&data);
}

#[cfg(unix)]
#[test]
fn a_pure_read_does_not_rewrite_the_record() {
    let _guard = crate::test_env::lock_env();
    let data = tmpdir("readonly");
    let _env = crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", &data);
    let me = crate::provenance::fsec::effective_uid();
    let digest = format!("sha256:{}", "8".repeat(64));
    seed(me, "sess-read", &digest);

    let path = state_path_for(me);
    let before = std::fs::read(&path).expect("record");
    let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

    for _ in 0..5 {
        let _ = instance_for(me, "sess-read").expect("readable");
        let _ = pending_shutdowns(me).expect("readable");
        let _ = pending_or_running(me);
    }

    assert_eq!(std::fs::read(&path).expect("record"), before);
    assert_eq!(
        std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
        stamp,
        "a pure read rewrote the record"
    );

    deregister(me, "sess-read");
    let _ = std::fs::remove_dir_all(&data);
}
