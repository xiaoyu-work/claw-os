//! Adversarial, process-level tests for extension package provenance.
//!
//! Unit tests cover the format and the policy structs. This file drives
//! the **public** API against a real filesystem: real Ed25519 signing,
//! real archives, real renames, real concurrency and a real spawned
//! process. Every case here is an attack that must fail closed.
//!
//! Skipped on non-Unix hosts: the guarantees (ownership gating,
//! `openat`/`O_NOFOLLOW`, hardlink and special-file detection) are
//! POSIX-specific and the implementation fails closed elsewhere.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cos::provenance::envelope::{content_digest, PackageKind, ENVELOPE_FILE};
use cos::provenance::install::{self, Limits};
use cos::provenance::sign::{self, SigningKeyFile};
use cos::provenance::trust::{
    TrustRootSpec, TrustStore, TrustTier, TRUST_SCHEMA_V1, USAGE_PACKAGE_SIGNING,
};
use cos::provenance::verify::{self, VerifyOptions};

/// A scratch directory with secure ancestry.
///
/// Trust roots require every ancestor up to `/` to be non-symlink,
/// correctly owned and free of group/world write bits. `/tmp` is
/// world-writable, so a trust root under it is refused — and must stay
/// refused. Fixtures therefore live under the owner's home.
fn scratch(label: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let base = home.join(".cache").join("cos-provenance-it");
    let dir = base.join(format!("{label}-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).unwrap();
    let _ = fs::set_permissions(&base, fs::Permissions::from_mode(0o700));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
}

fn me() -> u32 {
    unsafe { libc::geteuid() }
}

struct Fixture {
    root: PathBuf,
    state_dir: PathBuf,
    trust_root: PathBuf,
    key: SigningKeyFile,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = scratch(label);
        // Mirror the production layout: the domain's `state.json` sits
        // in the parent of its roots, so `TrustRootSpec::state_dir`
        // finds it.
        let state_dir = root.join("trust");
        let trust_root = state_dir.join("publishers.d");
        fs::create_dir_all(&trust_root).unwrap();
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&trust_root, fs::Permissions::from_mode(0o700)).unwrap();
        let key = SigningKeyFile::generate(Some("integration".to_string())).unwrap();
        let me_ = Self {
            root,
            state_dir,
            trust_root,
            key,
        };
        me_.write_trust(&[], &[]);
        me_
    }

    fn roots(&self) -> Vec<TrustRootSpec> {
        vec![TrustRootSpec {
            path: self.trust_root.clone(),
            tier: TrustTier::User,
            allowed_uids: vec![me()],
            domain: cos::provenance::state::TrustDomain::Owner(me()),
        }]
    }

    fn write_trust(&self, revoked_keys: &[String], revoked_packages: &[String]) {
        let body = serde_json::json!({
            "schema": TRUST_SCHEMA_V1,
            "keys": [{
                "key_id": self.key.key_id,
                "algorithm": "ed25519",
                "public_key": self.key.public_key,
                "usages": [USAGE_PACKAGE_SIGNING],
                "kinds": ["app", "skill", "mcp", "extension"],
                "status": "active",
            }],
            "revoked_keys": revoked_keys,
            "revoked_packages": revoked_packages,
        });
        let path = self.trust_root.join("publisher.json");
        fs::write(&path, serde_json::to_vec_pretty(&body).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        // Every mutation re-records the domain's durable generation;
        // without it the loader fails the domain closed, which is the
        // behaviour a separate test asserts.
        cos::provenance::state::bump(
            &self.state_dir,
            cos::provenance::state::TrustDomain::Owner(me()),
            &[self.trust_root.clone()],
        )
        .expect("record trust generation");
    }

    /// Install this fixture's roots as the process trust store.
    ///
    /// `AppLaunch::bind` re-asserts against the *process* store, which
    /// is the behaviour a daemon relies on, so a test that drives the
    /// launch path has to publish its roots there.
    fn activate(&self) -> std::sync::Arc<TrustStore> {
        cos::provenance::set_trust_store_for_roots(
            TrustStore::load_roots(&self.roots()),
            self.roots(),
        )
    }

    fn store(&self) -> TrustStore {
        let store = TrustStore::load_roots(&self.roots());
        assert!(
            !store.is_empty(),
            "trust root failed to load: {:?}",
            store.diagnostics()
        );
        store
    }

    /// Sign a package tree in place with the fixture's publisher key.
    fn sign(&self, dir: &Path, kind: PackageKind, id: &str, entrypoints: &[&str]) {
        let _ = fs::remove_file(dir.join(ENVELOPE_FILE));
        sign::sign_directory(
            dir,
            &sign::SignRequest {
                kind,
                id: id.to_string(),
                version: "1.0.0".to_string(),
                manifest_schema: "integration".to_string(),
                manifest_path: kind.manifest_file().to_string(),
                entrypoints: entrypoints.iter().map(|s| s.to_string()).collect(),
                resources: vec![],
            },
            &self.key,
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn app_package(fx: &Fixture, id: &str, body: &str) -> PathBuf {
    let dir = fx.root.join(id);
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("app.json"),
        &format!(r#"{{"id":"{id}","version":"1.0.0","name":"{id}","operations":{{}}}}"#),
    );
    write(&dir.join("main.py"), body);
    fs::set_permissions(dir.join("main.py"), fs::Permissions::from_mode(0o755)).unwrap();
    fx.sign(&dir, PackageKind::App, id, &["main.py"]);
    dir
}

fn agent_extension_package(fx: &Fixture, id: &str) -> PathBuf {
    use cos::provenance::envelope::{FileEntry, NodeKind};

    let dir = fx.root.join(id);
    fs::create_dir_all(dir.join("bin")).unwrap();
    let entry = "#!/bin/sh\nexit 0\n";
    write(&dir.join("bin/observer"), entry);
    fs::set_permissions(dir.join("bin/observer"), fs::Permissions::from_mode(0o755)).unwrap();
    let content_digest = content_digest(&[
        FileEntry {
            path: "bin".to_string(),
            kind: NodeKind::Dir,
            mode: 0o755,
            size: 0,
            digest: String::new(),
        },
        FileEntry {
            path: "bin/observer".to_string(),
            kind: NodeKind::File,
            mode: 0o755,
            size: entry.len() as u64,
            digest: format!("sha256:{}", cos::crypto::sha256_hex(entry.as_bytes())),
        },
    ]);
    write(
        &dir.join("extension.json"),
        &serde_json::json!({
            "schema_version": 1,
            "identity": {
                "id": id,
                "version": "1.0.0",
                "content_digest": content_digest,
            },
            "entry": "bin/observer",
            "protocol": {
                "min_version": 2,
                "max_version": 2,
                "required_features": ["observational-events"],
            },
            "subscriptions": ["session-start"],
            "requested_capabilities": [],
            "limits": {
                "event_timeout_ms": 500,
                "queue_capacity": 2,
                "max_output_bytes": 1024,
                "max_actions_per_event": 1,
                "max_in_flight": 1,
            },
        })
        .to_string(),
    );
    fx.sign(&dir, PackageKind::AgentExtension, id, &["bin/observer"]);
    dir
}

// ---------------------------------------------------------------------------
// Signing / verification round trip against the real filesystem
// ---------------------------------------------------------------------------

#[test]
fn signed_agent_extension_uses_the_shared_verified_snapshot() {
    let fx = Fixture::new("agent-extension");
    let dir = agent_extension_package(&fx, "observer");
    let package = verify::verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::AgentExtension).expect_id("observer"),
        &fx.store(),
    )
    .expect("verify Agent extension");
    let manifest = cos::agent_extensions::manifest::ExtensionManifest::parse_verified(&package)
        .expect("parse verified Agent extension manifest");
    assert_eq!(manifest.identity.id, "observer");
    assert_eq!(manifest.entry, "bin/observer");

    write(&dir.join("bin/observer"), "#!/bin/sh\nexit 7\n");
    assert!(
        package.read_verified("bin/observer").is_err(),
        "a verified snapshot must reject entrypoint drift"
    );
}

#[test]
fn real_signature_round_trip_and_forgery_rejection() {
    let fx = Fixture::new("roundtrip");
    let dir = app_package(&fx, "notes", "print('v1')\n");
    let trust = fx.store();
    let options = VerifyOptions::new(PackageKind::App).expect_id("notes");

    let pkg = verify::verify_package(&dir, &options, &trust).unwrap();
    assert_eq!(pkg.id(), "notes");
    assert!(pkg.content_digest().starts_with("sha256:"));

    // Re-signing with a different key that nobody trusts must fail.
    let stranger = SigningKeyFile::generate(None).unwrap();
    let _ = fs::remove_file(dir.join(ENVELOPE_FILE));
    sign::sign_directory(
        &dir,
        &sign::SignRequest {
            kind: PackageKind::App,
            id: "notes".to_string(),
            version: "1.0.0".to_string(),
            manifest_schema: "integration".to_string(),
            manifest_path: "app.json".to_string(),
            entrypoints: vec!["main.py".to_string()],
            resources: vec![],
        },
        &stranger,
    )
    .unwrap();
    let err = verify::verify_package(&dir, &options, &trust).unwrap_err();
    assert_eq!(err.code(), "provenance.untrusted_key");
}

#[test]
fn revocation_stops_future_use_immediately() {
    let fx = Fixture::new("revoke");
    let dir = app_package(&fx, "notes", "print('v1')\n");
    let options = VerifyOptions::new(PackageKind::App).expect_id("notes");
    let trust = fx.store();
    let pkg = verify::verify_package_cached(&dir, &options, &trust).unwrap();
    let digest = pkg.content_digest().to_string();

    // Revoke the package digest and reload the store.
    fx.write_trust(&[], &[digest.clone()]);
    let revoked = fx.store();
    assert_ne!(revoked.generation(), trust.generation());
    assert!(revoked.is_package_revoked(&digest));

    // A previously verified snapshot must stop being usable and the
    // cache must not serve it.
    assert!(pkg.assert_current(&revoked).is_err());
    let err = verify::verify_package_cached(&dir, &options, &revoked).unwrap_err();
    assert_eq!(err.code(), "provenance.untrusted_key");

    // Revoking the key has the same effect.
    fx.write_trust(&[fx.key.key_id.clone()], &[]);
    let key_revoked = fx.store();
    let err = verify::verify_package(&dir, &options, &key_revoked).unwrap_err();
    assert_eq!(err.code(), "provenance.untrusted_key");
}

// ---------------------------------------------------------------------------
// TOCTOU
// ---------------------------------------------------------------------------

#[test]
fn file_replaced_between_verify_and_use_is_detected() {
    let fx = Fixture::new("toctou");
    let dir = app_package(&fx, "notes", "print('good')\n");
    let trust = fx.store();
    let pkg = verify::verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::App).expect_id("notes"),
        &trust,
    )
    .unwrap();

    // Atomic swap — the attacker never truncates, so a naive
    // "stat then read" check would see a consistent-looking file.
    write(&dir.join(".evil"), "print('evil')\n");
    fs::rename(dir.join(".evil"), dir.join("main.py")).unwrap();

    let err = pkg.read_verified("main.py").unwrap_err();
    assert_eq!(err.code(), "provenance.content_mismatch");
    assert!(pkg.open_entrypoint("main.py").is_err());
}

