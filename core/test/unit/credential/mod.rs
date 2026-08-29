use super::*;

use std::sync::Once;
static PERMS_INIT: Once = Once::new();
fn perms_init() {
    PERMS_INIT.call_once(|| std::env::set_var("COS_PERMS_MODE", "permissive"));
}
use std::sync::atomic::{AtomicU32, Ordering};

static INIT: Once = Once::new();
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// All tests share one COS_CREDENTIALS_DIR (set once). Each test uses unique
/// credential names so there is no cross-test interference.
fn unique_name(prefix: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{prefix}-{n}")
}

fn setup() {
    INIT.call_once(|| {
        let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        // credentials_dir() now reads COS_CREDENTIALS_DIR (per-user
        // store moved out of $COS_DATA_DIR). Tests still set
        // COS_DATA_DIR for other modules that share this dir.
        std::env::set_var("COS_DATA_DIR", &dir);
        std::env::set_var("COS_CREDENTIALS_DIR", dir.join("credentials"));
    });
    std::env::remove_var("COS_SESSION");
}

// ---- SHA-256 ----------------------------------------------------------

#[test]
fn sha256_known_vectors() {
    perms_init();
    // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb924...
    let empty = sha256::hash(b"");
    assert_eq!(
        &empty[..4],
        &[0xe3, 0xb0, 0xc4, 0x42],
        "SHA-256 empty string"
    );

    // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223...
    let abc = sha256::hash(b"abc");
    assert_eq!(&abc[..4], &[0xba, 0x78, 0x16, 0xbf], "SHA-256 of 'abc'");
}

// ---- AES-256-GCM ------------------------------------------------------

#[test]
fn aes_gcm_encrypt_decrypt_roundtrip() {
    perms_init();
    let key = sha256::hash(b"test-key-for-aes-gcm");
    let nonce = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let plaintext = b"hello, AES-256-GCM world!";

    let ct = aes_gcm::encrypt(&key, &nonce, plaintext);
    // ct should be plaintext.len() + 16 (tag) bytes
    assert_eq!(ct.len(), plaintext.len() + 16);

    let pt = aes_gcm::decrypt(&key, &nonce, &ct).unwrap();
    assert_eq!(pt, plaintext);
}

#[test]
fn aes_gcm_tampered_ciphertext_fails() {
    perms_init();
    let key = sha256::hash(b"test-key-tamper");
    let nonce = [0u8; 12];
    let ct = aes_gcm::encrypt(&key, &nonce, b"secret");

    let mut tampered = ct.clone();
    tampered[0] ^= 0xff;
    assert!(aes_gcm::decrypt(&key, &nonce, &tampered).is_err());
}

#[test]
fn aes_gcm_empty_plaintext() {
    perms_init();
    let key = sha256::hash(b"empty-test");
    let nonce = [42u8; 12];
    let ct = aes_gcm::encrypt(&key, &nonce, b"");
    assert_eq!(ct.len(), 16); // tag only
    let pt = aes_gcm::decrypt(&key, &nonce, &ct).unwrap();
    assert!(pt.is_empty());
}

#[test]
fn aes_256_gcm_nist_vector_is_byte_compatible() {
    let key = [0u8; 32];
    let nonce = [0u8; 12];
    let plaintext = [0u8; 16];
    let expected = [
        0xce, 0xa7, 0x40, 0x3d, 0x4d, 0x60, 0x6b, 0x6e, 0x07, 0x4e, 0xc5, 0xd3, 0xba, 0xf3, 0x9d,
        0x18, 0xd0, 0xd1, 0xc8, 0xa7, 0x99, 0x99, 0x6b, 0xf0, 0x26, 0x5b, 0x98, 0xb5, 0xd4, 0x8a,
        0xb9, 0x19,
    ];

    let encrypted = aes_gcm::encrypt(&key, &nonce, &plaintext);
    assert_eq!(encrypted, expected);
    assert_eq!(
        aes_gcm::decrypt(&key, &nonce, &expected).unwrap(),
        plaintext
    );
}

// ---- Base64 -----------------------------------------------------------

#[test]
fn b64_roundtrip() {
    perms_init();
    let data = b"hello world 12345!@#$%";
    let encoded = to_b64(data);
    let decoded = from_b64(&encoded).unwrap();
    assert_eq!(decoded, data);
}

// ---- Legacy XOR backward compatibility --------------------------------

