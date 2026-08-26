use super::*;
use std::io::Write;
use tempfile::TempDir;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

fn make_zip(path: &Path, files: &[(&str, &str)]) {
    let f = File::create(path).unwrap();
    let mut zip = ZipWriter::new(f);
    let opts = SimpleFileOptions::default();
    for (name, content) in files {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn good_skill_md(name: &str) -> String {
    format!(
        "---\nname: {name}\nversion: 0.1.0\ndescription: test skill\n---\n# {name}\n\nA test skill.\n",
        name = name
    )
}

#[test]
fn sanitize_lowercases_and_keeps_dash_underscore() {
    assert_eq!(
        sanitize_skill_id("Foo-Bar_42").as_deref(),
        Some("foo-bar_42")
    );
}

#[test]
fn sanitize_replaces_unsafe_chars_with_dash() {
    assert_eq!(
        sanitize_skill_id("hello world!").as_deref(),
        Some("hello-world")
    );
}

#[test]
fn sanitize_collapses_consecutive_separators() {
    assert_eq!(sanitize_skill_id("a    b///c").as_deref(), Some("a-b-c"));
}

#[test]
fn sanitize_rejects_empty_after_strip() {
    assert!(sanitize_skill_id("").is_none());
    assert!(sanitize_skill_id("   ").is_none());
    assert!(sanitize_skill_id("///").is_none());
    assert!(sanitize_skill_id("..").is_none());
    assert!(sanitize_skill_id(".").is_none());
}

#[test]
fn sanitize_strips_leading_trailing_separators() {
    assert_eq!(
        sanitize_skill_id("---my-skill---").as_deref(),
        Some("my-skill")
    );
}

#[test]
fn missing_archive_returns_error() {
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let err = install_into(Path::new("/no/such/file.zip"), &dest, false).unwrap_err();
    assert!(matches!(err, SyncError::ArchiveMissing(_)));
}

#[test]
fn unsupported_format_rejected() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("bundle.rar");
    File::create(&archive).unwrap();
    let dest = tmp.path().join("skills");
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(matches!(err, SyncError::UnsupportedFormat(s) if s == "rar"));
}

#[test]
fn install_installs_flat_bundle() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("flat.zip");
    make_zip(
        &archive,
        &[
            ("SKILL.md", &good_skill_md("hello-skill")),
            ("script.py", "print('ok')\n"),
        ],
    );
    let dest = tmp.path().join("skills");
    let res = install_into(&archive, &dest, false).unwrap();
    assert_eq!(res.id, "hello-skill");
    assert_eq!(res.install_dir, dest.join("hello-skill"));
    assert!(res.install_dir.join("SKILL.md").is_file());
    assert!(res.install_dir.join("script.py").is_file());
    assert!(!res.replaced_existing);
    assert_eq!(res.files_extracted, 2);
    assert!(res.bytes_on_disk > 0);
}

#[test]
fn install_strips_single_wrapper_dir() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("wrapped.zip");
    make_zip(
        &archive,
        &[
            ("my-bundle/SKILL.md", &good_skill_md("wrapped-skill")),
            ("my-bundle/main.sh", "#!/bin/sh\necho hi\n"),
        ],
    );
    let dest = tmp.path().join("skills");
    let res = install_into(&archive, &dest, false).unwrap();
    assert_eq!(res.id, "wrapped-skill");
    assert!(res.install_dir.join("SKILL.md").is_file());
    assert!(res.install_dir.join("main.sh").is_file());
}

#[test]
fn install_uses_sanitised_id() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("cap.zip");
    make_zip(&archive, &[("SKILL.md", &good_skill_md("My Skill!"))]);
    let dest = tmp.path().join("skills");
    let res = install_into(&archive, &dest, false).unwrap();
    assert_eq!(res.id, "my-skill");
    assert_eq!(res.install_dir, dest.join("my-skill"));
}