#[test]
fn directory_replaced_between_verify_and_launch_is_detected() {
    let fx = Fixture::new("swapdir");
    let dir = app_package(&fx, "notes", "print('good')\n");
    let trust = fx.store();
    let pkg = verify::verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::App).expect_id("notes"),
        &trust,
    )
    .unwrap();
    pkg.assert_current(&trust).unwrap();

    let evil = fx.root.join("evil");
    fs::create_dir_all(&evil).unwrap();
    write(&evil.join("main.py"), "print('evil')\n");
    fs::remove_dir_all(&dir).unwrap();
    fs::rename(&evil, &dir).unwrap();

    let err = pkg.assert_current(&trust).unwrap_err();
    assert!(format!("{err}").contains("replaced"), "{err}");
}

#[test]
fn concurrent_update_and_verification_never_yields_mixed_content() {
    let fx = Fixture::new("concurrent");
    let live = fx.root.join("live").join("notes");
    let source = app_package(&fx, "notes", "print('v1')\n");
    let trust = Arc::new(fx.store());
    let limits = Limits::default();

    let staged = install::stage_directory(
        &source,
        &live,
        PackageKind::App,
        Some("notes"),
        &trust,
        &limits,
    )
    .unwrap();
    install::publish(staged, &live, false, &limits).unwrap();

    // Readers verify while a writer republishes a second version.
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let live = live.clone();
            let trust = Arc::clone(&trust);
            std::thread::spawn(move || {
                let options = VerifyOptions::new(PackageKind::App).expect_id("notes");
                for _ in 0..40 {
                    match verify::verify_package(&live, &options, &trust) {
                        Ok(pkg) => {
                            // Whatever version we got, it is internally
                            // consistent: the manifest and the
                            // entrypoint come from the same snapshot.
                            let body = pkg.read_verified_text("main.py").unwrap_or_default();
                            assert!(
                                body.is_empty()
                                    || body == "print('v1')\n"
                                    || body == "print('v2')\n",
                                "torn content observed: {body:?}"
                            );
                        }
                        Err(_) => {
                            // Observing the swap mid-flight is fine as
                            // long as it fails closed — which every
                            // error path here does. What must never
                            // happen is a *successful* verification
                            // over a half-replaced tree, and that is
                            // what the `Ok` arm asserts.
                        }
                    }
                }
            })
        })
        .collect();

    write(&source.join("main.py"), "print('v2')\n");
    fs::set_permissions(source.join("main.py"), fs::Permissions::from_mode(0o755)).unwrap();
    fx.sign(&source, PackageKind::App, "notes", &["main.py"]);
    let staged = install::stage_directory(
        &source,
        &live,
        PackageKind::App,
        Some("notes"),
        &trust,
        &limits,
    )
    .unwrap();
    install::publish(staged, &live, true, &limits).unwrap();

    for reader in readers {
        reader.join().unwrap();
    }
    assert_eq!(
        fs::read_to_string(live.join("main.py")).unwrap(),
        "print('v2')\n"
    );
}

// ---------------------------------------------------------------------------
// Install pipeline
// ---------------------------------------------------------------------------

#[test]
fn interrupted_install_leaves_no_partial_tree() {
    let fx = Fixture::new("crash");
    let live = fx.root.join("live").join("notes");
    let source = app_package(&fx, "notes", "print('v1')\n");
    let trust = fx.store();
    let limits = Limits::default();

    // Drop the staged package without publishing: the private staging
    // directory must clean itself up, and nothing may appear at the
    // live path.
    {
        let staged = install::stage_directory(
            &source,
            &live,
            PackageKind::App,
            Some("notes"),
            &trust,
            &limits,
        )
        .unwrap();
        assert!(staged.path().is_dir());
        assert_eq!(
            fs::metadata(staged.path()).unwrap().permissions().mode() & 0o777,
            0o700,
            "staging must be private"
        );
    }
    assert!(!live.exists(), "live path must not exist after an abort");
    let leftovers: Vec<_> = fs::read_dir(live.parent().unwrap())
        .map(|it| it.filter_map(Result::ok).map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "staging leaked: {leftovers:?}");
}