#[test]
fn legacy_xor_backward_compat() {
    perms_init();
    setup();
    let name = unique_name("legacy-xor");
    let namespace = "default";
    let plain = "legacy-secret-value";

    // Manually create a legacy-format credential (no nonce_b64, XOR-obfuscated).
    let key = legacy_obfuscation_key().unwrap();
    let obfuscated: Vec<u8> = plain
        .as_bytes()
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % key.len()])
        .collect();
    let value_b64 = to_b64(&obfuscated);

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let cred = StoredCredential {
        name: name.clone(),
        namespace: namespace.into(),
        value_b64,
        nonce_b64: None, // legacy — no nonce
        min_tier: 1,
        stored_at: now,
        stored_by: None,
        expires_at: None,
        refresh_cmd: None,
    };

    let dir = namespace_dir(namespace);
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.json"));
    let data = serde_json::to_string_pretty(&cred).unwrap();
    fs::write(&path, data).unwrap();

    // Load it through the normal path — should still work.
    let r = cmd_load(&[name.clone()]).unwrap();
    assert_eq!(r["value"], plain);
}

// ---- Store and load ---------------------------------------------------

#[test]
fn store_and_load() {
    perms_init();
    setup();
    let name = unique_name("store-load");

    let r = cmd_store(&[
        name.clone(),
        "secret-value-123".into(),
        "--tier".into(),
        "1".into(),
    ])
    .unwrap();
    assert_eq!(r["stored"], name);
    assert_eq!(r["min_tier"], 1);
    assert_eq!(r["namespace"], "default");

    let r = cmd_load(&[name.clone()]).unwrap();
    assert_eq!(r["name"], name);
    assert_eq!(r["value"], "secret-value-123");
}

// ---- Revoke -----------------------------------------------------------

#[test]
fn revoke_removes_credential() {
    perms_init();
    setup();
    let name = unique_name("revoke");

    cmd_store(&[name.clone(), "temp-value".into()]).unwrap();
    let r = cmd_revoke(&[name.clone()]).unwrap();
    assert_eq!(r["revoked"], name);

    let r = cmd_load(&[name.clone()]);
    assert!(r.is_err());
}

// ---- List (namespace) -------------------------------------------------

#[test]
fn list_shows_names_only() {
    perms_init();
    setup();
    let a = unique_name("list-a");
    let b = unique_name("list-b");

    cmd_store(&[a.clone(), "val-a".into()]).unwrap();
    cmd_store(&[b.clone(), "val-b".into()]).unwrap();

    let r = cmd_list(&["--namespace".into(), "default".into()]).unwrap();
    assert!(r["count"].as_u64().unwrap() >= 2);
    let creds = r["credentials"].as_array().unwrap();
    for c in creds {
        assert!(c.get("value").is_none(), "values must not appear in list");
        assert!(c["name"].is_string());
    }
}

#[test]
fn list_all_namespaces() {
    perms_init();
    setup();
    let name = unique_name("ns-list");
    cmd_store(&[
        name.clone(),
        "val".into(),
        "--namespace".into(),
        "alpha".into(),
    ])
    .unwrap();

    let r = cmd_list(&[]).unwrap();
    let nss = r["namespaces"].as_array().unwrap();
    let names: Vec<&str> = nss.iter().filter_map(|n| n["namespace"].as_str()).collect();
    assert!(names.contains(&"alpha"), "alpha namespace should be listed");
}

// ---- Validation -------------------------------------------------------

#[test]
fn store_invalid_name() {
    perms_init();
    setup();
    let r = cmd_store(&["bad/name".into(), "val".into()]);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("alphanumeric"));
}

#[test]
fn load_nonexistent() {
    perms_init();
    setup();
    let name = unique_name("nonexistent");
    let r = cmd_load(&[name]);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("not found"));
}

// ---- Namespace isolation ----------------------------------------------

#[test]
fn namespace_isolation() {
    perms_init();
    setup();
    let name = unique_name("ns-iso");

    // Store in namespace A
    cmd_store(&[
        name.clone(),
        "value-a".into(),
        "--namespace".into(),
        "ns-a".into(),
    ])
    .unwrap();

    // Store same name in namespace B with different value
    cmd_store(&[
        name.clone(),
        "value-b".into(),
        "--namespace".into(),
        "ns-b".into(),
    ])
    .unwrap();

    let ra = cmd_load(&[name.clone(), "--namespace".into(), "ns-a".into()]).unwrap();
    let rb = cmd_load(&[name.clone(), "--namespace".into(), "ns-b".into()]).unwrap();
    assert_eq!(ra["value"], "value-a");
    assert_eq!(rb["value"], "value-b");
}

// ---- TTL / expiry -----------------------------------------------------