#[test]
fn install_rejects_existing_destination_without_force() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("a.zip");
    make_zip(&archive, &[("SKILL.md", &good_skill_md("dup"))]);
    let dest = tmp.path().join("skills");
    install_into(&archive, &dest, false).unwrap();
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(matches!(err, SyncError::DestinationExists(_)));
}

#[test]
fn default_install_rejects_builtin_id_collision() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("claw-os.zip");
    make_zip(&archive, &[("SKILL.md", &good_skill_md("claw-os"))]);
    let user_root = tmp.path().join("user-skills");
    let system_root = tmp.path().join("system-skills");
    fs::create_dir_all(system_root.join("claw-os")).unwrap();
    fs::write(system_root.join("claw-os").join("SKILL.md"), "built in").unwrap();

    let error = install_into_with_policy_reserved(
        &archive,
        &user_root,
        false,
        None,
        &SignatureVerifyConfig::default(),
        Some(&system_root),
    )
    .expect_err("built-in id must be reserved");

    assert!(matches!(error, SyncError::BuiltInConflict { .. }));
    assert!(!user_root.join("claw-os").exists());
}

#[test]
fn install_force_overwrites_existing() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("a.zip");
    make_zip(
        &archive,
        &[
            ("SKILL.md", &good_skill_md("dup2")),
            ("v1.txt", "first install\n"),
        ],
    );
    let dest = tmp.path().join("skills");
    install_into(&archive, &dest, false).unwrap();

    let archive2 = tmp.path().join("b.zip");
    make_zip(
        &archive2,
        &[
            ("SKILL.md", &good_skill_md("dup2")),
            ("v2.txt", "second install\n"),
        ],
    );
    let res = install_into(&archive2, &dest, true).unwrap();
    assert!(res.replaced_existing);
    // Old file gone, new file present.
    assert!(!dest.join("dup2").join("v1.txt").exists());
    assert!(dest.join("dup2").join("v2.txt").is_file());
}

#[test]
fn install_rejects_missing_skill_md() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("nomd.zip");
    make_zip(&archive, &[("README.txt", "no manifest here\n")]);
    let dest = tmp.path().join("skills");
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(matches!(err, SyncError::MissingSkillMd(_)));
    // Staging dir cleaned up.
    let stage_remnants: Vec<_> = std::fs::read_dir(&dest)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".staging-"))
        .collect();
    assert!(stage_remnants.is_empty(), "staging dir leaked");
}

#[test]
fn install_rejects_invalid_manifest() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("bad.zip");
    make_zip(&archive, &[("SKILL.md", "no frontmatter at all\n")]);
    let dest = tmp.path().join("skills");
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(matches!(err, SyncError::InvalidManifest(_)));
}

#[test]
fn install_rejects_zip_slip() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("evil.zip");
    // Construct a zip with a path-traversal entry name. We
    // bypass `start_file`'s validation by writing the central
    // directory header directly via raw mode — but the simpler
    // way is to use a name like `..\\evil` on Windows; use
    // `../evil` on Unix.
    let f = File::create(&archive).unwrap();
    let mut zip = ZipWriter::new(f);
    let opts = SimpleFileOptions::default();
    // Some zip impls sanitise the name; the `enclosed_name()`
    // check in our extractor catches both forms.
    zip.start_file("../evil-skill/SKILL.md", opts).unwrap();
    zip.write_all(b"---\nname: bad\n---\n").unwrap();
    zip.finish().unwrap();
    let dest = tmp.path().join("skills");
    // `enclosed_name()` returns None for `..` segments → we
    // raise PathTraversal. Either path is acceptable; the
    // important thing is no file lands outside `dest`.
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(matches!(err, SyncError::PathTraversal(_)));
}

#[test]
fn install_rejects_empty_archive() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("empty.zip");
    let f = File::create(&archive).unwrap();
    // Empty central directory.
    ZipWriter::new(f).finish().unwrap();
    let dest = tmp.path().join("skills");
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(matches!(err, SyncError::EmptyArchive));
}

