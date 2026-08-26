//! Local-archive skill installer.
//!
//! Lays a `.zip` or `.tar.gz` skill bundle down at
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
//! Both `.zip` and `.tar.gz` / `.tgz` bundles are supported. Hub assets
//! (see [`crate::agent::skills::hub`]) ship as `.tar.gz`; local
//! `agentskills.io` bundles are `.zip`. Both extractors enforce the same
//! zip-slip / path-depth / per-entry / total-size caps and reject any
//! non-regular-file entry (symlinks, hard links, devices).

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::manifest::{self, ManifestError};
use super::provenance::{self, SignatureCheck, SignatureError, SignatureVerifyConfig};

/// Hard cap on how much *uncompressed* data a single zip may produce,
/// regardless of advertised entry sizes. Defends against zip bombs.
pub const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum number of entries we accept inside one zip.
pub const MAX_ZIP_ENTRIES: usize = 10_000;
/// Maximum nesting depth (number of path components) for any entry.
pub const MAX_PATH_DEPTH: usize = 16;
/// Per-entry safety cap — even a single advertised-tiny entry can
/// expand to GBs. Cap at 128 MiB.
pub const MAX_PER_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
/// Reject any zip entry whose advertised compression ratio exceeds
/// this — i.e., `uncompressed / compressed > MAX_COMPRESSION_RATIO`.
/// 100:1 is well above legitimate text compression (~10:1) and below
/// typical zip-bomb ratios (1000:1+).
pub const MAX_COMPRESSION_RATIO: u64 = 100;

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
    #[error("unsupported archive format: {0} (supported: .zip, .tar.gz, .tgz)")]
    UnsupportedFormat(String),
    #[error("archive is empty after extraction")]
    EmptyArchive,
    #[error("archive layout invalid: SKILL.md not found at {0}")]
    MissingSkillMd(PathBuf),
    #[error("SKILL.md frontmatter invalid: {0}")]
    InvalidManifest(#[from] ManifestError),
    #[error("manifest name is not safe to use as a directory name: {0}")]
    UnsafeSkillName(String),
    #[error("destination already exists: {0} (use --force to overwrite, or remove it first)")]
    DestinationExists(PathBuf),
    #[error("zip slip detected — entry path escapes destination: {0}")]
    PathTraversal(String),
    #[error("archive rejected: too many entries ({count}); cap {cap}")]
    ZipTooManyEntries { count: usize, cap: usize },
    #[error("archive rejected: total uncompressed size exceeds cap ({cap} bytes)")]
    ZipTooLarge { cap: u64 },
    #[error("archive rejected: entry `{name}` exceeds per-entry size cap ({cap} bytes)")]
    ZipEntryTooLarge { name: String, cap: u64 },
    #[error("archive rejected: entry `{name}` has suspicious compression ratio (> {ratio}:1)")]
    ZipBomb { name: String, ratio: u64 },
    #[error("archive rejected: entry path `{name}` is too deeply nested (max depth {cap})")]
    ZipPathTooDeep { name: String, cap: usize },
    #[error("archive integrity check failed: expected sha256 {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("manifest signature rejected: {0}")]
    Signature(#[from] SignatureError),
}

/// Install a skill bundle into the default agent skills directory.
pub fn install_from_archive(archive: &Path, force: bool) -> Result<SyncResult, SyncError> {
    install_into(archive, &crate::paths::agent_skills_dir(), force)
}

/// Install with an optional SHA-256 integrity check. The expected
/// digest is the lower-case hex sha256 of the archive bytes (as
/// published by the catalogue). Use [`install_into`] when the caller
/// has no expected digest to verify against.
pub fn install_from_archive_verified(
    archive: &Path,
    force: bool,
    expected_sha256: Option<&str>,
) -> Result<SyncResult, SyncError> {
    install_into_verified(
        archive,
        &crate::paths::agent_skills_dir(),
        force,
        expected_sha256,
    )
}

/// Install into an explicit `skills_root`. Used by tests and by
/// callers that want to install into a per-home tree.
pub fn install_into(
    archive: &Path,
    skills_root: &Path,
    force: bool,
) -> Result<SyncResult, SyncError> {
    install_into_verified(archive, skills_root, force, None)
}

/// Install into an explicit `skills_root` with an optional SHA-256
/// integrity check. When `expected_sha256` is `Some(_)` the archive
/// bytes are hashed and the install is rejected on mismatch.
///
/// Signature policy is read from the process environment via
/// [`SignatureVerifyConfig::from_env`]. Tests can dial the policy
/// explicitly through [`install_into_with_policy`].
pub fn install_into_verified(
    archive: &Path,
    skills_root: &Path,
    force: bool,
    expected_sha256: Option<&str>,
) -> Result<SyncResult, SyncError> {
    install_into_with_policy(
        archive,
        skills_root,
        force,
        expected_sha256,
        &SignatureVerifyConfig::from_env(),
    )
}

/// Install with an explicit signature policy. The other knobs
/// (`force`, `expected_sha256`) work as in [`install_into_verified`].
/// Pulled out so unit tests can drive the signature flow without
/// having to mutate process env state.
pub fn install_into_with_policy(
    archive: &Path,
    skills_root: &Path,
    force: bool,
    expected_sha256: Option<&str>,
    signature_config: &SignatureVerifyConfig,
) -> Result<SyncResult, SyncError> {
    if !archive.exists() {
        return Err(SyncError::ArchiveMissing(archive.to_path_buf()));
    }
    let fname = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let is_tar_gz = fname.ends_with(".tar.gz") || fname.ends_with(".tgz");
    let is_zip = fname.ends_with(".zip");
    if !is_zip && !is_tar_gz {
        let ext = archive
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        return Err(SyncError::UnsupportedFormat(ext));
    }

    // Optional archive integrity check before extraction. Streams the
    // bytes through a sha256 hasher so we never load the whole file
    // into RAM just to digest it.
    if let Some(expected) = expected_sha256 {
        let actual = sha256_file(archive)?;
        let expected_norm = expected.trim().to_ascii_lowercase();
        if actual != expected_norm {
            return Err(SyncError::ChecksumMismatch {
                expected: expected_norm,
                actual,
            });
        }
    }

    fs::create_dir_all(skills_root)?;
    let staging = skills_root.join(format!(".staging-{}", Uuid::new_v4()));
    fs::create_dir_all(&staging)?;

    // Track a backup directory created when `force=true`. We move
    // the live install aside (atomic rename) instead of deleting it
    // outright so a failure mid-rename can be rolled back without
    // losing the user's hand-edits.
    let mut backup: Option<PathBuf> = None;

    let result = (|| -> Result<SyncResult, SyncError> {
        let extracted = if is_tar_gz {
            extract_tar_gz(archive, &staging)?
        } else {
            extract_zip(archive, &staging)?
        };
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

        // Authenticate the manifest before we let it influence the
        // install destination. Unsigned manifests log a warning
        // (operator can spot them in audit) but go through when the
        // policy allows it.
        match provenance::verify_signature(&doc.manifest, signature_config)? {
            SignatureCheck::Verified { public_key_hex } => {
                tracing::info!(
                    skill = %doc.manifest.name,
                    key = %public_key_hex,
                    "skill manifest signature verified"
                );
            }
            SignatureCheck::Unsigned => {
                tracing::warn!(
                    skill = %doc.manifest.name,
                    "installing skill without a signature — set \
                     COS_SKILLS_REQUIRE_SIGNATURE=1 to refuse unsigned manifests"
                );
            }
        }

        let safe_id = sanitize_skill_id(&doc.manifest.name)
            .ok_or_else(|| SyncError::UnsafeSkillName(doc.manifest.name.clone()))?;

        let dest = skills_root.join(&safe_id);
        let mut replaced = false;
        if dest.exists() {
            if !force {
                return Err(SyncError::DestinationExists(dest));
            }
            // Move the existing install aside atomically. We name
            // the backup with a uuid suffix so concurrent installs
            // of the same skill don't collide on the backup path
            // and so a partial cleanup never confuses a later run.
            let bak = skills_root.join(format!(".bak-{safe_id}-{}", Uuid::new_v4()));
            fs::rename(&dest, &bak)?;
            backup = Some(bak);
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

    match &result {
        Ok(_) => {
            // Install succeeded — drop the backup so disk doesn't
            // grow unbounded across repeated `--force` upgrades.
            if let Some(bak) = backup.take() {
                let _ = fs::remove_dir_all(&bak);
            }
        }
        Err(_) => {
            // Roll back partial extraction.
            let _ = fs::remove_dir_all(&staging);
            // Restore the prior install from the backup we kept.
            if let Some(bak) = backup.take() {
                // Best-effort: if the destination doesn't exist (the
                // rename never happened, or it was reverted), put
                // the backup back; if it does (post-rename failure
                // mid-extract is rare here but possible), leave the
                // backup in place so the user can recover manually.
                // We use sanitize_skill_id from the parsed manifest
                // when available; for failures before manifest is
                // parsed we still kept the original dest unchanged.
                if let Some(orig) = strip_backup_suffix(&bak, skills_root) {
                    if !orig.exists() {
                        let _ = fs::rename(&bak, &orig);
                    }
                }
            }
        }
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
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err(SyncError::ZipTooManyEntries {
            count: zip.len(),
            cap: MAX_ZIP_ENTRIES,
        });
    }
    // Canonicalise the destination once so all path-escape checks are
    // resilient against symlinks placed inside `dest` before us.
    let dest_canon = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());
    let mut files = 0usize;
    let mut total_uncompressed: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let raw_name = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => return Err(SyncError::PathTraversal(entry.name().to_string())),
        };
        // Reject absolute paths and any `..` component explicitly,
        // independently of zip's enclosed_name check. Some
        // implementations leak through component-aware traversal
        // patterns; belt-and-suspenders.
        if raw_name.is_absolute()
            || raw_name
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(SyncError::PathTraversal(entry.name().to_string()));
        }
        let depth = raw_name
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .count();
        if depth > MAX_PATH_DEPTH {
            return Err(SyncError::ZipPathTooDeep {
                name: entry.name().to_string(),
                cap: MAX_PATH_DEPTH,
            });
        }

        // Per-entry advertised size sanity check before we touch
        // disk. The actual extracted byte count is also enforced
        // by `io::copy(&mut entry.take(...))` below.
        let advertised = entry.size();
        if advertised > MAX_PER_ENTRY_BYTES {
            return Err(SyncError::ZipEntryTooLarge {
                name: entry.name().to_string(),
                cap: MAX_PER_ENTRY_BYTES,
            });
        }
        let compressed = entry.compressed_size().max(1); // avoid div-by-zero
        if advertised > 0
            && compressed > 0
            && advertised / compressed > MAX_COMPRESSION_RATIO
            && advertised > 16 * 1024
        {
            // Skip the ratio gate for very small entries (<= 16 KiB)
            // where the ratio is dominated by zip overhead and easily
            // tripped by legitimate plaintext.
            return Err(SyncError::ZipBomb {
                name: entry.name().to_string(),
                ratio: MAX_COMPRESSION_RATIO,
            });
        }

        let outpath = dest.join(&raw_name);
        // Guard against symlink-races inside `dest`: resolve the
        // parent (which exists by the time we extract a deep file)
        // and verify it stays inside `dest_canon`.
        if let Some(parent) = outpath.parent() {
            fs::create_dir_all(parent)?;
            if let Ok(parent_canon) = parent.canonicalize() {
                if !parent_canon.starts_with(&dest_canon) {
                    return Err(SyncError::PathTraversal(entry.name().to_string()));
                }
            }
        }
        if !outpath.starts_with(dest) {
            return Err(SyncError::PathTraversal(entry.name().to_string()));
        }
        if entry.is_dir() {
            fs::create_dir_all(&outpath)?;
            continue;
        }
        // Reject symlink entries outright. Allowing them would let
        // an archive plant a link pointing outside `dest` that
        // subsequent writes follow.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Zip type-3 entries (symlinks) advertise themselves
            // through unix_mode. We reject anything that isn't a
            // regular file once we know we have unix-mode metadata.
            if let Some(mode) = entry.unix_mode() {
                let perms = std::fs::Permissions::from_mode(mode);
                let _ = perms; // touch to keep `PermissionsExt` import alive
                let ftype = mode & 0o170000;
                if ftype != 0 && ftype != 0o100000 && ftype != 0o040000 {
                    return Err(SyncError::PathTraversal(format!(
                        "{} (non-regular file in archive)",
                        entry.name()
                    )));
                }
            }
        }

        // Cap the per-entry extraction byte count regardless of
        // advertised size — handles archives that lie in the
        // local header.
        let remaining = MAX_TOTAL_UNCOMPRESSED_BYTES.saturating_sub(total_uncompressed);
        let per_entry_cap = MAX_PER_ENTRY_BYTES.min(remaining);
        if per_entry_cap == 0 {
            return Err(SyncError::ZipTooLarge {
                cap: MAX_TOTAL_UNCOMPRESSED_BYTES,
            });
        }
        let mut out = File::create(&outpath)?;
        let mut reader = (&mut entry).take(per_entry_cap + 1);
        let written = io::copy(&mut reader, &mut out)?;
        if written > per_entry_cap {
            return Err(SyncError::ZipEntryTooLarge {
                name: entry.name().to_string(),
                cap: MAX_PER_ENTRY_BYTES,
            });
        }
        total_uncompressed = total_uncompressed.saturating_add(written);
        if total_uncompressed > MAX_TOTAL_UNCOMPRESSED_BYTES {
            return Err(SyncError::ZipTooLarge {
                cap: MAX_TOTAL_UNCOMPRESSED_BYTES,
            });
        }
        files += 1;
    }
    Ok(ExtractStats { files })
}