#[test]
fn ttl_expired_credential() {
    perms_init();
    setup();
    let name = unique_name("ttl-exp");

    // Store with TTL = 0 (expires immediately)
    // We achieve "already expired" by writing directly with a past expires_at.
    let (value_b64, nonce_b64) = encrypt_value(b"will-expire").unwrap();
    let cred = StoredCredential {
        name: name.clone(),
        namespace: "default".into(),
        value_b64,
        nonce_b64: Some(nonce_b64),
        min_tier: 1,
        stored_at: "2020-01-01T00:00:00Z".into(),
        stored_by: None,
        expires_at: Some("2020-01-01T00:00:01Z".into()), // already past
        refresh_cmd: None,
    };
    let dir = namespace_dir("default");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.json"));
    fs::write(&path, serde_json::to_string_pretty(&cred).unwrap()).unwrap();

    let r = cmd_load(&[name.clone()]);
    assert!(r.is_err());
    let err = r.unwrap_err();
    assert!(
        err.contains("expired"),
        "error should mention expiry: {err}"
    );
}

#[test]
fn ttl_not_expired_credential() {
    perms_init();
    setup();
    let name = unique_name("ttl-ok");

    // Store with large TTL — should still be valid.
    cmd_store(&[
        name.clone(),
        "still-valid".into(),
        "--ttl".into(),
        "86400".into(), // 24 hours
    ])
    .unwrap();

    let r = cmd_load(&[name.clone()]).unwrap();
    assert_eq!(r["value"], "still-valid");
}

#[test]
fn list_shows_expiry() {
    perms_init();
    setup();
    let name = unique_name("list-exp");

    cmd_store(&[name.clone(), "v".into(), "--ttl".into(), "3600".into()]).unwrap();

    let r = cmd_list(&["--namespace".into(), "default".into()]).unwrap();
    let creds = r["credentials"].as_array().unwrap();
    let found = creds.iter().find(|c| c["name"].as_str() == Some(&name));
    assert!(found.is_some(), "credential should appear in list");
    let c = found.unwrap();
    assert!(c["expires_at"].is_string());
    assert_eq!(c["expired"], false);
}

// ---- Bundles ----------------------------------------------------------

#[test]
fn bundle_create_and_load() {
    perms_init();
    setup();
    let k1 = unique_name("bk1");
    let k2 = unique_name("bk2");
    let bundle = unique_name("bundle");

    cmd_store(&[k1.clone(), "val1".into()]).unwrap();
    cmd_store(&[k2.clone(), "val2".into()]).unwrap();

    let r = cmd_bundle(&[bundle.clone(), "--keys".into(), format!("{k1},{k2}")]).unwrap();
    assert_eq!(r["bundle"], bundle.as_str());

    let r = cmd_load_bundle(&[bundle.clone()]).unwrap();
    assert_eq!(r["credentials"][&k1], "val1");
    assert_eq!(r["credentials"][&k2], "val2");
    assert!(r.get("errors").is_none());
}

#[test]
fn bundle_with_missing_key() {
    perms_init();
    setup();
    let k1 = unique_name("bkm1");
    let missing = unique_name("bkm-missing");
    let bundle = unique_name("bundle-miss");

    cmd_store(&[k1.clone(), "present".into()]).unwrap();

    cmd_bundle(&[bundle.clone(), "--keys".into(), format!("{k1},{missing}")]).unwrap();

    let r = cmd_load_bundle(&[bundle.clone()]).unwrap();
    assert_eq!(r["credentials"][&k1], "present");
    assert!(
        r["errors"][&missing].is_string(),
        "missing key should have an error"
    );
}

// ---- Dispatch ---------------------------------------------------------

#[test]
fn run_dispatch() {
    perms_init();
    setup();
    let name = unique_name("dispatch");

    let r = run("store", &[name.clone(), "val".into()]).unwrap();
    assert_eq!(r["stored"], name);

    let r = run("list", &["--namespace".into(), "default".into()]).unwrap();
    assert!(r["count"].as_u64().unwrap() >= 1);

    let r = run("bogus", &[]);
    assert!(r.is_err());
}

#[test]
fn run_dispatch_bundle_commands() {
    perms_init();
    setup();
    let k = unique_name("dispk");
    let b = unique_name("dispb");

    run("store", &[k.clone(), "v".into()]).unwrap();
    run("bundle", &[b.clone(), "--keys".into(), k.clone()]).unwrap();
    let r = run("load-bundle", &[b.clone()]).unwrap();
    assert_eq!(r["credentials"][&k], "v");
}

// ---- Auto-refresh -----------------------------------------------------