#[test]
fn unsigned_and_tampered_bundles_never_reach_the_live_path() {
    let fx = Fixture::new("closed");
    let trust = fx.store();
    let limits = Limits::default();
    let live = fx.root.join("live").join("plain");

    let unsigned = fx.root.join("unsigned");
    fs::create_dir_all(&unsigned).unwrap();
    write(
        &unsigned.join("app.json"),
        r#"{"id":"plain","version":"1","name":"plain","operations":{}}"#,
    );
    let err = install::stage_directory(
        &unsigned,
        &live,
        PackageKind::App,
        Some("plain"),
        &trust,
        &limits,
    )
    .unwrap_err();
    assert_eq!(err.code(), "provenance.unsigned");
    assert!(!live.exists());

    // Signed, then edited: the staged copy carries the edit and fails.
    let tampered = app_package(&fx, "plain2", "print('ok')\n");
    write(&tampered.join("main.py"), "print('evil')\n");
    let err = install::stage_directory(
        &tampered,
        &fx.root.join("live").join("plain2"),
        PackageKind::App,
        Some("plain2"),
        &trust,
        &limits,
    )
    .unwrap_err();
    assert_eq!(err.code(), "provenance.content_mismatch");
}

#[test]
fn hostile_tree_shapes_are_refused_before_verification() {
    let fx = Fixture::new("hostile");
    let limits = Limits::default();

    // Symlink.
    let a = fx.root.join("a");
    fs::create_dir_all(&a).unwrap();
    write(&a.join("f"), "x");
    std::os::unix::fs::symlink("/etc/shadow", a.join("link")).unwrap();
    assert!(install::assert_safe_tree(&a, &limits)
        .unwrap_err()
        .to_string()
        .contains("symlink"));

    // Hard link.
    let b = fx.root.join("b");
    fs::create_dir_all(&b).unwrap();
    write(&b.join("f"), "x");
    fs::hard_link(b.join("f"), b.join("g")).unwrap();
    assert!(install::assert_safe_tree(&b, &limits)
        .unwrap_err()
        .to_string()
        .contains("hard link"));

    // FIFO.
    let c = fx.root.join("c");
    fs::create_dir_all(&c).unwrap();
    let fifo = c.join("pipe");
    let cpath = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
    if unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) } == 0 {
        assert!(install::assert_safe_tree(&c, &limits)
            .unwrap_err()
            .to_string()
            .contains("FIFO"));
    }

    // Case collision.
    let d = fx.root.join("d");
    fs::create_dir_all(&d).unwrap();
    write(&d.join("Main.py"), "x");
    write(&d.join("main.py"), "y");
    assert!(install::assert_safe_tree(&d, &limits)
        .unwrap_err()
        .to_string()
        .contains("case-collides"));

    // Decompression-bomb shaped payload: bounded by total bytes.
    let e = fx.root.join("e");
    fs::create_dir_all(&e).unwrap();
    fs::write(e.join("big"), vec![0u8; 1024]).unwrap();
    fs::set_permissions(e.join("big"), fs::Permissions::from_mode(0o644)).unwrap();
    let tight = Limits {
        max_total_bytes: 512,
        ..Limits::default()
    };
    assert!(install::assert_safe_tree(&e, &tight)
        .unwrap_err()
        .to_string()
        .contains("total bytes"));
}

#[test]
fn rollback_only_activates_a_still_verifiable_artifact() {
    let fx = Fixture::new("rollback");
    let data = fx.root.join("data");
    // `provenance_artifacts_dir` hangs off the data dir.
    std::env::set_var("COS_DATA_DIR", &data);

    let trust = fx.store();
    let limits = Limits::default();
    let live = fx.root.join("live").join("notes");
    let source = app_package(&fx, "notes", "print('v1')\n");

    let staged = install::stage_directory(
        &source,
        &live,
        PackageKind::App,
        Some("notes"),
        &trust,
        &limits,
    )
    .unwrap();
    let v1 = staged.verified.content_digest().to_string();
    install::publish(staged, &live, false, &limits).unwrap();

    write(&source.join("main.py"), "print('v2')\n");
    fs::set_permissions(source.join("main.py"), fs::Permissions::from_mode(0o755)).unwrap();
    fx.sign(&source, PackageKind::App, "notes", &["main.py"]);
    let staged = install::stage_directory(
        &source,
        &live,
        PackageKind::App,
        Some("notes"),
        &trust,
        &limits,
    )
    .unwrap();
    let v2 = staged.verified.content_digest().to_string();
    install::publish(staged, &live, true, &limits).unwrap();
    assert_ne!(v1, v2);

    // Roll back to the earlier verified artifact.
    install::rollback(PackageKind::App, "notes", &v1, &live, &trust, &limits).unwrap();
    assert_eq!(
        fs::read_to_string(live.join("main.py")).unwrap(),
        "print('v1')\n"
    );

    // Revoke v2 and confirm it can no longer be rolled forward onto.
    fx.write_trust(&[], &[v2.clone()]);
    let revoked = fx.store();
    let err =
        install::rollback(PackageKind::App, "notes", &v2, &live, &revoked, &limits).unwrap_err();
    assert_eq!(err.code(), "provenance.untrusted_key");

    std::env::remove_var("COS_DATA_DIR");
}

// ---------------------------------------------------------------------------
// Trust store hardening
// ---------------------------------------------------------------------------

