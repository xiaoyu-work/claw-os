//! Shared install pipeline for every extension kind.
//!
//! The rules are identical for Apps, Skills and MCP/adapter packages:
//!
//! 1. Treat the incoming bundle as untrusted bytes. Bound the total
//!    size, the file count, the path depth and every name length.
//! 2. Reject absolute paths, `..`, alternate separators, duplicate and
//!    case-colliding names, symlinks, hardlinks, devices, FIFOs,
//!    sockets and group/world-writable modes.
//! 3. Copy into a private staging directory (mode `0700`) on the
//!    destination filesystem and `fsync` it.
//! 4. Verify the provenance envelope and every file digest **on the
//!    staged copy**, not on the source.
//! 5. Retain the verified artifact in a content-addressed store, then
//!    atomically rename it into the live location and `fsync` the
//!    parent.
//!
//! A live install directory is never merged into: activation is always
//! a whole-directory rename, so a partially written package can never
//! be observed by discovery.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use super::envelope::{validate_tree_path, PackageKind, ENVELOPE_FILE};
use super::fsec;
use super::trust::TrustStore;
use super::verify::{verify_package, ProvenanceError, VerifiedPackage, VerifyOptions};

/// Bounds applied to an untrusted bundle before anything is verified.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_total_bytes: u64,
    pub max_files: usize,
    pub max_path_bytes: usize,
    pub max_name_bytes: usize,
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total_bytes: 256 * 1024 * 1024,
            max_files: 20_000,
            max_path_bytes: super::envelope::MAX_PATH_BYTES,
            max_name_bytes: super::envelope::MAX_NAME_BYTES,
            max_depth: super::envelope::MAX_PATH_DEPTH,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("{path}: {reason}")]
    Io { path: PathBuf, reason: String },
    #[error("bundle rejected at `{path}`: {reason}")]
    UnsafeEntry { path: String, reason: String },
    #[error("bundle exceeds the {limit} limit ({actual})")]
    Limit { limit: &'static str, actual: String },
    #[error(transparent)]
    Provenance(#[from] ProvenanceError),
    #[error("destination `{0}` already exists; pass --force to replace it")]
    DestinationExists(PathBuf),
}

impl InstallError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "io.install",
            Self::UnsafeEntry { .. } => "input.unsafe_bundle",
            Self::Limit { .. } => "limit.bundle",
            Self::Provenance(e) => e.code(),
            Self::DestinationExists(_) => "resource.exists",
        }
    }
}

fn io_err(path: &Path, e: impl std::fmt::Display) -> InstallError {
    InstallError::Io {
        path: path.to_path_buf(),
        reason: e.to_string(),
    }
}