#[test]
fn store_with_refresh_cmd() {
    perms_init();
    setup();
    let name = unique_name("refresh-store");
    let r = cmd_store(&[
        name.clone(),
        "initial-value".into(),
        "--ttl".into(),
        "3600".into(),
        "--refresh-cmd".into(),
        "cos credential load test".into(),
    ])
    .unwrap();
    assert_eq!(r["stored"], name);

    // Verify refresh_cmd is stored
    let path = namespace_dir("default").join(format!("{name}.json"));
    let data = fs::read_to_string(&path).unwrap();
    let cred: StoredCredential = serde_json::from_str(&data).unwrap();
    assert_eq!(
        cred.refresh_cmd.as_deref(),
        Some("cos credential load test")
    );
}

#[test]
fn store_rejects_non_cos_refresh_cmd() {
    perms_init();
    setup();
    let name = unique_name("bad-refresh");
    let r = cmd_store(&[
        name.clone(),
        "value".into(),
        "--refresh-cmd".into(),
        "echo evil".into(),
    ]);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("must be a cos command"));
}

#[test]
fn execute_refresh_rejects_non_cos() {
    perms_init();
    let r = execute_refresh("rm -rf /");
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("must be a cos command"));
}

#[test]
fn load_auto_refresh_on_expiry() {
    perms_init();
    setup();
    let name = unique_name("auto-refresh");

    // Store with expired TTL and a cos refresh command that will fail.
    // We write the credential file directly to bypass store validation.
    let dir = namespace_dir("default");
    let _ = fs::create_dir_all(&dir);
    let (value_b64, nonce_b64) = encrypt_value(b"old-value").unwrap();
    let now = chrono::Utc::now();
    let stored_at = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    // Use a fixed timestamp far in the past to guarantee expiry
    let expires_at = Some("2020-01-01T00:00:00Z".to_string());

    let cred = StoredCredential {
        name: name.clone(),
        namespace: "default".into(),
        value_b64,
        nonce_b64: Some(nonce_b64),
        min_tier: 0,
        stored_at,
        stored_by: None,
        expires_at,
        // Use a cos command with nonexistent subcommand that will exit non-zero.
        // "cos nonexistent-subcommand-xyz" will fail because the subcommand is unknown.
        refresh_cmd: Some("cos nonexistent-subcommand-xyz".into()),
    };
    let path = dir.join(format!("{name}.json"));
    let data = serde_json::to_string_pretty(&cred).unwrap();
    fs::write(&path, data).unwrap();

    // Load should attempt auto-refresh but fail
    let r = cmd_load(&[name.clone()]);
    // The cos binary may or may not be in PATH, but either way the refresh should fail:
    // - If cos is not in PATH: "failed to execute refresh command"
    // - If cos is in PATH but exits non-zero: "refresh command failed"
    // - If cos is in PATH and exits 0 with error JSON: we get a JSON string as value
    //   (which is still technically valid but the credential gets refreshed with error JSON)
    // In any case, we verify the refresh path is exercised by checking the result.
    // If it succeeds, the value will be error JSON from cos, not "old-value".
    match r {
        Err(e) => {
            assert!(
                e.contains("auto-refresh failed") || e.contains("failed to execute"),
                "unexpected error: {e}"
            );
        }
        Ok(v) => {
            // Refresh "succeeded" with error JSON from cos — value is not "old-value"
            assert_eq!(v["refreshed"], true);
            assert_ne!(v["value"], "old-value");
        }
    }
}

#[test]
fn load_expired_no_refresh_cmd_fails() {
    perms_init();
    setup();
    let name = unique_name("no-refresh");
    cmd_store(&[name.clone(), "val".into(), "--ttl".into(), "0".into()]).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(100));

    let r = cmd_load(&[name.clone()]);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("expired"));
}

// ---- URL encoding -----------------------------------------------------

#[test]
fn urlencoded_special_chars() {
    perms_init();
    assert_eq!(urlencoded("hello world"), "hello%20world");
    assert_eq!(urlencoded("a+b=c&d"), "a%2Bb%3Dc%26d");
    assert_eq!(urlencoded("simple"), "simple");
}

// ---- TTL computation --------------------------------------------------

#[test]
fn compute_ttl_from_timestamps() {
    perms_init();
    let cred = StoredCredential {
        name: "test".into(),
        namespace: "default".into(),
        value_b64: String::new(),
        nonce_b64: None,
        min_tier: 0,
        stored_at: "2026-03-25T10:00:00Z".into(),
        stored_by: None,
        expires_at: Some("2026-03-25T11:00:00Z".into()),
        refresh_cmd: None,
    };
    let ttl = compute_original_ttl(&cred);
    assert_eq!(ttl, Some(3600));
}

// ---- OAuth dispatch ---------------------------------------------------

#[test]
fn oauth_refresh_unknown_provider() {
    perms_init();
    setup();
    let r = cmd_oauth_refresh(&FILE_STORE, &["unknown".into()]);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("unsupported"));
}