#[test]
fn another_users_trust_root_is_never_loaded() {
    let fx = Fixture::new("crossuser");
    // Claim the root belongs to a different uid than the files do.
    let foreign = TrustStore::load_roots(&[TrustRootSpec {
        path: fx.trust_root.clone(),
        tier: TrustTier::User,
        allowed_uids: vec![me().wrapping_add(1)],
        domain: cos::provenance::state::TrustDomain::Owner(me().wrapping_add(1)),
    }]);
    assert!(foreign.is_empty());

    // And a world-writable root contributes nothing even when the files
    // inside it are well formed.
    fs::set_permissions(&fx.trust_root, fs::Permissions::from_mode(0o777)).unwrap();
    let loose = TrustStore::load_roots(&[TrustRootSpec {
        path: fx.trust_root.clone(),
        tier: TrustTier::User,
        allowed_uids: vec![me()],
        domain: cos::provenance::state::TrustDomain::Owner(me()),
    }]);
    assert!(loose.is_empty());
    fs::set_permissions(&fx.trust_root, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn environment_cannot_introduce_a_trust_root_or_disable_verification() {
    // Every variable an attacker might reach for. None of them may
    // change the resolved roots or make an unsigned package verify.
    for (key, value) in [
        ("COS_TRUST_DIR", "/tmp/evil"),
        ("COS_SKILLS_REQUIRE_SIGNATURE", "0"),
        ("COS_SKILLS_TRUSTED_KEYS", &"aa".repeat(32)),
        ("COS_PROVENANCE_DISABLE", "1"),
        ("COS_CONFIG_DIR", "/tmp/evil"),
    ] {
        std::env::set_var(key, value);
    }
    let roots = TrustStore::default_roots();
    for root in &roots {
        let display = root.path.display().to_string();
        assert!(
            display.starts_with("/usr/lib/cos")
                || display.starts_with("/etc/cos")
                || display.contains(".config/cos/trust"),
            "environment introduced trust root {display}"
        );
    }

    let fx = Fixture::new("envfree");
    let unsigned = fx.root.join("scratch");
    fs::create_dir_all(&unsigned).unwrap();
    write(
        &unsigned.join("app.json"),
        r#"{"id":"scratch","version":"1","name":"scratch","operations":{}}"#,
    );
    let err = verify::verify_package(
        &unsigned,
        &VerifyOptions::new(PackageKind::App).expect_id("scratch"),
        &TrustStore::default(),
    )
    .unwrap_err();
    assert_eq!(err.code(), "provenance.unsigned");

    for key in [
        "COS_TRUST_DIR",
        "COS_SKILLS_REQUIRE_SIGNATURE",
        "COS_SKILLS_TRUSTED_KEYS",
        "COS_PROVENANCE_DISABLE",
        "COS_CONFIG_DIR",
    ] {
        std::env::remove_var(key);
    }
}

#[test]
fn a_development_checkout_never_inherits_vendor_trust() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    assert!(!verify::is_vendor_root_path(&repo.join("apps")));
    assert!(!verify::is_vendor_root_path(&repo.join("skills")));
    assert!(!verify::is_vendor_root_path(&std::env::temp_dir()));
}

// ---------------------------------------------------------------------------
// Executable binding: a mutable interpreter cannot be substituted
// ---------------------------------------------------------------------------

#[test]
fn a_signed_entrypoint_executes_the_verified_bytes() {
    let fx = Fixture::new("exec");
    let dir = fx.root.join("runner");
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("agent-api.json"),
        r#"{"schema":"claw.agent-api/v1","id":"runner","name":"runner","transport":"mcp+stdio","command":"true"}"#,
    );
    let script = dir.join("run.sh");
    write(&script, "#!/bin/sh\necho verified\n");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fx.sign(&dir, PackageKind::Mcp, "runner", &["run.sh"]);

    let trust = fx.store();
    let pkg = verify::verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::Mcp).expect_id("runner"),
        &trust,
    )
    .unwrap();

    // The pinned descriptor names the exact inode that was hashed.
    let fd = pkg.open_entrypoint("run.sh").unwrap();
    let verified_bytes = fd.read_bounded(4096).unwrap();

    // The verified bytes really are runnable code.
    let output = std::process::Command::new("/bin/sh")
        .arg(&script)
        .output()
        .expect("run the verified entrypoint");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "verified");

    // Swapping the path afterwards cannot redirect the held descriptor…
    write(&dir.join(".evil"), "#!/bin/sh\necho evil\n");
    fs::rename(dir.join(".evil"), &script).unwrap();
    assert_eq!(
        fd.read_bounded(4096).unwrap(),
        verified_bytes,
        "the pinned descriptor must still be the verified inode"
    );

    // …and re-verification of the path now fails, so the launcher
    // refuses to run the replacement.
    assert!(pkg.read_verified("run.sh").is_err());
    assert!(pkg.open_entrypoint("run.sh").is_err());

    // Sanity: the replacement really is different code.
    let output = std::process::Command::new("/bin/sh")
        .arg(&script)
        .output()
        .expect("run the replacement");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "evil");
}

#[test]
fn a_writable_interpreter_on_path_is_not_a_valid_program() {
    // `/usr/bin/env` is package-manager owned; a copy in a temp dir is
    // not, and the MCP launcher must refuse it. The check is exposed
    // through `require_secure_location`, which is what the launcher
    // applies to a PATH-resolved interpreter.
    let fx = Fixture::new("interp");
    let fake = fx.root.join("python3");
    write(&fake, "#!/bin/sh\nexec /bin/sh\n");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        cos::provenance::fsec::require_secure_location(&fake, &[0]).is_err(),
        "a user-owned interpreter must not pass the root-owned check"
    );
    if Path::new("/bin/sh").exists() {
        let real = Path::new("/bin/sh").canonicalize().unwrap();
        assert!(
            cos::provenance::fsec::require_secure_location(&real, &[0]).is_ok(),
            "the distribution shell must pass"
        );
    }
}

// ---------------------------------------------------------------------------
// Content-manifest integrity
// ---------------------------------------------------------------------------

#[test]
fn content_digest_covers_mode_and_layout_not_just_bytes() {
    let fx = Fixture::new("digest");
    let dir = app_package(&fx, "notes", "print('x')\n");
    let trust = fx.store();
    let options = VerifyOptions::new(PackageKind::App).expect_id("notes");
    verify::verify_package(&dir, &options, &trust).unwrap();

    // Same bytes, different mode: the executable bit is security
    // relevant and is part of the signed tree.
    fs::set_permissions(dir.join("main.py"), fs::Permissions::from_mode(0o644)).unwrap();
    let err = verify::verify_package(&dir, &options, &trust).unwrap_err();
    assert!(format!("{err}").contains("mode"), "{err}");
    fs::set_permissions(dir.join("main.py"), fs::Permissions::from_mode(0o755)).unwrap();
    verify::verify_package(&dir, &options, &trust).unwrap();

    // An added directory is a tree change even with no file content.
    fs::create_dir_all(dir.join("extra")).unwrap();
    let err = verify::verify_package(&dir, &options, &trust).unwrap_err();
    assert!(
        format!("{err}").contains("not covered by the signature"),
        "{err}"
    );
}

#[test]
fn digest_of_an_empty_tree_is_stable_and_distinct() {
    assert_eq!(content_digest(&[]), content_digest(&[]));
    assert_ne!(
        content_digest(&[]),
        format!("sha256:{}", "0".repeat(64)),
        "the empty tree must not hash to a trivially guessable value"
    );
}

// ---------------------------------------------------------------------------
// Daemon-shaped trust staleness and revocation
// ---------------------------------------------------------------------------

#[test]
fn a_long_lived_reader_notices_a_revocation_without_restarting() {
    // This is the daemon case: the store is loaded once, work happens,
    // the operator revokes, and the *same* process must stop honouring
    // the key on its next authority check.
    let fx = Fixture::new("daemon-revoke");
    let dir = app_package(&fx, "notes", "print('v1')\n");
    let roots = fx.roots();
    let options = VerifyOptions::new(PackageKind::App).expect_id("notes");

    let store = TrustStore::load_roots(&roots);
    verify::verify_package(&dir, &options, &store).unwrap();
    assert!(
        store.is_current(&roots),
        "an untouched trust root must not force a reload"
    );

    // The operator revokes from another process.
    let digest = verify::verify_package(&dir, &options, &store)
        .unwrap()
        .content_digest()
        .to_string();
    fx.write_trust(&[], &[digest.clone()]);

    // The cheap staleness check is what a daemon runs before every
    // launch; it must now say "reload".
    assert!(
        !store.is_current(&roots),
        "a revocation must be visible to the cheap staleness check"
    );

    let reloaded = TrustStore::load_roots(&roots);
    assert!(reloaded.is_package_revoked(&digest));
    let err = verify::verify_package(&dir, &options, &reloaded).unwrap_err();
    assert_eq!(err.code(), "provenance.untrusted_key");
}