/// Extract a gzip-compressed tarball with the same safety envelope as
/// [`extract_zip`]: entry-count cap, path-depth cap, per-entry and total
/// uncompressed-size caps, zip-slip rejection, and a hard refusal of any
/// non-regular-file entry (symlinks, hard links, devices) so a hostile
/// bundle can't plant a link that subsequent writes follow out of `dest`.
/// Hub assets ship as `.tar.gz`, so this is the path hub installs take.
fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<ExtractStats, SyncError> {
    let file = File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    let dest_canon = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());

    let mut files = 0usize;
    let mut entry_count = 0usize;
    let mut total_uncompressed: u64 = 0;

    for entry in tar.entries()? {
        let mut entry = entry?;
        entry_count += 1;
        if entry_count > MAX_ZIP_ENTRIES {
            return Err(SyncError::ZipTooManyEntries {
                count: entry_count,
                cap: MAX_ZIP_ENTRIES,
            });
        }

        let raw_name = entry.path()?.to_path_buf();

        // Reject absolute paths and any `..` component (tar-slip).
        if raw_name.is_absolute()
            || raw_name
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(SyncError::PathTraversal(raw_name.display().to_string()));
        }
        let depth = raw_name
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .count();
        if depth > MAX_PATH_DEPTH {
            return Err(SyncError::ZipPathTooDeep {
                name: raw_name.display().to_string(),
                cap: MAX_PATH_DEPTH,
            });
        }

        let etype = entry.header().entry_type();
        // Only regular files and directories may be laid down. Symlinks
        // and hard links could redirect writes outside `dest`.
        if etype.is_symlink() || etype.is_hard_link() {
            return Err(SyncError::PathTraversal(format!(
                "{} (link in archive)",
                raw_name.display()
            )));
        }

        let advertised = entry.header().size().unwrap_or(0);
        if advertised > MAX_PER_ENTRY_BYTES {
            return Err(SyncError::ZipEntryTooLarge {
                name: raw_name.display().to_string(),
                cap: MAX_PER_ENTRY_BYTES,
            });
        }

        let outpath = dest.join(&raw_name);
        // Resolve the parent (created on demand) and verify it stays
        // inside `dest_canon`, defeating symlink races inside staging.
        if let Some(parent) = outpath.parent() {
            fs::create_dir_all(parent)?;
            if let Ok(parent_canon) = parent.canonicalize() {
                if !parent_canon.starts_with(&dest_canon) {
                    return Err(SyncError::PathTraversal(raw_name.display().to_string()));
                }
            }
        }
        if !outpath.starts_with(dest) {
            return Err(SyncError::PathTraversal(raw_name.display().to_string()));
        }

        if etype.is_dir() {
            fs::create_dir_all(&outpath)?;
            continue;
        }
        if !etype.is_file() {
            return Err(SyncError::PathTraversal(format!(
                "{} (non-regular entry)",
                raw_name.display()
            )));
        }

        // Cap per-entry and total extracted bytes regardless of the
        // advertised header size (headers can lie).
        let remaining = MAX_TOTAL_UNCOMPRESSED_BYTES.saturating_sub(total_uncompressed);
        let per_entry_cap = MAX_PER_ENTRY_BYTES.min(remaining);
        if per_entry_cap == 0 {
            return Err(SyncError::ZipTooLarge {
                cap: MAX_TOTAL_UNCOMPRESSED_BYTES,
            });
        }
        let mut out = File::create(&outpath)?;
        let mut reader = (&mut entry).take(per_entry_cap + 1);
        let written = io::copy(&mut reader, &mut out)?;
        if written > per_entry_cap {
            return Err(SyncError::ZipEntryTooLarge {
                name: raw_name.display().to_string(),
                cap: MAX_PER_ENTRY_BYTES,
            });
        }
        total_uncompressed = total_uncompressed.saturating_add(written);
        if total_uncompressed > MAX_TOTAL_UNCOMPRESSED_BYTES {
            return Err(SyncError::ZipTooLarge {
                cap: MAX_TOTAL_UNCOMPRESSED_BYTES,
            });
        }
        files += 1;
    }

    Ok(ExtractStats { files })
}

fn strip_backup_suffix(bak: &Path, skills_root: &Path) -> Option<PathBuf> {
    // `bak` looks like `<skills_root>/.bak-<safe_id>-<uuid>`. Derive
    // the original install directory so we can rename back on failure.
    let name = bak.file_name()?.to_str()?;
    let rest = name.strip_prefix(".bak-")?;
    // Split on the *last* `-` to isolate the uuid suffix.
    let safe_id = rest.rsplit_once('-').map(|(s, _)| s)?;
    if safe_id.is_empty() {
        return None;
    }
    Some(skills_root.join(safe_id))
}

fn sha256_file(p: &Path) -> io::Result<String> {
    let mut f = File::open(p)?;
    let mut hasher = crate::crypto::Sha256Stream::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize_hex())
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

pub(super) fn dir_size(p: &Path) -> io::Result<u64> {
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/skills/sync.rs"
    ));
}
