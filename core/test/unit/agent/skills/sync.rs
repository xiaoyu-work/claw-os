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

/// Build a signed skill bundle: write the files into a scratch
/// package, sign it with the process-wide test publisher key, then zip
/// the package (including its `.provenance.json`) at `prefix`.
///
/// Install is signature-gated, so every archive a test expects to
/// install successfully has to go through here.
fn make_signed_zip(path: &Path, id: &str, prefix: Option<&str>, extra: &[(&str, &str)]) {
    let scratch = TempDir::new().unwrap();
    let pkg = scratch.path().join(id);
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("SKILL.md"), good_skill_md(id)).unwrap();
    for (name, body) in extra {
        let target = pkg.join(name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, body).unwrap();
    }
    crate::test_env::sign_test_package(&pkg, crate::provenance::PackageKind::Skill, id);

    let f = File::create(path).unwrap();
    let mut zip = ZipWriter::new(f);
    let opts = SimpleFileOptions::default();
    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
    collect_pkg(&pkg, &pkg, &mut entries);
    entries.sort();
    for (rel, full) in entries {
        let name = match prefix {
            Some(p) => format!("{p}/{rel}"),
            None => rel,
        };
        zip.start_file(name, opts).unwrap();
        zip.write_all(&std::fs::read(&full).unwrap()).unwrap();
    }
    zip.finish().unwrap();
}

fn collect_pkg(root: &Path, dir: &Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    for entry in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            collect_pkg(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
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
    make_signed_zip(&archive, "hello-skill", None, &[("script.py", "print('ok')\n")]);
    let dest = tmp.path().join("skills");
    let res = install_into(&archive, &dest, false).unwrap();
    assert_eq!(res.id, "hello-skill");
    assert_eq!(res.install_dir, dest.join("hello-skill"));
    assert!(res.install_dir.join("SKILL.md").is_file());
    assert!(res.install_dir.join("script.py").is_file());
    assert!(!res.replaced_existing);
    // SKILL.md, script.py and the provenance envelope.
    assert_eq!(res.files_extracted, 3);
    assert!(res.bytes_on_disk > 0);
}

#[test]
fn install_strips_single_wrapper_dir() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("wrapped.zip");
    make_signed_zip(
        &archive,
        "wrapped-skill",
        Some("my-bundle"),
        &[("main.sh", "#!/bin/sh\necho hi\n")],
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
    // The sanitised id is what the envelope must bind to: an id that
    // only matches after sanitisation cannot be signed, so the install
    // is refused rather than landing under a name nobody signed.
    make_zip(&archive, &[("SKILL.md", &good_skill_md("My Skill!"))]);
    let dest = tmp.path().join("skills");
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(format!("{err}").contains("provenance"), "{err}");
    assert!(!dest.join("my-skill").exists());
}

#[test]
fn install_rejects_existing_destination_without_force() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("a.zip");
    make_signed_zip(&archive, "dup", None, &[]);
    let dest = tmp.path().join("skills");
    install_into(&archive, &dest, false).unwrap();
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(matches!(err, SyncError::DestinationExists(_)));
}

#[test]
fn default_install_rejects_builtin_id_collision() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("claw-os.zip");
    make_signed_zip(&archive, "claw-os", None, &[]);
    let user_root = tmp.path().join("user-skills");
    let system_root = tmp.path().join("system-skills");
    fs::create_dir_all(system_root.join("claw-os")).unwrap();
    fs::write(system_root.join("claw-os").join("SKILL.md"), "built in").unwrap();

    let error = install_into_reserved(&archive, &user_root, false, None, Some(&system_root))
        .expect_err("built-in id must be reserved");

    assert!(matches!(error, SyncError::BuiltInConflict { .. }));
    assert!(!user_root.join("claw-os").exists());
}