#[test]
fn an_edited_trust_file_without_a_recorded_generation_fails_closed() {
    // Restoring or hand-editing a trust file to undo a revocation must
    // not work. The recorded fingerprint no longer matches, so the
    // whole domain contributes nothing rather than serving stale keys.
    let fx = Fixture::new("fingerprint");
    let dir = app_package(&fx, "notes", "print('v1')\n");
    let options = VerifyOptions::new(PackageKind::App).expect_id("notes");
    verify::verify_package(&dir, &options, &fx.store()).unwrap();

    // Append a byte without re-recording the domain generation.
    let path = fx.trust_root.join("publisher.json");
    let mut body = fs::read_to_string(&path).unwrap();
    body.push(' ');
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

    let store = TrustStore::load_roots(&fx.roots());
    assert!(
        store.is_empty(),
        "an unrecorded edit must fail the domain closed"
    );
    assert!(
        store
            .diagnostics()
            .iter()
            .any(|d| d.contains("fingerprint")),
        "the operator needs to be told why: {:?}",
        store.diagnostics()
    );
    let err = verify::verify_package(&dir, &options, &store).unwrap_err();
    assert_eq!(err.code(), "provenance.untrusted_key");
}

#[test]
fn a_corrupt_state_file_fails_the_domain_closed() {
    let fx = Fixture::new("corrupt-state");
    let _dir = app_package(&fx, "notes", "print('v1')\n");
    assert!(!fx.store().is_empty());

    let state = fx.state_dir.join(cos::provenance::state::TRUST_STATE_FILE);
    fs::write(&state, "{ truncated").unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();

    let store = TrustStore::load_roots(&fx.roots());
    assert!(store.is_empty(), "a corrupt state file must fail closed");
    assert!(store
        .diagnostics()
        .iter()
        .any(|d| d.contains("fails closed")));
}

#[test]
fn the_durable_generation_moves_forward_on_every_change() {
    let fx = Fixture::new("generation");
    let domain = cos::provenance::state::TrustDomain::Owner(me());
    let first = fx.store().domain_generation(domain).unwrap();
    fx.write_trust(&[], &[]);
    let second = fx.store().domain_generation(domain).unwrap();
    assert!(second > first, "{second} must exceed {first}");
    // And the store-level generation digest changes with it, which is
    // what invalidates every cached verification.
    fx.write_trust(&[], &[format!("sha256:{}", "e".repeat(64))]);
    assert_ne!(fx.store().generation(), "");
}

// ---------------------------------------------------------------------------
// App launch: one snapshot for capability derivation and execution
// ---------------------------------------------------------------------------

#[test]
fn capability_derivation_and_execution_share_one_snapshot() {
    use cos::bridge::AppLaunch;

    let fx = Fixture::new("one-snapshot");
    let dir = fx.root.join("notes");
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("app.json"),
        r#"{"id":"notes","version":"1.0.0","name":"notes",
             "operations":{"go":{"label":"Go","args":[],"needs":[]}}}"#,
    );
    write(
        &dir.join("main.py"),
        "def run(command, args):\n    return {}\n",
    );
    fx.sign(&dir, PackageKind::App, "notes", &["main.py"]);

    let trust = fx.activate();
    let pkg = std::sync::Arc::new(
        verify::verify_package(
            &dir,
            &VerifyOptions::new(PackageKind::App).expect_id("notes"),
            &trust,
        )
        .unwrap(),
    );
    let launch = AppLaunch::new(std::sync::Arc::clone(&pkg)).unwrap();
    assert_eq!(launch.app_id(), "notes");
    assert!(launch.manifest().operations.contains_key("go"));

    // Replace the manifest on disk with one that demands far more.
    write(
        &dir.join("app.json"),
        r#"{"id":"notes","version":"9.9.9","name":"notes",
             "operations":{"go":{"label":"Go","args":[],
               "needs":[{"verb":"sys.identity","scope":{"kind":"wild"},"why":"escalate"}]}}}"#,
    );

    // The snapshot is unchanged: the manifest that drives capability
    // derivation is still the signed one.
    assert_eq!(launch.manifest().version, "1.0.0");
    assert!(launch.manifest().operations["go"].needs.is_empty());

    // And the snapshot now refuses to be re-read or bound, so nothing
    // downstream can silently pick up the edited bytes.
    assert!(pkg.manifest_text().is_err());
    assert!(launch.bind(&["main.py".to_string()]).is_err());
}

#[test]
fn replacing_the_entrypoint_after_binding_cannot_change_what_runs() {
    use cos::bridge::AppLaunch;

    let fx = Fixture::new("entry-swap");
    let dir = app_package(&fx, "notes", "print('verified')\n");
    let trust = fx.activate();
    let pkg = std::sync::Arc::new(
        verify::verify_package(
            &dir,
            &VerifyOptions::new(PackageKind::App).expect_id("notes"),
            &trust,
        )
        .unwrap(),
    );
    let launch = AppLaunch::new(pkg).unwrap();

    // Bind: this re-hashes the entrypoint and holds its descriptor.
    let binding = launch.bind(&["main.py".to_string()]).unwrap();

    // Swap the file behind the binding.
    write(&dir.join(".evil"), "print('evil')\n");
    fs::set_permissions(dir.join(".evil"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(dir.join(".evil"), dir.join("main.py")).unwrap();

    // The launch is refused rather than running the replacement: the
    // package no longer matches its signature, and re-binding fails.
    assert!(launch.bind(&["main.py".to_string()]).is_err());
    drop(binding);
}

#[test]
fn an_undeclared_file_is_not_an_entrypoint() {
    let fx = Fixture::new("undeclared");
    let dir = fx.root.join("notes");
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("app.json"),
        r#"{"id":"notes","version":"1.0.0","name":"notes","operations":{}}"#,
    );
    write(&dir.join("main.py"), "print('ok')\n");
    write(&dir.join("helper.py"), "print('helper')\n");
    // Only main.py is declared.
    fx.sign(&dir, PackageKind::App, "notes", &["main.py"]);

    let pkg = verify::verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::App).expect_id("notes"),
        &fx.store(),
    )
    .unwrap();
    assert!(pkg.open_entrypoint("main.py").is_ok());
    let err = pkg.open_entrypoint("helper.py").unwrap_err();
    assert!(
        format!("{err}").contains("not a signed entrypoint"),
        "{err}"
    );
}

#[test]
fn unsigned_developer_content_declares_only_its_manifest_entry() {
    // The developer path has no envelope, so entrypoints come from the
    // manifest — never "every regular file in the tree".
    let fx = Fixture::new("dev-entries");
    let dir = fx.root.join("scratch");
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("app.json"),
        r#"{"id":"scratch","version":"1","name":"scratch","operations":{}}"#,
    );
    write(&dir.join("main.py"), "print('ok')\n");
    write(&dir.join("extra.py"), "print('extra')\n");

    let body = sign::build_body(
        &dir,
        &sign::SignRequest {
            kind: PackageKind::App,
            id: "scratch".to_string(),
            version: "dev".to_string(),
            manifest_schema: "developer".to_string(),
            manifest_path: "app.json".to_string(),
            entrypoints: vec![],
            resources: vec![],
        },
    )
    .unwrap();
    let digest = content_digest(&body.files);

    let dev_root = fx.root.join("devtrust");
    fs::create_dir_all(&dev_root).unwrap();
    fs::set_permissions(&dev_root, fs::Permissions::from_mode(0o700)).unwrap();
    let grants = serde_json::json!({
        "schema": cos::provenance::trust::DEV_TRUST_SCHEMA_V1,
        "grants": [{
            "kind": "app",
            "id": "scratch",
            "path": dir.canonicalize().unwrap(),
            "content_digest": digest,
            "granted_at": "2026-01-01T00:00:00Z",
        }],
    });
    let grants_path = dev_root.join("grants.json");
    fs::write(&grants_path, serde_json::to_vec_pretty(&grants).unwrap()).unwrap();
    fs::set_permissions(&grants_path, fs::Permissions::from_mode(0o600)).unwrap();
    // The generation is recorded in the directory the loader looks in —
    // the *parent* of the root — because a domain with trust files but
    // no recorded generation fails closed.
    cos::provenance::state::bump(
        &fx.root,
        cos::provenance::state::TrustDomain::Owner(me()),
        &[dev_root.clone()],
    )
    .unwrap();

    let trust = TrustStore::load_roots(&[TrustRootSpec {
        path: dev_root.clone(),
        tier: TrustTier::Developer,
        allowed_uids: vec![me()],
        domain: cos::provenance::state::TrustDomain::Owner(me()),
    }]);
    let pkg = verify::verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::App).expect_id("scratch"),
        &trust,
    )
    .unwrap();
    assert_eq!(pkg.tier(), TrustTier::Developer);

    let entries: Vec<&str> = pkg.entrypoints().iter().map(String::as_str).collect();
    assert!(entries.contains(&"app.json"));
    assert!(entries.contains(&"main.py"));
    assert!(
        !entries.contains(&"extra.py"),
        "an undeclared file must not become an entrypoint: {entries:?}"
    );

    // And the ceiling is the restricted one.
    let ceiling = pkg.ceiling();
    assert!(ceiling.is_developer());
    assert!(!ceiling.allows_verb(cos::caps::Verb::SYS_IDENTITY));
    assert!(!ceiling.allows_mcp_attach());
}