#[test]
fn install_handles_unicode_skill_name() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("u.zip");
    make_zip(&archive, &[("SKILL.md", &good_skill_md("调研助手"))]);
    let dest = tmp.path().join("skills");
    // All chars are non-ASCII so sanitise yields nothing →
    // UnsafeSkillName.
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(matches!(err, SyncError::UnsafeSkillName(_)));
}

#[test]
fn zip_bomb_rejected() {
    // Build an archive whose one entry compresses ~30 KiB of
    // zeros to under 100 bytes — well above MAX_COMPRESSION_RATIO
    // (100:1) and above the small-entry exemption (16 KiB).
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("bomb.zip");
    let f = File::create(&archive).unwrap();
    let mut zip = ZipWriter::new(f);
    let opts =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("SKILL.md", opts).unwrap();
    zip.write_all(b"---\nname: bomb\n---\n").unwrap();
    zip.start_file("payload.bin", opts).unwrap();
    // 64 MiB of zeros → DEFLATE compresses to roughly 64 KiB, a
    // ratio of ~1000:1 — comfortably above MAX_COMPRESSION_RATIO.
    let zeros = vec![0u8; 64 * 1024 * 1024];
    zip.write_all(&zeros).unwrap();
    zip.finish().unwrap();
    let dest = tmp.path().join("skills");
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(
        matches!(err, SyncError::ZipBomb { .. } | SyncError::ZipTooLarge { .. }),
        "expected ZipBomb or ZipTooLarge, got {err:?}"
    );
    // No leaked directories.
    if dest.exists() {
        let leaked: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        for e in leaked {
            let n = e.file_name().to_string_lossy().to_string();
            assert!(
                !n.starts_with(".staging-"),
                "staging dir not cleaned up: {n}"
            );
        }
    }
}

#[test]
fn install_atomic_on_failure() {
    // First install OK; second install with --force fails partway
    // through (invalid manifest in the new archive). The existing
    // install must survive intact.
    let tmp = TempDir::new().unwrap();
    let a1 = tmp.path().join("v1.zip");
    make_zip(
        &a1,
        &[
            ("SKILL.md", &good_skill_md("keepme")),
            ("v1.txt", "keep me alive\n"),
        ],
    );
    let dest = tmp.path().join("skills");
    install_into(&a1, &dest, false).unwrap();

    // Force-install a *valid skill id* (so dest path collides)
    // but with a broken manifest so the second install fails
    // mid-flight, after we've renamed the old dir aside.
    let a2 = tmp.path().join("v2.zip");
    make_zip(
        &a2,
        &[("SKILL.md", "no frontmatter — guaranteed to fail\n")],
    );
    // The new archive doesn't share an id, so this targets a
    // different dest — emulate the same-id case by giving it the
    // same name in the manifest, but invalid frontmatter forces
    // a failure before we rename anything. To exercise the
    // *backup restore* path we need to fail *after* the rename;
    // construct a zip whose manifest parses (same id "keepme")
    // but whose `name:` would re-sanitise to a different id…
    // Easier: a zip with id "keepme" but a path-traversal
    // entry that fails extract_zip. That fails BEFORE the
    // rename so we cover only the staging-only-rollback path.
    //
    // To cover the rename-then-fail path: provide a zip whose
    // manifest parses with id "keepme" but extract_zip fails
    // for a later entry. Construct such a zip below.
    let a3 = tmp.path().join("v3.zip");
    let f = File::create(&a3).unwrap();
    let mut zip = ZipWriter::new(f);
    let opts = SimpleFileOptions::default();
    zip.start_file("SKILL.md", opts).unwrap();
    zip.write_all(good_skill_md("keepme").as_bytes()).unwrap();
    // Entry name with a literal `..` segment — caught by our
    // explicit ParentDir check. Some zip libs accept this.
    zip.start_file("../escape.txt", opts).unwrap();
    zip.write_all(b"bad").unwrap();
    zip.finish().unwrap();

    let err = install_into(&a3, &dest, true).unwrap_err();
    assert!(matches!(err, SyncError::PathTraversal(_)));

    // Original install must still exist and be intact.
    let live = dest.join("keepme");
    assert!(live.is_dir(), "live install was deleted on failure");
    assert!(live.join("SKILL.md").is_file());
    assert!(
        live.join("v1.txt").is_file(),
        "v1 contents lost on failed --force"
    );

    // And no stale `.bak-*` directory left over.
    let stale_baks: Vec<_> = std::fs::read_dir(&dest)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".bak-"))
        .collect();
    assert!(
        stale_baks.is_empty(),
        "stale .bak-* leaked: {:?}",
        stale_baks
            .iter()
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );
}