#[test]
fn oauth_refresh_missing_provider() {
    perms_init();
    setup();
    let r = cmd_oauth_refresh(&FILE_STORE, &[]);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("usage"));
}

#[test]
fn broker_oauth_refresh_accepts_only_matching_builtin_command() {
    assert_eq!(
        broker_oauth_provider(
            "cos credential oauth-refresh google --namespace default",
            "default"
        ),
        Some("google")
    );
    assert_eq!(
        broker_oauth_provider(
            "cos credential oauth-refresh microsoft --namespace other",
            "default"
        ),
        None
    );
    assert_eq!(
        broker_oauth_provider("sh -c 'steal secrets'", "default"),
        None
    );
}

// ---- Persistent root key (CRITICAL audit fix) -------------------------

/// Verifies the CRITICAL fix: when `/etc/machine-id` is unreadable AND no
/// `credential-root.key` exists yet, the store must generate a random
/// 32-byte key via the OS CSPRNG, persist it with mode 0600, and reuse it
/// on subsequent calls. The previous behaviour was to silently fall back
/// to `sha256("claw-os-credential-store-key-v1")` — a universally-known
/// key that decrypts every credential store offline.
#[test]
fn test_machine_id_missing_falls_back_to_persistent_random_key() {
    perms_init();
    setup();

    // Use a per-test scratch dir + the `*_at` helpers so this test does
    // NOT mutate any process-global env vars (which would race the other
    // ~80 credential tests).
    let dir = std::env::temp_dir().join(format!(
        "cos-cred-rootkey-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::create_dir_all(&dir);
    let key_path = dir.join("credential-root.key");
    let _ = fs::remove_file(&key_path);

    // 1. No persistent key yet.
    assert!(
        load_persistent_root_key_at(&key_path).unwrap().is_none(),
        "key file must not exist before first generate call"
    );

    // 2. Generate + persist (this is what `derive_key` calls when neither
    //    the keyring nor machine-id are available).
    let key1 = generate_and_persist_root_key_at(&key_path).unwrap();
    assert_eq!(key1.len(), 32, "root key must be exactly 32 bytes");
    assert!(
        key1.iter().any(|&b| b != 0),
        "generated key must not be all zeros (CSPRNG sanity)"
    );

    // 3. The on-disk file matches: 32 bytes exactly, mode 0o600.
    let bytes = fs::read(&key_path).expect("key file must exist");
    assert_eq!(bytes.len(), 32, "persisted key must be 32 bytes");
    assert_eq!(bytes, &key1[..], "on-disk bytes must equal returned key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(&key_path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "persisted key must be mode 0600 from creation (no chmod race)"
        );
    }

    // 4. Second call returns the SAME key from disk.
    let key2 = load_persistent_root_key_at(&key_path)
        .unwrap()
        .expect("key should load after generate");
    assert_eq!(key1, key2, "persisted key must round-trip");

    // 5. A redundant `generate_and_persist_root_key_at()` call (e.g. from
    //    a racing process) MUST NOT overwrite the existing key — it must
    //    fall back to reading whatever is already there.
    let key3 = generate_and_persist_root_key_at(&key_path).unwrap();
    assert_eq!(
        key1, key3,
        "repeated generate must not overwrite an existing on-disk key"
    );

    let _ = fs::remove_file(&key_path);
    let _ = fs::remove_dir(&dir);
}

#[test]
fn malformed_persistent_root_key_is_not_silently_replaced() {
    let dir = std::env::temp_dir().join(format!(
        "cos-cred-malformed-rootkey-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    let key_path = dir.join("credential-root.key");
    fs::write(&key_path, b"too-short").unwrap();

    let error = load_persistent_root_key_at(&key_path).unwrap_err();
    assert!(error.to_string().contains("invalid length"));
    assert_eq!(fs::read(&key_path).unwrap(), b"too-short");

    fs::remove_file(&key_path).unwrap();
    fs::remove_dir(&dir).unwrap();
}

#[test]
fn random_failure_is_typed_and_preserves_its_source() {
    let dir = std::env::temp_dir().join(format!(
        "cos-cred-random-failure-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let error = inject_root_key_random_failure(&dir.join("credential-root.key"));

    assert_eq!(error.kind(), CredentialErrorKind::Unavailable);
    assert_eq!(error.operation(), "root_key.random");
    assert!(std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .is_some());
    assert!(!dir.join("credential-root.key").exists());

    fs::remove_dir(&dir).unwrap();
}

#[test]
fn root_key_write_failure_leaves_no_final_or_temporary_file() {
    let dir = std::env::temp_dir().join(format!(
        "cos-cred-rootkey-write-failure-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    let key_path = dir.join("credential-root.key");

    let error = inject_root_key_write_failure(&key_path);

    assert_eq!(error.kind(), CredentialErrorKind::Unavailable);
    assert!(!key_path.exists());
    let leftovers = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "temporary files remained: {leftovers:?}"
    );
    fs::remove_dir(dir).unwrap();
}

#[test]
fn concurrent_root_key_publishers_only_observe_a_complete_winner() {
    let dir = std::env::temp_dir().join(format!(
        "cos-cred-rootkey-race-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    let key_path = dir.join("credential-root.key");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let path = key_path.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            generate_root_key_at_barrier(&path, &barrier).unwrap()
        }));
    }
    let first = threads.remove(0).join().unwrap();
    let second = threads.remove(0).join().unwrap();

    assert_eq!(first, second);
    assert_eq!(fs::read(&key_path).unwrap(), first);
    let leftovers = fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path() != key_path)
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "temporary files remained");

    fs::remove_file(key_path).unwrap();
    fs::remove_dir(dir).unwrap();
}

#[test]
fn typed_load_preserves_corrupt_record_category_and_serde_source() {
    perms_init();
    setup();
    let name = unique_name("typed-corrupt");
    let path = namespace_dir("default").join(format!("{name}.json"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{not valid credential json").unwrap();

    let error = try_load_typed(&name, "default").unwrap_err();

    assert_eq!(error.kind(), CredentialErrorKind::Corrupt);
    assert_eq!(error.operation(), "credential.load");
    assert!(std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<serde_json::Error>())
        .is_some());
    fs::remove_file(path).unwrap();
}

#[test]
fn typed_command_classifies_invalid_input_and_not_found() {
    perms_init();
    setup();

    let invalid = run_typed("store", &["bad/name".into(), "value".into()]).unwrap_err();
    assert_eq!(invalid.kind(), CredentialErrorKind::InvalidInput);

    let missing = run_typed("load", &[unique_name("typed-missing")]).unwrap_err();
    assert_eq!(missing.kind(), CredentialErrorKind::NotFound);
}

#[test]
fn credential_error_debug_and_external_display_redact_token_like_values() {
    const SECRET: &str = "sk-this-is-a-long-secret-token-value";
    let error = CredentialError::external(
        "credential.test",
        format!("provider rejected token {SECRET}"),
    );

    assert!(!error.to_string().contains(SECRET));
    assert!(!format!("{error:?}").contains(SECRET));
    assert!(error.to_string().contains("***"));
}

#[cfg(target_os = "linux")]
#[test]
fn keyring_failure_is_typed_and_preserves_its_source() {
    let error = inject_keyring_failure();

    assert_eq!(error.kind(), CredentialErrorKind::Unavailable);
    assert_eq!(error.operation(), "keyring.read");
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn encrypted_record_json_shape_is_stable() {
    let credential = StoredCredential {
        name: "API_KEY".into(),
        namespace: "default".into(),
        value_b64: "AQID".into(),
        nonce_b64: Some("BAUG".into()),
        min_tier: 2,
        stored_at: "2026-01-02T03:04:05Z".into(),
        stored_by: Some("session-1".into()),
        expires_at: Some("2026-01-02T04:04:05Z".into()),
        refresh_cmd: None,
    };

    assert_eq!(
        serde_json::to_string_pretty(&credential).unwrap(),
        "{\n  \"name\": \"API_KEY\",\n  \"namespace\": \"default\",\n  \"value_b64\": \"AQID\",\n  \"nonce_b64\": \"BAUG\",\n  \"min_tier\": 2,\n  \"stored_at\": \"2026-01-02T03:04:05Z\",\n  \"stored_by\": \"session-1\",\n  \"expires_at\": \"2026-01-02T04:04:05Z\",\n  \"refresh_cmd\": null\n}"
    );
}

#[test]
fn keyring_master_key_label_is_stable() {
    assert_eq!(MASTER_KEY_LABEL, b"cos-credential-key");
}

#[cfg(unix)]
#[test]
fn atomic_write_replaces_content_with_mode_0600() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!(
        "cos-cred-atomic-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("record.json");

    write_credential_atomic(&path, "first").unwrap();
    write_credential_atomic(&path, "second").unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!path.with_extension("tmp").exists());

    fs::remove_file(&path).unwrap();
    let _ = fs::remove_file(path.with_file_name("record.json.lock"));
    fs::remove_dir(&dir).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn scheduled_load_rejects_symlinked_credential() {
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::MetadataExt;

    let root = std::env::temp_dir().join(format!(
        "cos-cred-symlink-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let home = root.join("home");
    let credential_dir = home.join(".local/share/cos/credentials/default");
    fs::create_dir_all(&credential_dir).unwrap();
    let outside = root.join("outside.json");
    fs::write(&outside, "{}").unwrap();
    let link = credential_dir.join("API_KEY.json");
    symlink(&outside, &link).unwrap();
    let uid = fs::metadata(&home).unwrap().uid();

    let error = load_for_scheduler("API_KEY", "default", &home, uid, 0).unwrap_err();
    assert!(error.contains("failed to open scheduled credential"));

    fs::remove_file(&link).unwrap();
    fs::remove_file(&outside).unwrap();
    fs::remove_dir_all(&root).unwrap();
}

// ---- Refresh serialization (HIGH audit fix) ---------------------------

/// Verifies the HIGH fix: concurrent auto-refresh attempts on the same
/// credential must be serialized via a per-credential flock so that two
/// callers cannot both call the OAuth endpoint and cannibalise each
/// other's rotated refresh token.
///
/// We exercise the primitive (`with_refresh_lock`) directly with N
/// threads, asserting that the maximum observed concurrency inside the
/// critical section is exactly 1.
#[test]
fn test_concurrent_refresh_serialized() {
    perms_init();
    setup();

    let name = unique_name("refresh-serialized");
    let path = namespace_dir("default").join(format!("{name}.json"));
    let _ = fs::create_dir_all(path.parent().unwrap());
    let _ = fs::remove_file(refresh_sentinel_path(&path));

    use std::sync::atomic::AtomicI64;
    use std::sync::Arc;
    let in_flight = Arc::new(AtomicI64::new(0));
    let max_in_flight = Arc::new(AtomicI64::new(0));
    let calls = Arc::new(AtomicU32::new(0));

    let n_threads = 8usize;
    let mut handles = Vec::with_capacity(n_threads);
    for _ in 0..n_threads {
        let in_flight = in_flight.clone();
        let max_in_flight = max_in_flight.clone();
        let calls = calls.clone();
        let p = path.clone();
        handles.push(std::thread::spawn(move || {
            with_refresh_lock(&p, || {
                let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                let mut prev = max_in_flight.load(Ordering::SeqCst);
                while cur > prev {
                    match max_in_flight.compare_exchange(
                        prev,
                        cur,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(_) => break,
                        Err(p) => prev = p,
                    }
                }
                calls.fetch_add(1, Ordering::SeqCst);
                // Hold the critical section long enough that any race
                // would surface as concurrent observed > 1.
                std::thread::sleep(std::time::Duration::from_millis(30));
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok::<(), CredentialError>(())
            })
            .unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        n_threads as u32,
        "every thread must enter the critical section exactly once"
    );
    assert_eq!(
        max_in_flight.load(Ordering::SeqCst),
        1,
        "with_refresh_lock must serialize: max concurrent must be 1"
    );

    let _ = fs::remove_file(refresh_sentinel_path(&path));
}

// ---- OAuth argv leak (HIGH audit fix) ---------------------------------

/// Verifies the HIGH fix: the curl command used for OAuth refresh must
/// NOT receive the request body (containing `client_secret` and
/// `refresh_token`) on its argv, where any same-uid process could read it
/// from `/proc/<pid>/cmdline`. The body must come from stdin via
/// `--data-binary @-`.
#[test]
fn test_oauth_refresh_no_argv_leak() {
    perms_init();
    let secret = "super-secret-refresh-token-DO-NOT-LEAK-ME";
    // The builder, by design, accepts only URL + content type — there is
    // no parameter through which the body could reach argv. We verify
    // both the design (no body parameter) and the observable contract
    // (no -d / --data, body via stdin only).
    let cmd = build_curl_post(
        "https://oauth2.googleapis.com/token",
        "application/x-www-form-urlencoded",
    );
    let argv: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let argv_joined = argv.join(" ");

    assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/usr/bin/curl"));
    assert!(
        !argv_joined.contains(secret),
        "argv must never contain a secret; argv = {argv_joined}"
    );
    assert!(
        !argv.iter().any(|a| a == "-d"),
        "argv must not use `-d` (puts body in argv); argv = {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--data"),
        "argv must not use `--data` (puts body in argv); argv = {argv:?}"
    );
    let stdin_pair = argv.windows(2).find(|w| w[0] == "--data-binary");
    assert_eq!(
        stdin_pair.map(|w| w[1].as_str()),
        Some("@-"),
        "argv must use `--data-binary @-` (body read from stdin); argv = {argv:?}"
    );
}

// ---- Debug masking (MEDIUM audit fix) --------------------------------

/// Verifies the MEDIUM fix: Debug / Display impls for `StoredCredential`
/// must never echo the encrypted blob or its nonce, so an accidental
/// `tracing::debug!(?cred)` cannot regress into leaking credential
/// material through log sinks.
#[test]
fn stored_credential_debug_display_masks_secret() {
    perms_init();
    let cred = StoredCredential {
        name: "api-key".into(),
        namespace: "default".into(),
        value_b64: "VERY-SECRET-CIPHERTEXT-NEVER-LOG-ME".into(),
        nonce_b64: Some("very-secret-nonce".into()),
        min_tier: 0,
        stored_at: "2026-01-01T00:00:00Z".into(),
        stored_by: None,
        expires_at: None,
        refresh_cmd: None,
    };
    let dbg = format!("{cred:?}");
    assert!(
        !dbg.contains("VERY-SECRET-CIPHERTEXT"),
        "Debug must mask value_b64: {dbg}"
    );
    assert!(
        !dbg.contains("very-secret-nonce"),
        "Debug must mask nonce_b64: {dbg}"
    );
    assert!(dbg.contains("***"), "Debug must show masking marker: {dbg}");
    let disp = format!("{cred}");
    assert!(
        !disp.contains("VERY-SECRET-CIPHERTEXT"),
        "Display must not echo ciphertext: {disp}"
    );
    assert_eq!(disp, "credential(default/api-key)");
}

// ---- Tier comparison semantics (HIGH audit fix) ----------------------

/// Pins the semantic of the renamed tier comparator. Lower number ==
/// more privileged. ROOT (0) accesses everything; SANDBOX (3) accesses
/// only what's explicitly tier-3-allowed.
#[test]
fn tier_grants_access_semantics_pinned() {
    perms_init();
    // Same-tier always grants.
    assert!(tier_grants_access(0, 0));
    assert!(tier_grants_access(1, 1));
    assert!(tier_grants_access(2, 2));
    assert!(tier_grants_access(3, 3));
    // Stronger session (lower number) accesses weaker-tier creds.
    assert!(tier_grants_access(0, 1));
    assert!(tier_grants_access(0, 3));
    assert!(tier_grants_access(2, 3));
    // Weaker session (higher number) CANNOT access stronger-tier creds.
    assert!(!tier_grants_access(1, 0));
    assert!(!tier_grants_access(2, 0));
    assert!(!tier_grants_access(3, 0));
    assert!(!tier_grants_access(3, 2));
    // u8::MAX (fail-closed "weakest possible") accesses nothing
    // except a (non-existent) u8::MAX cred.
    assert!(!tier_grants_access(u8::MAX, 0));
    assert!(!tier_grants_access(u8::MAX, 3));
}

// ---- from_b64 strictness (LOW audit fix) -----------------------------

/// Verifies the LOW fix: `from_b64` now rejects garbage instead of
/// silently mapping non-alphabet bytes to 'A' (which surfaced later as
/// an opaque AES-GCM authentication failure).
#[test]
fn from_b64_rejects_garbage() {
    perms_init();
    assert!(from_b64("###@@@").is_err(), "non-alphabet bytes must error");
    assert!(
        from_b64("AAAA!!!!").is_err(),
        "embedded junk must be rejected"
    );
    // Valid base64 still round-trips.
    let round_trip = from_b64(&to_b64(b"hello world")).unwrap();
    assert_eq!(round_trip, b"hello world");
}

// ---- --fd N out-of-band value (MEDIUM audit fix) ---------------------

/// Verifies the MEDIUM fix: `cmd_load --fd N` writes the plaintext to
/// the specified file descriptor and omits the `"value"` field from the
/// JSON return, so logging the response payload cannot leak the secret.
#[cfg(unix)]
#[test]
fn cmd_load_fd_writes_value_to_fd_and_omits_from_json() {
    perms_init();
    setup();
    let name = unique_name("load-fd");
    cmd_store(&[name.clone(), "fd-secret-xyz".into()]).unwrap();

    // pipe(2) gives us a reader/writer pair we control completely.
    let mut fds = [0i32; 2];
    let r = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(r, 0, "pipe(2) failed");
    let rfd = fds[0];
    let wfd = fds[1];

    let result = cmd_load(&[name.clone(), "--fd".into(), wfd.to_string()]).unwrap();
    unsafe { libc::close(wfd) };

    assert!(
        result.get("value").is_none(),
        "value must not be in JSON when --fd is used: {result}"
    );
    assert_eq!(result["value_fd"], wfd);

    let mut buf = vec![0u8; 64];
    let n = unsafe { libc::read(rfd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    unsafe { libc::close(rfd) };
    assert!(n > 0, "expected bytes on the fd, got {n}");
    buf.truncate(n as usize);
    assert_eq!(buf, b"fd-secret-xyz");
}
