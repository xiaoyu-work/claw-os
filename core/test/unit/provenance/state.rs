use super::*;

use std::fs;
use std::path::PathBuf;

fn tmpdir(label: &str) -> PathBuf {
    let p = crate::test_env::secure_scratch_dir(&format!("state-{label}"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
    }
    p
}

#[cfg(unix)]
fn domain() -> TrustDomain {
    TrustDomain::Owner(crate::provenance::fsec::effective_uid())
}

fn write_trust_file(dir: &std::path::Path, name: &str, body: &str) {
    fs::write(dir.join(name), body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn fingerprint_covers_names_and_bytes() {
    let a = vec![("k.json".to_string(), b"one".to_vec())];
    let b = vec![("k.json".to_string(), b"two".to_vec())];
    let c = vec![("other.json".to_string(), b"one".to_vec())];
    assert_ne!(fingerprint_files(&a), fingerprint_files(&b));
    assert_ne!(fingerprint_files(&a), fingerprint_files(&c));
    let two = vec![
        ("b.json".to_string(), b"2".to_vec()),
        ("a.json".to_string(), b"1".to_vec()),
    ];
    let two_rev = vec![
        ("a.json".to_string(), b"1".to_vec()),
        ("b.json".to_string(), b"2".to_vec()),
    ];
    assert_eq!(fingerprint_files(&two), fingerprint_files(&two_rev));
}

#[cfg(unix)]
#[test]
fn bump_records_a_monotonic_generation() {
    let dir = tmpdir("bump");
    write_trust_file(&dir, "k.json", "{}");
    let first = bump_in_place(&dir, domain()).unwrap();
    assert_eq!(first.generation, 1);
    let second = bump_in_place(&dir, domain()).unwrap();
    assert_eq!(second.generation, 2);
    assert_eq!(first.fingerprint, second.fingerprint);

    write_trust_file(&dir, "k.json", r#"{"changed":true}"#);
    let third = bump_in_place(&dir, domain()).unwrap();
    assert_eq!(third.generation, 3);
    assert_ne!(third.fingerprint, second.fingerprint);
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn missing_state_is_uninitialised_not_corrupt() {
    let dir = tmpdir("missing");
    assert!(read_state(&dir, domain()).unwrap().is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn corrupt_or_mis_owned_state_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("corrupt");
    write_trust_file(&dir, "k.json", "{}");
    bump_in_place(&dir, domain()).unwrap();

    fs::write(dir.join(TRUST_STATE_FILE), "not json").unwrap();
    fs::set_permissions(dir.join(TRUST_STATE_FILE), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        read_state(&dir, domain()),
        Err(StateError::Corrupt { .. })
    ));

    bump_in_place(&dir, domain()).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join(TRUST_STATE_FILE)).unwrap()).unwrap();
    value["schema"] = serde_json::json!("claw.trust-state/v2");
    fs::write(dir.join(TRUST_STATE_FILE), value.to_string()).unwrap();
    fs::set_permissions(dir.join(TRUST_STATE_FILE), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        read_state(&dir, domain()),
        Err(StateError::Corrupt { .. })
    ));

    bump_in_place(&dir, domain()).unwrap();
    fs::set_permissions(dir.join(TRUST_STATE_FILE), fs::Permissions::from_mode(0o666)).unwrap();
    assert!(matches!(
        read_state(&dir, domain()),
        Err(StateError::Corrupt { .. })
    ));

    fs::set_permissions(dir.join(TRUST_STATE_FILE), fs::Permissions::from_mode(0o600)).unwrap();
    let foreign = TrustDomain::Owner(crate::provenance::fsec::effective_uid().wrapping_add(1));
    assert!(matches!(
        read_state(&dir, foreign),
        Err(StateError::Corrupt { .. })
    ));

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn write_is_atomic_and_leaves_no_temp_file() {
    let dir = tmpdir("atomic");
    write_trust_file(&dir, "k.json", "{}");
    bump_in_place(&dir, domain()).unwrap();
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "temp file leaked: {leftovers:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn watch_notices_a_changed_trust_file() {
    let dir = crate::test_env::secure_scratch_dir("state-watch");
    write_trust_file(&dir, "k.json", "{}");
    // The store-level watch set includes each trust file, because
    // editing one in place changes neither the directory's mtime nor
    // the state file — and a daemon must still notice.
    let roots = vec![crate::provenance::trust::TrustRootSpec {
        path: dir.clone(),
        tier: crate::provenance::TrustTier::User,
        allowed_uids: vec![crate::provenance::fsec::effective_uid()],
        domain: domain(),
    }];
    let paths = crate::provenance::TrustStore::watch_paths(&roots);
    assert!(
        paths.iter().any(|p| p.ends_with("k.json")),
        "individual trust files must be watched: {paths:?}"
    );
    let before = TrustWatch::observe(&paths);
    write_trust_file(&dir, "k.json", r#"{"more":"content"}"#);
    let after = TrustWatch::observe(&paths);
    assert_ne!(before, after);
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn rolled_back_trust_file_no_longer_matches_the_recorded_fingerprint() {
    // Restoring an older trust file without re-recording state is how
    // an attacker would try to undo a revocation. mtime alone could be
    // preserved; the fingerprint cannot.
    let dir = tmpdir("rollback");
    write_trust_file(&dir, "k.json", r#"{"v":1}"#);
    let old = bump_in_place(&dir, domain()).unwrap();
    write_trust_file(&dir, "k.json", r#"{"v":2,"revoked":true}"#);
    let new = bump_in_place(&dir, domain()).unwrap();
    assert!(new.generation > old.generation);

    write_trust_file(&dir, "k.json", r#"{"v":1}"#);
    let recorded = read_state(&dir, domain()).unwrap().unwrap();
    let observed = fingerprint_files(&read_domain_files(&dir).unwrap());
    assert_ne!(
        recorded.fingerprint, observed,
        "a rolled-back trust file must not match the recorded fingerprint"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn domain_keys_are_distinct_per_owner() {
    assert_eq!(TrustDomain::System.as_key(), "system");
    assert_ne!(
        TrustDomain::Owner(1000).as_key(),
        TrustDomain::Owner(1001).as_key()
    );
    assert_eq!(TrustDomain::System.allowed_uids(), vec![0]);
    assert_eq!(TrustDomain::Owner(1000).allowed_uids(), vec![1000]);
}
