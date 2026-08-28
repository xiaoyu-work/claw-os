use super::*;

use std::fs;
use std::path::PathBuf;

use crate::provenance::envelope::{key_id_for, PackageKind};
use crate::provenance::sign::SigningKeyFile;

fn tmpdir(label: &str) -> PathBuf {
    let p = crate::test_env::secure_scratch_dir(&format!("trust-{label}"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

#[cfg(unix)]
fn me() -> u32 {
    crate::provenance::fsec::effective_uid()
}

#[cfg(unix)]
fn user_root(path: &std::path::Path) -> TrustRootSpec {
    TrustRootSpec {
        path: path.to_path_buf(),
        tier: TrustTier::User,
        allowed_uids: vec![me()],
    domain: crate::provenance::state::TrustDomain::Owner(
        crate::provenance::fsec::effective_uid(),
    ),
    }
}

#[cfg(unix)]
fn dev_root(path: &std::path::Path) -> TrustRootSpec {
    TrustRootSpec {
        path: path.to_path_buf(),
        tier: TrustTier::Developer,
        allowed_uids: vec![me()],
    domain: crate::provenance::state::TrustDomain::Owner(
        crate::provenance::fsec::effective_uid(),
    ),
    }
}

fn write(path: &std::path::Path, body: &serde_json::Value) {
    fs::write(path, serde_json::to_vec_pretty(body).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    // Trust files without a recorded generation fail the domain closed
    // in production, so a fixture records it the same way the CLI does.
    if let Some(dir) = path.parent() {
        record_state_for(dir);
    }
}

/// Record the owner domain's generation over one root directory.
fn record_state_for(root: &std::path::Path) {
    crate::test_env::record_trust_state(&[TrustRootSpec {
        path: root.to_path_buf(),
        tier: TrustTier::User,
        allowed_uids: vec![me()],
        domain: crate::provenance::state::TrustDomain::Owner(me()),
    }]);
}

fn entry(key: &SigningKeyFile, kinds: &[&str], status: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": TRUST_SCHEMA_V1,
        "keys": [{
            "key_id": key.key_id,
            "algorithm": "ed25519",
            "public_key": key.public_key,
            "usages": [USAGE_PACKAGE_SIGNING],
            "kinds": kinds,
            "status": status,
        }],
    })
}

fn pk_of(key: &SigningKeyFile) -> [u8; 32] {
    hex::decode(&key.public_key)
        .unwrap()
        .as_slice()
        .try_into()
        .unwrap()
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn digest() -> String {
    format!("sha256:{}", "a".repeat(64))
}

#[cfg(unix)]
#[test]
fn default_roots_are_compiled_in_and_env_free() {
    // Nothing an attacker can set in the environment may introduce a
    // trust root. Only the fixed system paths (plus the passwd-derived
    // per-user roots) may appear.
    let guard = crate::test_env::TestEnvVarGuard::set("COS_TRUST_DIR", "/attacker/keys");
    let roots = TrustStore::default_roots();
    drop(guard);
    assert!(roots
        .iter()
        .all(|r| !r.path.to_string_lossy().contains("attacker")));
    assert_eq!(roots[0].path, std::path::Path::new(VENDOR_TRUST_ROOT));
    assert_eq!(roots[0].allowed_uids, vec![0]);
    assert_eq!(roots[1].path, std::path::Path::new(SYSTEM_TRUST_ROOT));
    assert_eq!(roots[1].allowed_uids, vec![0]);
}

#[cfg(unix)]
#[test]
fn trusted_key_authorises_matching_kind() {
    let dir = tmpdir("authorize");
    let key = SigningKeyFile::generate(None).unwrap();
    write(&dir.join("pub.json"), &entry(&key, &["skill"], "active"));
    let store = TrustStore::load_roots(&[user_root(&dir)]);

    let pk = pk_of(&key);
    store
        .authorize(&key.key_id, &pk, PackageKind::Skill, &digest(), now())
        .unwrap();
    let err = store
        .authorize(&key.key_id, &pk, PackageKind::App, &digest(), now())
        .unwrap_err();
    assert!(matches!(err, TrustError::KindNotPermitted { .. }), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn key_id_must_bind_to_its_public_key() {
    let dir = tmpdir("collide");
    let real = SigningKeyFile::generate(None).unwrap();
    let other = SigningKeyFile::generate(None).unwrap();
    // Claim the real key's id while shipping a different public key.
    let forged = serde_json::json!({
        "schema": TRUST_SCHEMA_V1,
        "keys": [{
            "key_id": real.key_id,
            "algorithm": "ed25519",
            "public_key": other.public_key,
            "usages": [USAGE_PACKAGE_SIGNING],
            "kinds": ["app"],
            "status": "active",
        }],
    });
    write(&dir.join("forged.json"), &forged);
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(store.is_empty());
    assert!(store
        .diagnostics()
        .iter()
        .any(|d| d.contains("does not bind to its public key")));
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn package_signing_usage_is_mandatory() {
    let dir = tmpdir("usage");
    let key = SigningKeyFile::generate(None).unwrap();
    let bad = serde_json::json!({
        "schema": TRUST_SCHEMA_V1,
        "keys": [{
            "key_id": key.key_id,
            "algorithm": "ed25519",
            "public_key": key.public_key,
            "usages": ["release-signing"],
            "kinds": ["app"],
            "status": "active",
        }],
    });
    write(&dir.join("k.json"), &bad);
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(store.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn revoked_key_and_revoked_digest_fail_closed() {
    let dir = tmpdir("revoke");
    let key = SigningKeyFile::generate(None).unwrap();
    write(&dir.join("a.json"), &entry(&key, &["app"], "active"));
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    let pk = pk_of(&key);
    let before = store.generation().to_string();
    store
        .authorize(&key.key_id, &pk, PackageKind::App, &digest(), now())
        .unwrap();

    write(&dir.join("a.json"), &entry(&key, &["app"], "revoked"));
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert_ne!(store.generation(), before, "revocation must move the generation");
    let err = store
        .authorize(&key.key_id, &pk, PackageKind::App, &digest(), now())
        .unwrap_err();
    assert!(matches!(err, TrustError::RevokedKey(_)), "{err}");

    let mut file = entry(&key, &["app"], "active");
    file["revoked_packages"] = serde_json::json!([digest()]);
    write(&dir.join("a.json"), &file);
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(store.is_package_revoked(&digest()));
    let err = store
        .authorize(&key.key_id, &pk, PackageKind::App, &digest(), now())
        .unwrap_err();
    assert!(matches!(err, TrustError::RevokedPackage(_)), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn rotation_bounds_the_predecessor() {
    let dir = tmpdir("rotate");
    let key = SigningKeyFile::generate(None).unwrap();
    let mut file = entry(&key, &["app"], "active");
    file["keys"][0]["not_after"] = serde_json::json!("2025-01-01T00:00:00Z");
    write(&dir.join("k.json"), &file);
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    let err = store
        .authorize(&key.key_id, &pk_of(&key), PackageKind::App, &digest(), now())
        .unwrap_err();
    assert!(matches!(err, TrustError::OutsideValidity { .. }), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_key_is_not_usable_before_its_not_before() {
    let dir = tmpdir("notbefore");
    let key = SigningKeyFile::generate(None).unwrap();
    let mut file = entry(&key, &["app"], "active");
    file["keys"][0]["not_before"] = serde_json::json!("2027-01-01T00:00:00Z");
    write(&dir.join("k.json"), &file);
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    let err = store
        .authorize(&key.key_id, &pk_of(&key), PackageKind::App, &digest(), now())
        .unwrap_err();
    assert!(matches!(err, TrustError::OutsideValidity { .. }), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn validity_windows_are_parsed_as_utc_normalised_rfc3339() {
    // Offsets normalise; the two spellings are the same instant.
    let a = Validity::parse(None, Some("2026-01-01T00:00:00+01:00")).unwrap();
    let b = Validity::parse(None, Some("2025-12-31T23:00:00Z")).unwrap();
    assert_eq!(a.not_after, b.not_after);

    // A comparison that would be wrong lexicographically is right here:
    // "2026-01-01T00:00:00+01:00" sorts after "2025-12-31T23:30:00Z" as
    // text, but is earlier as an instant.
    let boundary = chrono::DateTime::parse_from_rfc3339("2025-12-31T23:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(!a.contains(boundary), "expiry must compare as an instant");

    // Malformed, ambiguous or partial values are rejected outright —
    // never ignored, never treated as "no expiry".
    for bad in [
        "2026-01-01",              // date only
        "2026-01-01 00:00:00Z",    // space separator
        "2026-01-01T00:00:00",     // no offset
        "2026-13-01T00:00:00Z",    // impossible month
        "2026-02-30T00:00:00Z",    // impossible day
        "1999-01-01T00:00:00Z",    // out of supported range
        "2400-01-01T00:00:00Z",    // out of supported range
        "not-a-time",
        "",
        " 2026-01-01T00:00:00Z",   // padded
    ] {
        assert!(
            Validity::parse(None, Some(bad)).is_err(),
            "`{bad}` must be rejected"
        );
    }

    // A leap second is a legal RFC 3339 instant.
    assert!(Validity::parse(None, Some("2026-06-30T23:59:60Z")).is_ok());

    // An impossible window is refused at load time.
    assert!(Validity::parse(
        Some("2026-06-01T00:00:00Z"),
        Some("2026-01-01T00:00:00Z")
    )
    .is_err());

    // No bounds means always valid.
    assert!(Validity::parse(None, None).unwrap().contains(boundary));
}

#[cfg(unix)]
#[test]
fn a_malformed_validity_window_rejects_the_whole_entry() {
    let dir = tmpdir("badtime");
    let key = SigningKeyFile::generate(None).unwrap();
    let mut file = entry(&key, &["app"], "active");
    file["keys"][0]["not_after"] = serde_json::json!("soon");
    write(&dir.join("k.json"), &file);
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(
        store.is_empty(),
        "a key whose expiry cannot be parsed must not authorise anything"
    );
    assert!(store
        .diagnostics()
        .iter()
        .any(|d| d.contains("validity window")));
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn key_material_must_match_the_trusted_entry() {
    let dir = tmpdir("material");
    let key = SigningKeyFile::generate(None).unwrap();
    let other = SigningKeyFile::generate(None).unwrap();
    write(&dir.join("k.json"), &entry(&key, &["app"], "active"));
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    let err = store
        .authorize(
            &key.key_id,
            &pk_of(&other),
            PackageKind::App,
            &digest(),
            now(),
        )
        .unwrap_err();
    assert!(matches!(err, TrustError::KeyMaterialMismatch { .. }), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn insecure_trust_files_are_ignored_with_a_diagnostic() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("perm");
    let key = SigningKeyFile::generate(None).unwrap();
    let path = dir.join("k.json");
    write(&path, &entry(&key, &["app"], "active"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(store.is_empty());
    assert!(store.diagnostics().iter().any(|d| d.contains("mode")));

    // A symlinked trust file is never followed.
    fs::remove_file(&path).unwrap();
    let outside = tmpdir("perm-outside");
    let real = outside.join("real.json");
    write(&real, &entry(&key, &["app"], "active"));
    std::os::unix::fs::symlink(&real, &path).unwrap();
    // Re-record the generation over the domain as it now stands, so
    // the loader gets past the "trust files changed without the state
    // being re-recorded" check and reaches the per-file one. Without
    // this the domain fails closed for a different — also correct —
    // reason, and the symlink rule would go untested.
    record_state_for(&dir);
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(store.is_empty());
    assert!(
        store.diagnostics().iter().any(|d| d.contains("symlink")),
        "expected a symlink diagnostic, got {:?}",
        store.diagnostics()
    );
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&outside);
}

#[cfg(unix)]
#[test]
fn world_writable_trust_root_contributes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("wwroot");
    let key = SigningKeyFile::generate(None).unwrap();
    write(&dir.join("k.json"), &entry(&key, &["app"], "active"));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(store.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn cross_user_trust_root_is_refused() {
    let dir = tmpdir("crossuser");
    let key = SigningKeyFile::generate(None).unwrap();
    write(&dir.join("k.json"), &entry(&key, &["app"], "active"));
    // Pretend the root belongs to a different uid than the files do.
    let foreign = TrustRootSpec {
        path: dir.clone(),
        tier: TrustTier::User,
        allowed_uids: vec![me().wrapping_add(1)],
    domain: crate::provenance::state::TrustDomain::Owner(
        crate::provenance::fsec::effective_uid(),
    ),
    };
    let store = TrustStore::load_roots(&[foreign]);
    assert!(store.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn developer_grants_load_from_the_segregated_root() {
    let publishers = tmpdir("devpub");
    let developer = tmpdir("devgrants");
    let grant = serde_json::json!({
        "schema": DEV_TRUST_SCHEMA_V1,
        "grants": [{
            "kind": "app",
            "id": "scratch",
            "path": "/opt/scratch",
            "content_digest": format!("sha256:{}", "a".repeat(64)),
            "granted_at": "2026-01-01T00:00:00Z",
            "note": "local dev",
        }],
    });
    write(&developer.join("grants.json"), &grant);
    let store = TrustStore::load_roots(&[user_root(&publishers), dev_root(&developer)]);
    let found = store.dev_grant(PackageKind::App, "scratch").unwrap();
    assert_eq!(found.path, std::path::Path::new("/opt/scratch"));
    assert!(store.dev_grant(PackageKind::Skill, "scratch").is_none());
    // Developer trust never authorises privileged routes.
    assert!(!TrustTier::Developer.allows_privileged_routes());
    assert!(TrustTier::Vendor.allows_privileged_routes());
    let _ = fs::remove_dir_all(&publishers);
    let _ = fs::remove_dir_all(&developer);
}

#[cfg(unix)]
#[test]
fn unknown_schema_or_algorithm_is_rejected() {
    let dir = tmpdir("schema");
    let key = SigningKeyFile::generate(None).unwrap();
    let mut file = entry(&key, &["app"], "active");
    file["schema"] = serde_json::json!("claw.trust/v2");
    write(&dir.join("a.json"), &file);
    let mut alg = entry(&key, &["app"], "active");
    alg["keys"][0]["algorithm"] = serde_json::json!("rsa");
    write(&dir.join("b.json"), &alg);
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(store.is_empty());
    assert_eq!(store.diagnostics().len(), 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn key_id_helper_is_a_digest_not_an_alias() {
    let id = key_id_for(&[3u8; 32]);
    assert!(id.starts_with("sha256:"));
    assert_ne!(id, key_id_for(&[4u8; 32]));
}


#[cfg(unix)]
#[test]
fn a_domain_with_trust_files_but_no_state_fails_closed() {
    // Deleting `state.json` must not be the way to reinstate a revoked
    // key. A domain that has trust files was initialised — every
    // command that writes one records the state in the same operation —
    // so a missing state means it was removed afterwards, and the one
    // record of which generation these bytes belong to is gone.
    let dir = tmpdir("nostate");
    let key = SigningKeyFile::generate(None).unwrap();
    write(&dir.join("k.json"), &entry(&key, &["app"], "active"));
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(!store.is_empty(), "the fixture should start usable");

    let state = dir
        .parent()
        .expect("state dir")
        .join(crate::provenance::state::TRUST_STATE_FILE);
    fs::remove_file(&state).expect("remove the recorded generation");

    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(store.is_empty(), "a domain with no recorded generation must contribute nothing");
    assert!(
        store
            .diagnostics()
            .iter()
            .any(|d| d.contains("state.json") && d.contains("missing")),
        "expected a missing-state diagnostic, got {:?}",
        store.diagnostics()
    );

    // Re-recording it restores the domain, which is the documented fix.
    record_state_for(&dir);
    assert!(!TrustStore::load_roots(&[user_root(&dir)]).is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn an_empty_domain_with_no_state_is_a_legitimate_fresh_machine() {
    // The other half of the rule: nothing installed and nothing
    // recorded is a machine where the operator has not added a key.
    // Empty is itself fail-closed — no keys means nothing verifies —
    // so this must not be reported as an error.
    let dir = tmpdir("fresh");
    let store = TrustStore::load_roots(&[user_root(&dir)]);
    assert!(store.is_empty());
    assert!(
        !store.diagnostics().iter().any(|d| d.contains("state.json")),
        "a fresh domain should not raise a state diagnostic: {:?}",
        store.diagnostics()
    );
    let _ = fs::remove_dir_all(&dir);
}