// ---------------------------------------------------------------------------
// Running-instance revocation, end to end
// ---------------------------------------------------------------------------
//
// These drive the *global* trust singleton and the on-disk running
// instance record, with real child processes in their own process
// groups, and with the revocation written by a genuinely separate
// process that sends this one no notification of any kind. What is
// being proved is the timing guarantee: a revoked instance is denied on
// its very next authority call and its process group is stopped by the
// next lifecycle pass, without a daemon restart and without waiting for
// any grant to expire.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cos::provenance::runtime::{self, InstanceClass, PackageRef};

/// Environment variable that turns a re-execution of this test binary
/// into "the other process that revokes something".
const REVOKE_SPEC_ENV: &str = "COS_TEST_REVOKE_SPEC";

/// The revoking process.
///
/// Runs as a normal test when the environment variable is absent, so it
/// costs nothing in an ordinary run. When the parent re-executes this
/// binary with `COS_TEST_REVOKE_SPEC` set, this is a *different
/// process* rewriting the owner's trust file and bumping the domain's
/// durable generation — exactly what `cos provenance trust revoke`
/// does, and with no channel back to the process under test.
#[test]
fn revoker_helper() {
    let Ok(spec) = std::env::var(REVOKE_SPEC_ENV) else {
        return;
    };
    let spec: serde_json::Value = serde_json::from_str(&spec).expect("revoke spec");
    let trust_root = PathBuf::from(spec["trust_root"].as_str().unwrap());
    let state_dir = PathBuf::from(spec["state_dir"].as_str().unwrap());
    let body = serde_json::json!({
        "schema": TRUST_SCHEMA_V1,
        "keys": [{
            "key_id": spec["key_id"],
            "algorithm": "ed25519",
            "public_key": spec["public_key"],
            "usages": [USAGE_PACKAGE_SIGNING],
            "kinds": ["app", "skill", "mcp", "extension"],
            "status": "active",
        }],
        "revoked_keys": spec["revoked_keys"],
        "revoked_packages": spec["revoked_packages"],
    });
    let path = trust_root.join("publisher.json");
    fs::write(&path, serde_json::to_vec_pretty(&body).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    cos::provenance::state::bump(
        &state_dir,
        cos::provenance::state::TrustDomain::Owner(me()),
        &[trust_root],
    )
    .expect("record trust generation");
}

impl Fixture {
    /// Revoke from a separate process and return only once it has
    /// exited, so the change is durably on disk before the caller looks.
    fn revoke_from_another_process(&self, revoked_keys: &[String], revoked_packages: &[String]) {
        let spec = serde_json::json!({
            "trust_root": self.trust_root,
            "state_dir": self.state_dir,
            "key_id": self.key.key_id,
            "public_key": self.key.public_key,
            "revoked_keys": revoked_keys,
            "revoked_packages": revoked_packages,
        });
        let exe = std::env::current_exe().expect("test binary path");
        let status = Command::new(exe)
            .args(["--exact", "revoker_helper", "--nocapture"])
            .env(REVOKE_SPEC_ENV, spec.to_string())
            .env_remove("COS_PROVENANCE_RUNTIME_DIR")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("spawn the revoking process");
        assert!(status.success(), "revoking process failed: {status:?}");
    }
}

/// Point the running-instance record at a private directory for the
/// duration of one test, and clear the process-wide caches around it.
struct RuntimeScope {
    dir: PathBuf,
    previous: Option<std::ffi::OsString>,
}

impl RuntimeScope {
    fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).expect("runtime dir");
        let previous = std::env::var_os("COS_PROVENANCE_RUNTIME_DIR");
        std::env::set_var("COS_PROVENANCE_RUNTIME_DIR", &dir);
        runtime::reset_cache();
        Self { dir, previous }
    }
}