#[test]
fn install_verifies_sha256_when_provided() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("a.zip");
    make_zip(
        &archive,
        &[("SKILL.md", &good_skill_md("checksummed"))],
    );
    let actual = sha256_file(&archive).unwrap();
    let dest = tmp.path().join("skills");

    // Wrong digest is rejected, dest untouched.
    let bad =
        install_into_verified(&archive, &dest, false, Some("00".repeat(32).as_str()))
            .unwrap_err();
    assert!(matches!(bad, SyncError::ChecksumMismatch { .. }));
    assert!(!dest.join("checksummed").exists());

    // Correct digest installs cleanly.
    let ok = install_into_verified(&archive, &dest, false, Some(&actual)).unwrap();
    assert_eq!(ok.id, "checksummed");
}

// ----- ed25519 signature flow -----

#[test]
fn install_rejects_malformed_trusted_keys_before_side_effects() {
    let _lock = crate::test_env::lock_env();
    let _trusted_keys = crate::test_env::TestEnvVarGuard::set(
        provenance::ENV_TRUSTED_KEYS,
        "not-hex",
    );
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("unsigned.zip");
    make_zip(
        &archive,
        &[("SKILL.md", &good_skill_md("must-not-install"))],
    );
    let dest = tmp.path().join("skills");

    let err = install_into(&archive, &dest, false).unwrap_err();
    let message = err.to_string();

    assert!(matches!(err, SyncError::SignatureConfig(_)));
    assert!(message.contains(provenance::ENV_TRUSTED_KEYS));
    assert!(message.contains("not valid hex"));
    assert!(!dest.exists(), "install created the skills directory");
}

#[test]
fn install_honors_valid_trusted_keys_from_env() {
    let _lock = crate::test_env::lock_env();
    let _require_signature = crate::test_env::TestEnvVarGuard::set(
        provenance::ENV_REQUIRE_SIGNATURE,
        "true",
    );
    let _trusted_keys = crate::test_env::TestEnvVarGuard::set(
        provenance::ENV_TRUSTED_KEYS,
        hex::encode([7u8; 32]),
    );
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("signed.zip");
    let (md, signer_key) = signed_skill_md("env-signed", "0.1.0");
    make_zip(&archive, &[("SKILL.md", &md)]);
    let dest = tmp.path().join("skills");

    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(matches!(
        err,
        SyncError::Signature(SignatureError::UntrustedKey { .. })
    ));
    assert!(
        !dest.join("env-signed").exists(),
        "untrusted skill was installed"
    );

    std::env::set_var(
        provenance::ENV_TRUSTED_KEYS,
        hex::encode(signer_key),
    );
    let installed = install_into(&archive, &dest, false).unwrap();
    assert_eq!(installed.id, "env-signed");
}