#[test]
fn install_force_overwrites_existing() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("a.zip");
    make_signed_zip(&archive, "dup2", None, &[("v1.txt", "first install\n")]);
    let dest = tmp.path().join("skills");
    install_into(&archive, &dest, false).unwrap();

    let archive2 = tmp.path().join("b.zip");
    make_signed_zip(&archive2, "dup2", None, &[("v2.txt", "second install\n")]);
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
    make_signed_zip(&a1, "keepme", None, &[("v1.txt", "keep me alive\n")]);
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
    make_signed_zip(&archive, "checksummed", None, &[]);
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

// ----- package provenance -----

#[test]
fn install_rejects_unsigned_bundle_by_default() {
    // There is no environment variable that relaxes this: the old
    // COS_SKILLS_REQUIRE_SIGNATURE opt-in is gone and unsigned bundles
    // fail closed.
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("unsigned.zip");
    make_zip(&archive, &[("SKILL.md", &good_skill_md("must-not-install"))]);
    let dest = tmp.path().join("skills");
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(format!("{err}").contains("provenance"), "{err}");
    assert!(!dest.join("must-not-install").exists());
}

#[test]
fn install_rejects_a_tampered_signed_bundle() {
    let tmp = TempDir::new().unwrap();
    let good = tmp.path().join("good.zip");
    make_signed_zip(&good, "tampered", None, &[("data.txt", "original\n")]);

    // Re-zip the signed package with one file swapped.
    let scratch = TempDir::new().unwrap();
    let unpacked = scratch.path().join("pkg");
    std::fs::create_dir_all(&unpacked).unwrap();
    let reader = File::open(&good).unwrap();
    let mut zip = zip::ZipArchive::new(reader).unwrap();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        let out = unpacked.join(entry.name());
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf).unwrap();
        std::fs::write(out, buf).unwrap();
    }
    std::fs::write(unpacked.join("data.txt"), "evil\n").unwrap();

    let archive = tmp.path().join("tampered.zip");
    let f = File::create(&archive).unwrap();
    let mut writer = ZipWriter::new(f);
    let opts = SimpleFileOptions::default();
    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
    collect_pkg(&unpacked, &unpacked, &mut entries);
    entries.sort();
    for (rel, full) in entries {
        writer.start_file(rel, opts).unwrap();
        writer.write_all(&std::fs::read(&full).unwrap()).unwrap();
    }
    writer.finish().unwrap();

    let dest = tmp.path().join("skills");
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(format!("{err}").contains("digest") || format!("{err}").contains("signature"), "{err}");
    assert!(!dest.join("tampered").exists(), "no half-installed tree");
}

#[test]
fn install_rejects_a_bundle_signed_by_an_untrusted_key() {
    let tmp = TempDir::new().unwrap();
    let scratch = TempDir::new().unwrap();
    let pkg = scratch.path().join("evil-skill");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("SKILL.md"), good_skill_md("evil-skill")).unwrap();
    let stranger = crate::provenance::sign::SigningKeyFile::generate(None).unwrap();
    crate::provenance::sign::sign_directory(
        &pkg,
        &crate::provenance::sign::SignRequest {
            kind: crate::provenance::PackageKind::Skill,
            id: "evil-skill".to_string(),
            version: "0.1.0".to_string(),
            manifest_schema: "test".to_string(),
            manifest_path: "SKILL.md".to_string(),
            entrypoints: vec![],
            resources: vec![],
        },
        &stranger,
    )
    .unwrap();
    crate::test_env::install_test_trust();

    let archive = tmp.path().join("evil.zip");
    let f = File::create(&archive).unwrap();
    let mut writer = ZipWriter::new(f);
    let opts = SimpleFileOptions::default();
    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
    collect_pkg(&pkg, &pkg, &mut entries);
    entries.sort();
    for (rel, full) in entries {
        writer.start_file(rel, opts).unwrap();
        writer.write_all(&std::fs::read(&full).unwrap()).unwrap();
    }
    writer.finish().unwrap();

    let dest = tmp.path().join("skills");
    let err = install_into(&archive, &dest, false).unwrap_err();
    assert!(format!("{err}").contains("trusted"), "{err}");
    assert!(!dest.join("evil-skill").exists());
}
