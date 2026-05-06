//! Local-archive skill installer.
//!
//! Lays a `.zip` skill bundle down at
//! `<agent_skills_dir>/<skill-id>/`. **No network.** Caller passes
//! the path to the archive; the skill id is read from the bundled
//! `SKILL.md` frontmatter (`name:` field). The id is sanitised to a
//! filesystem-safe form before being used as a directory name.
//!
//! ## Atomicity
//!
//! 1. Extract under `<agent_skills_dir>/.staging-<uuid>/`.
//! 2. If the archive contains exactly one wrapper directory, strip
//!    it (matches the convention engines/install_local follows).
//! 3. Validate that `SKILL.md` exists at the extracted root and
//!    parses cleanly via [`crate::agent::skills::manifest::parse`].
//! 4. Atomic-rename staging → `<agent_skills_dir>/<skill-id>/`.
//!    Fails if the destination already exists, unless `force = true`
//!    (in which case the prior install is removed first).
//!
//! On any failure the staging directory is removed so partially-
//! extracted skills never linger.
//!
//! Tar.gz is intentionally not supported here yet — every sample
//! `agentskills.io` bundle in the wild ships as a `.zip`. Wire it
//! up the same way `engine_pkg::install_local::extract` already
//! does once a real tar.gz bundle shows up.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::manifest::{self, ManifestError};

#[derive(Debug)]
pub struct SyncResult {
    pub id: String,
    pub install_dir: PathBuf,
    pub files_extracted: usize,
    pub bytes_on_disk: u64,
    pub replaced_existing: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("archive does not exist: {0}")]
    ArchiveMissing(PathBuf),
    #[error(
        "unsupported archive format: {0} (only .zip is supported today; tar.gz can be added later)"
    )]
    UnsupportedFormat(String),
    #[error("archive is empty after extraction")]
    EmptyArchive,
    #[error("archive layout invalid: SKILL.md not found at {0}")]
    MissingSkillMd(PathBuf),
    #[error("SKILL.md frontmatter invalid: {0}")]
    InvalidManifest(#[from] ManifestError),
    #[error("manifest name is not safe to use as a directory name: {0}")]
    UnsafeSkillName(String),
    #[error(
        "destination already exists: {0} (use --force to overwrite, or remove it first)"
    )]
    DestinationExists(PathBuf),
    #[error("zip slip detected — entry path escapes destination: {0}")]
    PathTraversal(String),
}

/// Install a skill bundle into the default agent skills directory.
pub fn install_from_archive(archive: &Path, force: bool) -> Result<SyncResult, SyncError> {
    install_into(archive, &crate::paths::agent_skills_dir(), force)
}