/// Build a SKILL.md whose signature block authenticates the
/// canonical signing input for the rest of the manifest.
fn signed_skill_md(name: &str, version: &str) -> (String, [u8; 32]) {
    use ed25519_dalek::{Signer, SigningKey};
    let secret: [u8; 32] = [42u8; 32];
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let pk_hex = hex::encode(verifying_key.to_bytes());

    // Parse the unsigned form first to recover the same
    // canonical bytes that the verifier will compute when the
    // signed SKILL.md is loaded from disk.
    let unsigned = format!(
        "---\nname: {name}\nversion: {version}\ndescription: test skill\n---\n# {name}\n"
    );
    let doc = manifest::parse(&unsigned).unwrap();
    let canonical = manifest::canonical_signing_input(&doc.manifest);
    let mut hasher = crate::crypto::Sha256Stream::new();
    hasher.update(&canonical);
    let digest = hasher.finalize_bytes();
    let signature = signing_key.sign(&digest);
    let sig_hex = hex::encode(signature.to_bytes());

    let signed = format!(
        "---\nname: {name}\nversion: {version}\ndescription: test skill\nsignature:\n  algorithm: ed25519\n  public_key: {pk_hex}\n  value: {sig_hex}\n---\n# {name}\n"
    );
    (signed, verifying_key.to_bytes())
}

#[test]
fn install_accepts_valid_signature() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("signed.zip");
    let (md, _pk) = signed_skill_md("signed-skill", "0.1.0");
    make_zip(&archive, &[("SKILL.md", &md)]);
    let dest = tmp.path().join("skills");
    let policy = SignatureVerifyConfig {
        require_signature: true,
        trusted_keys: None,
    };
    let res =
        install_into_with_policy(&archive, &dest, false, None, &policy).unwrap();
    assert_eq!(res.id, "signed-skill");
}

#[test]
fn install_rejects_tampered_manifest() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("tampered.zip");
    let (mut md, _pk) = signed_skill_md("tampered", "0.1.0");
    // Flip the version after signing — every byte of the
    // canonical signing input feeds the digest, so a value
    // change must invalidate the signature.
    md = md.replace("version: 0.1.0", "version: 9.9.9");
    make_zip(&archive, &[("SKILL.md", &md)]);
    let dest = tmp.path().join("skills");
    let policy = SignatureVerifyConfig::default();
    let err =
        install_into_with_policy(&archive, &dest, false, None, &policy).unwrap_err();
    match err {
        SyncError::Signature(SignatureError::BadSignature(_)) => {}
        other => panic!("expected BadSignature, got {other:?}"),
    }
    // No half-installed tree on rejection.
    assert!(!dest.join("tampered").exists());
}

#[test]
fn install_rejects_unsigned_when_required() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("nosig.zip");
    make_zip(&archive, &[("SKILL.md", &good_skill_md("nosig"))]);
    let dest = tmp.path().join("skills");
    let policy = SignatureVerifyConfig {
        require_signature: true,
        trusted_keys: None,
    };
    let err =
        install_into_with_policy(&archive, &dest, false, None, &policy).unwrap_err();
    assert!(matches!(
        err,
        SyncError::Signature(SignatureError::Required)
    ));
    // And without the policy flag, the same archive installs.
    let res =
        install_into_with_policy(&archive, &dest, false, None, &SignatureVerifyConfig::default())
            .unwrap();
    assert_eq!(res.id, "nosig");
}

#[test]
fn install_rejects_untrusted_key() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("evil.zip");
    let (md, _signer_pk) = signed_skill_md("evil-skill", "0.1.0");
    make_zip(&archive, &[("SKILL.md", &md)]);
    let dest = tmp.path().join("skills");
    // Allow-list contains a *different* key than the one that
    // signed this manifest.
    let other_key: [u8; 32] = [7u8; 32];
    let policy = SignatureVerifyConfig {
        require_signature: true,
        trusted_keys: Some(vec![other_key]),
    };
    let err =
        install_into_with_policy(&archive, &dest, false, None, &policy).unwrap_err();
    assert!(matches!(
        err,
        SyncError::Signature(SignatureError::UntrustedKey { .. })
    ));
}