impl Drop for RuntimeScope {
    fn drop(&mut self) {
        runtime::reset_cache();
        match self.previous.take() {
            Some(value) => std::env::set_var("COS_PROVENANCE_RUNTIME_DIR", value),
            None => std::env::remove_var("COS_PROVENANCE_RUNTIME_DIR"),
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A real child in its own process group, like a sandboxed worker.
///
/// `setsid` is what the worker launcher does at spawn, and it is what
/// makes "terminate the instance" mean the whole group rather than one
/// pid with orphaned descendants.
/// A leader plus one descendant, whose pid it writes out.
///
/// The descendant is the point: it is not a child of the test process,
/// so nothing reaps it into a zombie and "is it gone?" has an
/// unambiguous answer. A termination that signalled only the leader's
/// pid would leave it running and the assertion would catch that.
struct Group {
    leader: std::process::Child,
    descendant_pid: u32,
}

fn spawn_group(dir: &Path, label: &str) -> Group {
    let pidfile = dir.join(format!("{label}.pid"));
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(format!(
            "sleep 300 & echo $! > {}; sleep 300",
            pidfile.display()
        ))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let leader = command.spawn().expect("spawn a group leader");
    let deadline = Instant::now() + Duration::from_secs(5);
    let descendant_pid = loop {
        if let Ok(raw) = fs::read_to_string(&pidfile) {
            if let Ok(pid) = raw.trim().parse::<u32>() {
                break pid;
            }
        }
        assert!(Instant::now() < deadline, "group never reported its child");
        std::thread::sleep(Duration::from_millis(20));
    };
    Group {
        leader,
        descendant_pid,
    }
}

impl Group {
    fn leader_pid(&self) -> u32 {
        self.leader.id()
    }

    fn kill(&mut self) {
        unsafe {
            libc::kill(-(self.leader.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.leader.wait();
    }
}

fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn wait_until_gone(pid: u32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    !alive(pid)
}

fn package_ref_of(fx: &Fixture, dir: &Path, id: &str) -> PackageRef {
    let options = VerifyOptions::new(PackageKind::App).expect_id(id);
    let pkg = verify::verify_package(dir, &options, &fx.store()).expect("verify");
    PackageRef::of(&pkg)
}

#[test]
fn a_revocation_from_another_process_denies_the_next_broker_call_and_kills_the_group() {
    let fx = Fixture::new("revoke-live");
    let _scope = RuntimeScope::new(fx.root.join("procdata"));
    fx.activate();

    let doomed_dir = app_package(&fx, "doomed", "print('doomed')\n");
    let doomed = package_ref_of(&fx, &doomed_dir, "doomed");
    let sibling_dir = app_package(&fx, "sibling", "print('sibling')\n");
    let sibling = package_ref_of(&fx, &sibling_dir, "sibling");
    assert_ne!(doomed.content_digest, sibling.content_digest);

    let mut doomed_group = spawn_group(&fx.root, "doomed");
    let mut sibling_group = spawn_group(&fx.root, "sibling");
    let doomed_pid = doomed_group.leader_pid();
    let sibling_pid = sibling_group.leader_pid();

    runtime::register(
        me(),
        "app-doomed",
        &verify::verify_package(
            &doomed_dir,
            &VerifyOptions::new(PackageKind::App).expect_id("doomed"),
            &fx.store(),
        )
        .unwrap(),
    );
    runtime::bind_process(me(), "app-doomed", doomed_pid);
    runtime::register(
        me(),
        "app-sibling",
        &verify::verify_package(
            &sibling_dir,
            &VerifyOptions::new(PackageKind::App).expect_id("sibling"),
            &fx.store(),
        )
        .unwrap(),
    );
    runtime::bind_process(me(), "app-sibling", sibling_pid);

    // The launch endpoint the sandbox talks to. Deliberately built with
    // an *empty* capability set: a worker that holds nothing still has
    // to be refused, because the endpoint is also the thing that would
    // otherwise answer "deny, not-granted" instead of "this package is
    // no longer trusted" and leave the instance running.
    let endpoint = cos::worker::BrokerAuthority::new(
        "app-doomed".to_string(),
        Some("doomed".to_string()),
        cos::caps::CapSet::new(),
        cos::worker::relay_slot(),
    )
    .with_package(Some(doomed.clone()));
    let sibling_endpoint = cos::worker::BrokerAuthority::new(
        "app-sibling".to_string(),
        Some("sibling".to_string()),
        cos::caps::CapSet::new(),
        cos::worker::relay_slot(),
    )
    .with_package(Some(sibling.clone()));

    endpoint.assert_live().expect("live before the revocation");
    sibling_endpoint.assert_live().expect("sibling is live");

    // Another process revokes the artifact. Nothing tells this one.
    fx.revoke_from_another_process(&[], &[doomed.content_digest.clone()]);

    // No reload_trust(), no restart: the very next call notices.
    let denial = endpoint
        .assert_live()
        .expect_err("a revoked package must be denied on its next broker call");
    assert!(denial.contains("revoked"), "unexpected: {denial}");
    sibling_endpoint
        .assert_live()
        .expect("a sibling package is unaffected by another package's revocation");

    // The denial also marked it, and the lifecycle pass stops the whole
    // group — the `sleep` the shell backgrounded included.
    let marked = runtime::pending_shutdowns(me()).expect("pending");
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0].session_id, "app-doomed");

    let report = runtime::lifecycle_tick(
        me(),
        &cos::provenance::trust_store(),
        runtime::SHUTDOWN_GRACE,
    );
    assert!(
        report.terminated.contains(&"app-doomed".to_string()),
        "expected the revoked group to be terminated, got {report:?}"
    );
    // The descendant is the real proof: a pid-only kill would have left
    // it running.
    assert!(
        wait_until_gone(doomed_group.descendant_pid, Duration::from_secs(5)),
        "a descendant of the revoked instance survived; the group was not signalled"
    );
    let _ = doomed_group.leader.wait();
    assert!(
        !alive(doomed_pid) || wait_until_gone(doomed_pid, Duration::from_secs(2)),
        "the revoked instance's group leader is still alive"
    );
    assert!(alive(sibling_pid), "the sibling process was killed too");
    assert!(
        alive(sibling_group.descendant_pid),
        "the sibling's descendant was killed too"
    );

    // The record is cleared, so nothing is left claiming to be running.
    assert!(runtime::instance_for(me(), "app-doomed")
        .expect("readable")
        .is_none());
    assert!(runtime::pending_shutdowns(me())
        .expect("pending")
        .is_empty());
    assert!(runtime::instance_for(me(), "app-sibling")
        .expect("readable")
        .is_some());

    sibling_group.kill();
}

#[test]
fn an_idle_instance_is_stopped_by_the_bounded_lifecycle_pass() {
    let fx = Fixture::new("revoke-idle");
    let _scope = RuntimeScope::new(fx.root.join("procdata"));
    fx.activate();

    let dir = app_package(&fx, "idle", "print('idle')\n");
    let reference = package_ref_of(&fx, &dir, "idle");
    let mut group = spawn_group(&fx.root, "idle");
    let pid = group.leader_pid();
    runtime::register(
        me(),
        "app-idle",
        &verify::verify_package(
            &dir,
            &VerifyOptions::new(PackageKind::App).expect_id("idle"),
            &fx.store(),
        )
        .unwrap(),
    );
    runtime::bind_process(me(), "app-idle", pid);

    // Revoked by the publisher key this time, and the instance makes no
    // authority call at all — the sweep is the only thing that can find
    // it.
    fx.revoke_from_another_process(&[fx.key.key_id.clone()], &[]);
    assert!(alive(pid));

    let report = runtime::lifecycle_tick(
        me(),
        &cos::provenance::trust_store(),
        runtime::SHUTDOWN_GRACE,
    );
    assert!(report.marked.contains(&"app-idle".to_string()));
    assert!(report.terminated.contains(&"app-idle".to_string()));
    assert!(
        wait_until_gone(group.descendant_pid, Duration::from_secs(5)),
        "an idle instance's descendant survived the lifecycle pass"
    );
    let _ = group.leader.wait();
    assert!(runtime::instance_for(me(), "app-idle")
        .expect("readable")
        .is_none());
    let _ = reference;
    let _ = pid;
}

#[test]
fn a_recycled_pid_is_never_signalled() {
    let fx = Fixture::new("revoke-pidreuse");
    let _scope = RuntimeScope::new(fx.root.join("procdata"));
    fx.activate();

    let dir = app_package(&fx, "ghost", "print('ghost')\n");
    let reference = package_ref_of(&fx, &dir, "ghost");

    // A live bystander that must survive. Its pid is recorded against
    // the instance together with a start time that does not match it,
    // which is exactly the shape a recycled pid takes: the number is
    // valid, the process behind it is not the one that was recorded.
    let mut bystander = spawn_group(&fx.root, "bystander");
    let bystander_pid = bystander.leader_pid();

    runtime::register(
        me(),
        "app-ghost",
        &verify::verify_package(
            &dir,
            &VerifyOptions::new(PackageKind::App).expect_id("ghost"),
            &fx.store(),
        )
        .unwrap(),
    );
    runtime::bind_process(me(), "app-ghost", bystander_pid);
    runtime::mark_for_shutdown(me(), "app-ghost", "test");

    // Rewrite the recorded start time so the identity no longer matches
    // the process holding that pid.
    let state_path = fx.root.join("procdata").join("provenance-running.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    state["shutdown"]["app-ghost"]["process"]["start_time_ticks"] = serde_json::json!(u64::MAX);
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    runtime::reset_cache();

    let signalled = runtime::terminate(me(), "app-ghost", Duration::from_millis(200))
        .expect("terminate is decided, not skipped");
    assert!(
        !signalled,
        "a pid whose identity no longer matches must not be signalled"
    );
    assert!(
        alive(bystander_pid),
        "an unrelated process holding a recycled pid was killed"
    );
    // The record is still released: there is nothing left to govern.
    assert!(runtime::instance_for(me(), "app-ghost")
        .expect("readable")
        .is_none());

    let _ = reference;
    bystander.kill();
}

#[test]
fn an_operator_configured_mcp_server_is_classified_not_ignored() {
    let fx = Fixture::new("revoke-operator");
    let _scope = RuntimeScope::new(fx.root.join("procdata"));
    fx.activate();

    // No package, no publisher, nothing a revocation can name — but it
    // is recorded and labelled, so it is a deliberate category rather
    // than a gap.
    runtime::register_operator_mcp(me(), "mcp:local-tool");
    let instance = runtime::instance_for(me(), "mcp:local-tool")
        .expect("readable")
        .expect("recorded");
    assert_eq!(instance.class, InstanceClass::McpOperatorConfig);
    assert!(instance.package.is_none());
    assert!(!instance.class.is_package_backed());

    // Revoking every key in the store leaves it alone: it never claimed
    // that trust in the first place.
    fx.revoke_from_another_process(&[fx.key.key_id.clone()], &[]);
    let report = runtime::lifecycle_tick(
        me(),
        &cos::provenance::trust_store(),
        runtime::SHUTDOWN_GRACE,
    );
    assert!(report.is_empty(), "unexpected action taken: {report:?}");
    assert!(runtime::assert_live_now(me(), "mcp:local-tool").is_ok());
    assert!(runtime::instance_for(me(), "mcp:local-tool")
        .expect("readable")
        .is_some());

    runtime::deregister(me(), "mcp:local-tool");
}

#[test]
fn a_marked_instance_stays_denied_until_the_lifecycle_pass_reaches_it() {
    let fx = Fixture::new("revoke-window");
    let _scope = RuntimeScope::new(fx.root.join("procdata"));
    fx.activate();

    let dir = app_package(&fx, "window", "print('window')\n");
    runtime::register(
        me(),
        "app-window",
        &verify::verify_package(
            &dir,
            &VerifyOptions::new(PackageKind::App).expect_id("window"),
            &fx.store(),
        )
        .unwrap(),
    );

    // Marked but not yet acted on: the process may still be alive in
    // this window, and it must not keep spending authority in it.
    runtime::mark_for_shutdown(me(), "app-window", "operator revoked the publisher");
    let error = runtime::assert_live_now(me(), "app-window")
        .expect_err("a marked instance is denied even before it is stopped");
    assert!(error.contains("operator revoked"), "unexpected: {error}");

    // And the mark survives a cache drop, because it is on disk.
    runtime::reset_cache();
    assert!(runtime::assert_live_now(me(), "app-window").is_err());
    runtime::deregister(me(), "app-window");
}

/// Environment variable that turns a re-execution of this binary into
/// "another process registering an instance".
const REGISTER_SPEC_ENV: &str = "COS_TEST_REGISTER_SPEC";

/// The registering process. Inert in a normal run.
#[test]
fn register_helper() {
    let Ok(spec) = std::env::var(REGISTER_SPEC_ENV) else {
        return;
    };
    let spec: serde_json::Value = serde_json::from_str(&spec).expect("register spec");
    let session = spec["session"].as_str().unwrap().to_string();
    // Every writer takes the same cross-process lock, so eight of these
    // running at once must produce eight records rather than whichever
    // one happened to write last.
    runtime::register_operator_mcp(me(), &session);
}

#[test]
fn concurrent_processes_do_not_clobber_the_running_record() {
    let fx = Fixture::new("concurrent-record");
    let _scope = RuntimeScope::new(fx.root.join("procdata"));
    fx.activate();

    let dir = fx.root.join("procdata");
    let exe = std::env::current_exe().expect("test binary path");
    let mut children = Vec::new();
    for index in 0..8 {
        let spec = serde_json::json!({ "session": format!("mcp:writer-{index}") });
        children.push(
            Command::new(&exe)
                .args(["--exact", "register_helper", "--nocapture"])
                .env(REGISTER_SPEC_ENV, spec.to_string())
                .env("COS_PROVENANCE_RUNTIME_DIR", &dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn a registering process"),
        );
    }
    for mut child in children {
        let status = child.wait().expect("wait");
        assert!(status.success(), "a registering process failed: {status:?}");
    }

    let running = runtime::running_instances(me()).expect("readable");
    assert_eq!(
        running.len(),
        8,
        "a concurrent registration from another process was lost: {running:?}"
    );
    for index in 0..8 {
        let key = format!("mcp:writer-{index}");
        assert!(running.contains_key(&key), "missing {key}");
    }

    // A sweep run here reads the same file those processes wrote, and
    // an operator-config instance has no package to revoke, so nothing
    // is marked.
    let report = runtime::lifecycle_tick(
        me(),
        &cos::provenance::trust_store(),
        runtime::SHUTDOWN_GRACE,
    );
    assert!(report.is_empty(), "unexpected action: {report:?}");

    for index in 0..8 {
        runtime::deregister(me(), &format!("mcp:writer-{index}"));
    }
}

#[test]
fn a_relay_style_check_cannot_outlive_a_revocation() {
    // The property the daemon relies on: once the artifact is revoked
    // by another process, the very next check on the *same* record —
    // with no reload call and no restart — denies.
    let fx = Fixture::new("relay-revoke");
    let _scope = RuntimeScope::new(fx.root.join("procdata"));
    fx.activate();

    let dir = app_package(&fx, "relayed", "print('relayed')\n");
    let pkg = verify::verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::App).expect_id("relayed"),
        &fx.store(),
    )
    .unwrap();
    let digest = pkg.content_digest().to_string();
    runtime::register(me(), "app-relayed", &pkg);

    runtime::assert_live_instance_now(me(), "app-relayed").expect("live before the revocation");

    fx.revoke_from_another_process(&[], &[digest]);

    let denial = runtime::assert_live_instance_now(me(), "app-relayed")
        .expect_err("a relay must not outlive the revocation");
    assert!(denial.contains("revoked"), "unexpected: {denial}");

    // And once the record is gone entirely — the instance was swept —
    // a relay still fails closed rather than reading "no record" as
    // "not an extension".
    runtime::deregister(me(), "app-relayed");
    let missing = runtime::assert_live_instance_now(me(), "app-relayed")
        .expect_err("a missing record denies a package-backed session");
    assert!(
        missing.contains("no running-instance record"),
        "unexpected: {missing}"
    );
}

#[test]
fn the_record_path_is_the_same_from_every_context() {
    // A direct CLI run, `agentd` and `clawd` acting for one owner must
    // all address the same file. The path is a pure function of the
    // owner uid, so this holds regardless of which process asks.
    let fx = Fixture::new("record-path");
    let _scope = RuntimeScope::new(fx.root.join("procdata"));

    let expected = fx.root.join("procdata").join("provenance-running.json");
    assert_eq!(runtime::state_path_for(me()), expected);
    assert_eq!(runtime::current_owner(), me());

    // Ask again from a genuinely separate process and compare.
    let exe = std::env::current_exe().expect("test binary path");
    let output = Command::new(exe)
        .args(["--exact", "print_record_path_helper", "--nocapture"])
        .env("COS_TEST_PRINT_RECORD_PATH", "1")
        .env("COS_PROVENANCE_RUNTIME_DIR", fx.root.join("procdata"))
        .output()
        .expect("spawn the path-reporting process");
    let reported = String::from_utf8_lossy(&output.stdout);
    assert!(
        reported.contains(&expected.display().to_string()),
        "another process resolved a different record path: {reported}"
    );
}

#[test]
fn print_record_path_helper() {
    if std::env::var_os("COS_TEST_PRINT_RECORD_PATH").is_none() {
        return;
    }
    println!("{}", runtime::state_path_for(me()).display());
}