/// Install into an explicit `skills_root`. Used by tests and by
/// callers that want to install into a per-den tree.
pub fn install_into(
    archive: &Path,
    skills_root: &Path,
    force: bool,
) -> Result<SyncResult, SyncError> {
    if !archive.exists() {
        return Err(SyncError::ArchiveMissing(archive.to_path_buf()));
    }
    let ext = archive
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if ext != "zip" {
        return Err(SyncError::UnsupportedFormat(ext));
    }

    fs::create_dir_all(skills_root)?;
    let staging = skills_root.join(format!(".staging-{}", Uuid::new_v4()));
    fs::create_dir_all(&staging)?;

    let result = (|| -> Result<SyncResult, SyncError> {
        let extracted = extract_zip(archive, &staging)?;
        if extracted.files == 0 {
            return Err(SyncError::EmptyArchive);
        }

        // Strip a single wrapper dir if the bundle ships as
        // `my-skill/SKILL.md` (the typical case).
        let bundle_root = match strip_single_wrapper(&staging)? {
            Some(p) => p,
            None => staging.clone(),
        };

        let manifest_path = bundle_root.join("SKILL.md");
        if !manifest_path.is_file() {
            return Err(SyncError::MissingSkillMd(bundle_root.clone()));
        }
        let raw = fs::read_to_string(&manifest_path)?;
        let doc = manifest::parse(&raw)?;
        let safe_id = sanitize_skill_id(&doc.manifest.name)
            .ok_or_else(|| SyncError::UnsafeSkillName(doc.manifest.name.clone()))?;

        let dest = skills_root.join(&safe_id);
        let mut replaced = false;
        if dest.exists() {
            if !force {
                return Err(SyncError::DestinationExists(dest));
            }
            fs::remove_dir_all(&dest)?;
            replaced = true;
        }

        // If we stripped a wrapper the bundle root lives inside
        // staging — move that. Otherwise move the staging dir
        // itself.
        if bundle_root == staging {
            fs::rename(&staging, &dest)?;
        } else {
            fs::rename(&bundle_root, &dest)?;
            // Best-effort cleanup of the now-empty staging dir.
            let _ = fs::remove_dir_all(&staging);
        }

        let bytes = dir_size(&dest).unwrap_or(0);
        Ok(SyncResult {
            id: safe_id,
            install_dir: dest,
            files_extracted: extracted.files,
            bytes_on_disk: bytes,
            replaced_existing: replaced,
        })
    })();

    if result.is_err() {
        // Roll back partial extraction.
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[derive(Debug)]
struct ExtractStats {
    files: usize,
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<ExtractStats, SyncError> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut files = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let raw_name = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => return Err(SyncError::PathTraversal(entry.name().to_string())),
        };
        let outpath = dest.join(&raw_name);
        if !outpath.starts_with(dest) {
            return Err(SyncError::PathTraversal(entry.name().to_string()));
        }
        if entry.is_dir() {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = File::create(&outpath)?;
            io::copy(&mut entry, &mut out)?;
            files += 1;
        }
    }
    Ok(ExtractStats { files })
}

fn strip_single_wrapper(dir: &Path) -> Result<Option<PathBuf>, SyncError> {
    let entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    if entries.len() != 1 {
        return Ok(None);
    }
    let only = &entries[0];
    if only.file_type()?.is_dir() {
        Ok(Some(only.path()))
    } else {
        Ok(None)
    }
}

fn dir_size(p: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(p)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            total = total.saturating_add(dir_size(&entry.path())?);
        } else {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

/// Reduce a manifest `name:` field to a filesystem-safe directory
/// id. Returns `None` if no usable characters remain (e.g. the
/// name was made entirely of separators or whitespace).
///
/// Rules:
/// - lowercase ASCII letters/digits and `-`/`_` pass through
/// - all other characters become `-`
/// - leading/trailing `-`/`_`/`.` stripped
/// - consecutive separators collapsed to a single `-`
/// - reserved names (`.`, `..`, empty) rejected
pub fn sanitize_skill_id(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut prev_sep = false;
    for ch in name.chars() {
        let safe = matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_');
        if safe {
            out.push(ch.to_ascii_lowercase());
            prev_sep = false;
        } else if !prev_sep && !out.is_empty() {
            out.push('-');
            prev_sep = true;
        }
    }
    let trimmed = out.trim_matches(|c: char| c == '-' || c == '_' || c == '.');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
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
        assert_eq!(sanitize_skill_id("Foo-Bar_42").as_deref(), Some("foo-bar_42"));
    }

    #[test]
    fn sanitize_replaces_unsafe_chars_with_dash() {
        assert_eq!(sanitize_skill_id("hello world!").as_deref(), Some("hello-world"));
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
        assert_eq!(sanitize_skill_id("---my-skill---").as_deref(), Some("my-skill"));
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
        let archive = tmp.path().join("bundle.tar.gz");
        File::create(&archive).unwrap();
        let dest = tmp.path().join("skills");
        let err = install_into(&archive, &dest, false).unwrap_err();
        assert!(matches!(err, SyncError::UnsupportedFormat(s) if s == "gz"));
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
}
