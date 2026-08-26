//! Local archive install — Phase 2.1.
//!
//! Lays a `.zip`, `.tar`, `.tar.gz`, or `.tgz` archive down at
//! `<engines_dir>/<engine>/<version>/`, then registers the install in
//! `engines.json`. **No network, no version inference**: caller passes
//! `engine` + `version` + path to the archive.
//!
//! ## Atomicity
//!
//! 1. Extract under `<engines_dir>/<engine>/.staging-<rand>/`
//! 2. Validate that the extracted tree contains at least one usable
//!    sub-directory (`bin/` or `lib/`).
//! 3. Atomic-rename the staging dir to `<engine>/<version>/`. Fails if
//!    the destination already exists.
//! 4. Register in `engines.json` and `save()`.
//!
//! On any error we roll back by removing the staging dir.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use chrono::Utc;
use uuid::Uuid;

use super::registry::{EnginesIndex, InstalledVersion, RegistryError};

#[derive(Debug)]
pub struct InstallResult {
    pub install_dir: PathBuf,
    pub files_extracted: usize,
    pub bytes_on_disk: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("registry: {0}")]
    Registry(#[from] RegistryError),
    #[error("archive does not exist: {0}")]
    ArchiveMissing(PathBuf),
    #[error("unsupported archive format: {0} (supported: .zip, .tar, .tar.gz, .tgz)")]
    UnsupportedFormat(String),
    #[error("archive is empty after extraction")]
    EmptyArchive,
    #[error("archive layout invalid: missing bin/ or lib/ directory under {0}")]
    InvalidLayout(PathBuf),
    #[error("destination already exists: {0} (uninstall first or use a different version)")]
    DestinationExists(PathBuf),
    #[error("zip slip detected — entry path escapes destination: {0}")]
    PathTraversal(String),
}

pub fn install_from_archive(
    index: &mut EnginesIndex,
    engine: &str,
    version: &str,
    archive: &Path,
) -> Result<InstallResult, InstallError> {
    if !archive.exists() {
        return Err(InstallError::ArchiveMissing(archive.to_path_buf()));
    }
    let install_dir = super::paths::engine_version_dir(engine, version);
    if install_dir.exists() {
        return Err(InstallError::DestinationExists(install_dir));
    }

    let engine_dir = super::paths::engine_dir(engine);
    fs::create_dir_all(&engine_dir)?;

    let staging = engine_dir.join(format!(".staging-{}", Uuid::new_v4()));
    fs::create_dir_all(&staging)?;

    let extract_result = extract(archive, &staging);
    let extract = match extract_result {
        Ok(e) => e,
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
    };

    if extract.files == 0 {
        let _ = fs::remove_dir_all(&staging);
        return Err(InstallError::EmptyArchive);
    }

    let canonical = match strip_single_wrapper(&staging)? {
        Some(inner) => inner,
        None => staging.clone(),
    };

    if !is_usable_engine_layout(&canonical) {
        let _ = fs::remove_dir_all(&staging);
        return Err(InstallError::InvalidLayout(canonical));
    }

    if canonical != staging {
        fs::rename(&canonical, &install_dir)?;
        let _ = fs::remove_dir_all(&staging);
    } else {
        fs::rename(&staging, &install_dir)?;
    }

    let bytes = dir_size(&install_dir).unwrap_or(0);
    index.record_install(
        engine,
        InstalledVersion {
            version: version.to_string(),
            installed_at: Utc::now(),
            bytes,
            source: format!("local:{}", archive.display()),
            sha256: String::new(),
        },
    )?;
    if index.entry_mut(engine).source.is_empty() {
        index.entry_mut(engine).source = "local".to_string();
    }
    index.save()?;
    Ok(InstallResult {
        install_dir,
        files_extracted: extract.files,
        bytes_on_disk: bytes,
    })
}

struct ExtractStats {
    files: usize,
}

fn extract(archive: &Path, dest: &Path) -> Result<ExtractStats, InstallError> {
    let name = archive
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.ends_with(".zip") {
        extract_zip(archive, dest)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let file = File::open(archive)?;
        let gz = flate2::read::GzDecoder::new(file);
        extract_tar(gz, dest)
    } else if name.ends_with(".tar") {
        let file = File::open(archive)?;
        extract_tar(file, dest)
    } else {
        Err(InstallError::UnsupportedFormat(
            archive
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
        ))
    }
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<ExtractStats, InstallError> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut files = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let raw_name = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => {
                return Err(InstallError::PathTraversal(entry.name().to_string()));
            }
        };
        let outpath = dest.join(&raw_name);
        if !outpath.starts_with(dest) {
            return Err(InstallError::PathTraversal(entry.name().to_string()));
        }
        if entry.is_dir() {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = File::create(&outpath)?;
            io::copy(&mut entry, &mut out)?;
            // Preserve the Unix permission bits from the archive so
            // engine binaries land on disk with their executable bit
            // intact (zip's default mode-0666 -> umask makes them
            // unrunnable). We restrict the mask we honor to the read,
            // write and execute bits — any setuid/setgid/sticky bits
            // are dropped so a malicious archive can't promote
            // privileges through extraction. Symlink bits aren't
            // relevant here: the entry has already been routed to
            // `File::create`.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if let Some(mode) = entry.unix_mode() {
                    let safe = mode & 0o777;
                    if safe != 0 {
                        // Drop the file handle before chmod'ing so the
                        // permission change is visible on next open.
                        drop(out);
                        let perms = fs::Permissions::from_mode(safe);
                        fs::set_permissions(&outpath, perms)?;
                    }
                }
            }
            files += 1;
        }
    }
    Ok(ExtractStats { files })
}

fn extract_tar<R: Read>(reader: R, dest: &Path) -> Result<ExtractStats, InstallError> {
    let mut archive = tar::Archive::new(reader);
    let mut files = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw_path = entry.path()?.to_string_lossy().to_string();
        let entry_type = entry.header().entry_type();
        let unpacked = entry.unpack_in(dest)?;
        if !unpacked {
            return Err(InstallError::PathTraversal(raw_path));
        }
        if entry_type.is_file() {
            files += 1;
        }
    }
    Ok(ExtractStats { files })
}

fn strip_single_wrapper(dir: &Path) -> Result<Option<PathBuf>, InstallError> {
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

fn is_usable_engine_layout(root: &Path) -> bool {
    root.join("bin").is_dir() || root.join("lib").is_dir()
}

fn dir_size(p: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(p)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            total = total.saturating_add(dir_size(&entry.path())?);
        } else if ft.is_file() {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

#[allow(dead_code)]
pub fn sha256_of(path: &Path) -> io::Result<String> {
    use std::io::Read as _;
    let mut f = File::open(path)?;
    let mut hasher = crate::crypto::Sha256Stream::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize_hex())
}

// Re-export so existing `engine_pkg::install_local::Sha256Stream`
// callers (download.rs) keep working without churn. The implementation
// lives in `crate::crypto`.
#[allow(unused_imports)]
pub(super) use crate::crypto::Sha256Stream;

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/engine_pkg/install_local.rs"
    ));
}