/// Walk an already-extracted tree and reject every shape that must
/// never reach a live install. Runs on the staging copy, so it also
/// catches anything an archive extractor let through.
pub fn assert_safe_tree(root: &Path, limits: &Limits) -> Result<(), InstallError> {
    let mut folded: BTreeMap<String, String> = BTreeMap::new();
    let mut count = 0usize;
    let mut total = 0u64;
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        if depth > limits.max_depth {
            return Err(InstallError::Limit {
                limit: "path depth",
                actual: format!("{depth}"),
            });
        }
        let entries = std::fs::read_dir(&dir).map_err(|e| io_err(&dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io_err(&dir, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if depth == 0 && name == ENVELOPE_FILE {
                continue;
            }
            if name.len() > limits.max_name_bytes {
                return Err(InstallError::UnsafeEntry {
                    path: rel,
                    reason: format!("name exceeds {} bytes", limits.max_name_bytes),
                });
            }
            if rel.len() > limits.max_path_bytes {
                return Err(InstallError::UnsafeEntry {
                    path: rel,
                    reason: format!("path exceeds {} bytes", limits.max_path_bytes),
                });
            }
            validate_tree_path(&rel).map_err(|reason| InstallError::UnsafeEntry {
                path: rel.clone(),
                reason,
            })?;
            let folded_key = rel.to_ascii_lowercase();
            if let Some(other) = folded.insert(folded_key, rel.clone()) {
                return Err(InstallError::UnsafeEntry {
                    path: rel,
                    reason: format!("case-collides with `{other}`"),
                });
            }
            let meta = fsec::lstat(&entry.path()).map_err(|e| io_err(&entry.path(), e))?;
            if meta.is_symlink {
                return Err(InstallError::UnsafeEntry {
                    path: rel,
                    reason: "symlink".to_string(),
                });
            }
            if meta.is_group_or_world_writable() {
                return Err(InstallError::UnsafeEntry {
                    path: rel,
                    reason: format!("mode {:o} is group- or world-writable", meta.mode),
                });
            }
            if meta.is_dir {
                stack.push((entry.path(), rel, depth + 1));
                continue;
            }
            if !meta.is_file {
                return Err(InstallError::UnsafeEntry {
                    path: rel,
                    reason: "device, FIFO or socket node".to_string(),
                });
            }
            if meta.nlink != 1 {
                return Err(InstallError::UnsafeEntry {
                    path: rel,
                    reason: format!("hard link ({} links)", meta.nlink),
                });
            }
            count += 1;
            if count > limits.max_files {
                return Err(InstallError::Limit {
                    limit: "file count",
                    actual: format!("{count}"),
                });
            }
            total = total.saturating_add(meta.size);
            if total > limits.max_total_bytes {
                return Err(InstallError::Limit {
                    limit: "total bytes",
                    actual: format!("{total}"),
                });
            }
        }
    }
    Ok(())
}

/// A private staging directory that removes itself on drop unless it
/// was published.
#[derive(Debug)]
pub struct Staging {
    path: PathBuf,
    published: bool,
}

impl Staging {
    /// Create `<parent>/.<label>.staging-<uuid>` with mode `0700`.
    pub fn create(parent: &Path, label: &str) -> Result<Self, InstallError> {
        std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        let name = format!(
            ".{label}.staging-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        );
        let path = parent.join(name);
        // Create the directory already private: a `create_dir` at the
        // process umask followed by a `chmod` leaves a window in which
        // another user on the machine can open the staging tree and
        // watch — or substitute — a package mid-install.
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::PermissionsExt;
            let c = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io_err(&path, "path contains a NUL byte"))?;
            if unsafe { libc::mkdir(c.as_ptr(), 0o700) } != 0 {
                return Err(io_err(&path, std::io::Error::last_os_error()));
            }
            // `mkdir` is masked by the umask, so assert the mode we need.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| io_err(&path, e))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir(&path).map_err(|e| io_err(&path, e))?;
        }
        Ok(Self {
            path,
            published: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn release(mut self) -> PathBuf {
        self.published = true;
        self.path.clone()
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Copy an untrusted source tree into `staging`, applying the bounds
/// and rejecting unsafe nodes as it goes.
pub fn copy_bundle(source: &Path, staging: &Path, limits: &Limits) -> Result<(), InstallError> {
    let mut count = 0usize;
    let mut total = 0u64;
    copy_dir(source, staging, limits, 0, &mut count, &mut total)?;
    assert_safe_tree(staging, limits)
}

fn copy_dir(
    src: &Path,
    dst: &Path,
    limits: &Limits,
    depth: usize,
    count: &mut usize,
    total: &mut u64,
) -> Result<(), InstallError> {
    if depth > limits.max_depth {
        return Err(InstallError::Limit {
            limit: "path depth",
            actual: format!("{depth}"),
        });
    }
    std::fs::create_dir_all(dst).map_err(|e| io_err(dst, e))?;
    for entry in std::fs::read_dir(src).map_err(|e| io_err(src, e))? {
        let entry = entry.map_err(|e| io_err(src, e))?;
        let name = entry.file_name();
        let from = entry.path();
        let to = dst.join(&name);
        let meta = fsec::lstat(&from).map_err(|e| io_err(&from, e))?;
        if meta.is_symlink {
            return Err(InstallError::UnsafeEntry {
                path: from.display().to_string(),
                reason: "symlink".to_string(),
            });
        }
        if meta.is_dir {
            copy_dir(&from, &to, limits, depth + 1, count, total)?;
            continue;
        }
        if !meta.is_file {
            return Err(InstallError::UnsafeEntry {
                path: from.display().to_string(),
                reason: "device, FIFO or socket node".to_string(),
            });
        }
        *count += 1;
        if *count > limits.max_files {
            return Err(InstallError::Limit {
                limit: "file count",
                actual: format!("{count}"),
            });
        }
        *total = total.saturating_add(meta.size);
        if *total > limits.max_total_bytes {
            return Err(InstallError::Limit {
                limit: "total bytes",
                actual: format!("{total}"),
            });
        }
        std::fs::copy(&from, &to).map_err(|e| io_err(&from, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Strip group/world write on copy so a lax source tree
            // cannot produce a writable installed package.
            let mode = meta.mode & !0o022;
            let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

/// `fsync` every regular file and directory under `root`.
pub fn sync_tree(root: &Path) -> io::Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                std::fs::File::open(&path)?.sync_all()?;
            }
        }
        fsec::sync_dir(&dir)?;
    }
    Ok(())
}

/// One staged, verified package awaiting publication.
#[derive(Debug)]
pub struct StagedPackage {
    staging: Staging,
    pub verified: VerifiedPackage,
}

impl StagedPackage {
    pub fn path(&self) -> &Path {
        self.staging.path()
    }
}

/// Stage an untrusted directory and verify it before it can be seen by
/// anything else.
pub fn stage_directory(
    source: &Path,
    dest: &Path,
    kind: PackageKind,
    expect_id: Option<&str>,
    trust: &TrustStore,
    limits: &Limits,
) -> Result<StagedPackage, InstallError> {
    let parent = dest.parent().ok_or_else(|| InstallError::Io {
        path: dest.to_path_buf(),
        reason: "destination has no parent directory".to_string(),
    })?;
    let label = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("package");
    let staging = Staging::create(parent, label)?;
    copy_bundle(source, staging.path(), limits)?;
    sync_tree(staging.path()).map_err(|e| io_err(staging.path(), e))?;
    verify_staged(staging, kind, expect_id, trust, limits)
}

/// Verify an already-populated staging directory. Skill archives
/// extract in place and then call straight into this.
pub fn verify_staged(
    staging: Staging,
    kind: PackageKind,
    expect_id: Option<&str>,
    trust: &TrustStore,
    limits: &Limits,
) -> Result<StagedPackage, InstallError> {
    assert_safe_tree(staging.path(), limits)?;
    let mut options = VerifyOptions::new(kind).signature_only();
    options.max_bytes = limits.max_total_bytes;
    if let Some(id) = expect_id {
        options = options.expect_id(id);
    }
    let verified = verify_package(staging.path(), &options, trust)?;
    Ok(StagedPackage { staging, verified })
}

/// Result of publishing a staged package.
#[derive(Debug, Clone)]
pub struct Published {
    pub live_dir: PathBuf,
    pub artifact_dir: PathBuf,
    pub content_digest: String,
    pub replaced: bool,
}

/// Content-addressed retention directory for one verified artifact.
pub fn artifact_dir(kind: PackageKind, id: &str, content_digest: &str) -> PathBuf {
    let short: String = content_digest
        .strip_prefix("sha256:")
        .unwrap_or(content_digest)
        .chars()
        .take(32)
        .collect();
    crate::paths::provenance_artifacts_dir()
        .join(kind.as_str())
        .join(id)
        .join(short)
}

/// Copy the verified staged tree into the immutable artifact store and
/// then atomically activate it at `dest`.
///
/// The artifact copy is what a later rollback re-verifies, so a
/// rollback can only ever land on content that passed verification.
pub fn publish(
    staged: StagedPackage,
    dest: &Path,
    force: bool,
    limits: &Limits,
) -> Result<Published, InstallError> {
    let StagedPackage { staging, verified } = staged;
    let live_exists = std::fs::symlink_metadata(dest).is_ok();
    if live_exists && !force {
        return Err(InstallError::DestinationExists(dest.to_path_buf()));
    }

    let artifact = artifact_dir(verified.kind(), verified.id(), verified.content_digest());
    if std::fs::symlink_metadata(&artifact).is_err() {
        let artifact_parent = artifact.parent().ok_or_else(|| InstallError::Io {
            path: artifact.clone(),
            reason: "artifact path has no parent".to_string(),
        })?;
        let artifact_staging = Staging::create(artifact_parent, "artifact")?;
        copy_bundle(staging.path(), artifact_staging.path(), limits)?;
        // The envelope is skipped by `copy_bundle`; carry it across so
        // the retained artifact is independently verifiable.
        let envelope = staging.path().join(ENVELOPE_FILE);
        if envelope.is_file() {
            std::fs::copy(&envelope, artifact_staging.path().join(ENVELOPE_FILE))
                .map_err(|e| io_err(&envelope, e))?;
        }
        sync_tree(artifact_staging.path()).map_err(|e| io_err(artifact_staging.path(), e))?;
        let from = artifact_staging.release();
        std::fs::rename(&from, &artifact).map_err(|e| {
            let _ = std::fs::remove_dir_all(&from);
            io_err(&artifact, e)
        })?;
        fsec::sync_dir(artifact_parent).map_err(|e| io_err(artifact_parent, e))?;
    }

    let parent = dest.parent().ok_or_else(|| InstallError::Io {
        path: dest.to_path_buf(),
        reason: "destination has no parent directory".to_string(),
    })?;
    let backup = live_exists.then(|| {
        parent.join(format!(
            ".{}.replaced-{}",
            dest.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("package"),
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ))
    });
    if let Some(backup) = &backup {
        std::fs::rename(dest, backup).map_err(|e| io_err(dest, e))?;
    }
    let from = staging.release();
    match std::fs::rename(&from, dest) {
        Ok(()) => {}
        Err(e) => {
            let _ = std::fs::remove_dir_all(&from);
            if let Some(backup) = &backup {
                let _ = std::fs::rename(backup, dest);
            }
            return Err(io_err(dest, e));
        }
    }
    fsec::sync_dir(parent).map_err(|e| io_err(parent, e))?;
    if let Some(backup) = &backup {
        let _ = std::fs::remove_dir_all(backup);
    }

    Ok(Published {
        live_dir: dest.to_path_buf(),
        artifact_dir: artifact,
        content_digest: verified.content_digest().to_string(),
        replaced: live_exists,
    })
}

/// Re-activate a previously verified artifact.
///
/// The artifact is verified again before activation, so a rollback
/// target that was revoked or tampered with in the store is refused.
pub fn rollback(
    kind: PackageKind,
    id: &str,
    content_digest: &str,
    dest: &Path,
    trust: &TrustStore,
    limits: &Limits,
) -> Result<Published, InstallError> {
    let artifact = artifact_dir(kind, id, content_digest);
    if std::fs::symlink_metadata(&artifact).is_err() {
        return Err(InstallError::Io {
            path: artifact,
            reason: "no retained artifact with that content digest".to_string(),
        });
    }
    let parent = dest.parent().ok_or_else(|| InstallError::Io {
        path: dest.to_path_buf(),
        reason: "destination has no parent directory".to_string(),
    })?;
    let staging = Staging::create(parent, id)?;
    copy_bundle(&artifact, staging.path(), limits)?;
    let envelope = artifact.join(ENVELOPE_FILE);
    if envelope.is_file() {
        std::fs::copy(&envelope, staging.path().join(ENVELOPE_FILE))
            .map_err(|e| io_err(&envelope, e))?;
    }
    sync_tree(staging.path()).map_err(|e| io_err(staging.path(), e))?;
    let staged = verify_staged(staging, kind, Some(id), trust, limits)?;
    if staged.verified.content_digest() != content_digest {
        return Err(InstallError::Io {
            path: artifact,
            reason: "retained artifact digest changed".to_string(),
        });
    }
    publish(staged, dest, true, limits)
}

/// List retained, still-verifiable artifacts for one package.
pub fn list_artifacts(kind: PackageKind, id: &str) -> Vec<PathBuf> {
    let root = crate::paths::provenance_artifacts_dir()
        .join(kind.as_str())
        .join(id);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/install.rs"
    ));
}
