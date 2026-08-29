//! The unprivileged runtime projection of the security floor.
//!
//! # Why a projection at all
//!
//! The authoritative floor lives inside `/var/lib/cos/security`, which
//! is `0700 root:root` because it holds the recovery authorizations and
//! the generation history that make rollback detectable. Widening that
//! tree so an ordinary `cos` process could read it would trade a real
//! secret for a convenience.
//!
//! Instead the authoritative commit publishes a **minimal, read-only
//! projection** into its own root-owned directory
//! ([`RUNTIME_STATE_DIR`](super::RUNTIME_STATE_DIR)):
//!
//! ```text
//! /var/lib/cos-security/            0755 root:root   traversable
//! └── runtime-floor.json            0644 root:root   world readable
//! ```
//!
//! It carries exactly what an unprivileged binary needs to refuse to
//! run — security epoch, ABI, protocol epochs, per-package versions and
//! component digests — plus the generation and digest of the
//! authoritative floor it was derived from. It carries **no** recovery
//! authorization, no history, and no trust material.
//!
//! # Ordering and failure
//!
//! The projection is written only *after* the authoritative floor has
//! been committed, and by the same atomic recipe: fresh temporary,
//! `fsync`, `rename`, `fsync` of the parent directory. So the
//! projection is never ahead of the authority.
//!
//! If the projection cannot be written after the private commit
//! succeeded, that is reported as an **indeterminate** state rather
//! than a success: the private floor has already moved forward
//! (monotonically, so nothing is weakened), but the machine's
//! unprivileged view has not. `claw-security-floor project` — which
//! `clawd` also runs at startup — repairs it, and repeating the commit
//! repairs it too.
//!
//! # Ratchet
//!
//! Presence of the directory is the "this machine is protected"
//! signal, and only root can create it because `/var/lib` is
//! root-owned. Once it exists, a missing, corrupt, wrongly owned,
//! wrongly moded, symlinked or hardlinked projection fails closed for
//! every unprivileged Claw OS binary. `clawd` additionally compares the
//! projection against the private floor, so a *stale* projection is
//! caught by the one process that can see both.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Map, Value};

use crate::provenance::fsec;

use super::canonical;
use super::floor::{ComponentFloor, Floor};
use super::manifest::require_digest;

pub const FORMAT: &str = "claw.security-runtime-floor/v1";

const MAX_PROJECTION_BYTES: u64 = 1024 * 1024;

/// Marker left in the private tree when a projection could not be
/// published after the authoritative commit.
pub const PENDING_MARKER: &str = "runtime-projection.pending";

#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("runtime security floor is insecure: {0}")]
    Insecure(String),
    #[error("runtime security floor is unreadable: {0}")]
    Unreadable(String),
    #[error("runtime security floor is corrupt: {0}")]
    Corrupt(String),
    #[error(
        "the security floor was committed but its unprivileged runtime view could not be \
         published ({0}); the update is INDETERMINATE. Re-run the package configuration, or \
         repair it with `claw-security-floor project`."
    )]
    Indeterminate(String),
}

/// What an unprivileged Claw OS process is allowed to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFloor {
    pub security_epoch: u64,
    pub abi: u32,
    pub protocols: BTreeMap<String, u32>,
    pub packages: BTreeMap<String, (u64, String)>,
    pub components: BTreeMap<String, ComponentFloor>,
    pub floor_generation: u64,
    pub floor_sha256: String,
    pub updated_at: DateTime<Utc>,
}

impl RuntimeFloor {
    fn from_floor(floor: &Floor) -> Self {
        Self {
            security_epoch: floor.security_epoch,
            abi: floor.abi,
            protocols: floor.protocols.clone(),
            packages: floor
                .packages
                .iter()
                .map(|(name, entry)| (name.clone(), (entry.security_epoch, entry.version.clone())))
                .collect(),
            components: floor.components.clone(),
            floor_generation: floor.generation,
            floor_sha256: floor.digest.clone(),
            updated_at: floor.updated_at,
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut packages = Map::new();
        for (name, (epoch, version)) in &self.packages {
            packages.insert(
                name.clone(),
                json!({ "security_epoch": epoch, "version": version }),
            );
        }
        let mut components = Map::new();
        for (name, entry) in &self.components {
            components.insert(
                name.clone(),
                json!({
                    "path": entry.path,
                    "sha256": entry.sha256,
                    "size": entry.size,
                }),
            );
        }
        let mut protocols = Map::new();
        for (name, epoch) in &self.protocols {
            protocols.insert(name.clone(), json!(epoch));
        }
        canonical::to_bytes(&json!({
            "abi": self.abi,
            "components": Value::Object(components),
            "floor_generation": self.floor_generation,
            "floor_sha256": self.floor_sha256,
            "format": FORMAT,
            "packages": Value::Object(packages),
            "protocols": Value::Object(protocols),
            "security_epoch": self.security_epoch,
            "updated_at": self.updated_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        }))
        .unwrap_or_default()
    }

    fn parse(bytes: &[u8]) -> Result<Self, ProjectionError> {
        let value = canonical::parse_canonical(bytes).map_err(ProjectionError::Corrupt)?;
        let object = value.as_object().ok_or_else(|| {
            ProjectionError::Corrupt("runtime floor is not an object".to_string())
        })?;
        if object.get("format").and_then(Value::as_str) != Some(FORMAT) {
            return Err(ProjectionError::Corrupt(
                "runtime floor has an unknown format".to_string(),
            ));
        }
        let number = |key: &str| -> Result<u64, ProjectionError> {
            object.get(key).and_then(Value::as_u64).ok_or_else(|| {
                ProjectionError::Corrupt(format!("runtime floor field `{key}` is not a number"))
            })
        };
        let text = |key: &str| -> Result<String, ProjectionError> {
            object
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    ProjectionError::Corrupt(format!("runtime floor field `{key}` is missing"))
                })
        };

        let floor_sha256 = text("floor_sha256")?;
        require_digest(&floor_sha256).map_err(ProjectionError::Corrupt)?;

        let mut protocols = BTreeMap::new();
        for (name, raw) in object
            .get("protocols")
            .and_then(Value::as_object)
            .ok_or_else(|| ProjectionError::Corrupt("runtime floor has no protocols".to_string()))?
        {
            let epoch = raw
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    ProjectionError::Corrupt("runtime protocol epoch is invalid".to_string())
                })?;
            protocols.insert(name.clone(), epoch);
        }

        let mut packages = BTreeMap::new();
        for (name, raw) in object
            .get("packages")
            .and_then(Value::as_object)
            .ok_or_else(|| ProjectionError::Corrupt("runtime floor has no packages".to_string()))?
        {
            let entry = raw.as_object().ok_or_else(|| {
                ProjectionError::Corrupt("runtime package entry is not an object".to_string())
            })?;
            let version = entry
                .get("version")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ProjectionError::Corrupt("runtime package version is missing".to_string())
                })?
                .to_string();
            let epoch = entry
                .get("security_epoch")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    ProjectionError::Corrupt("runtime package epoch is missing".to_string())
                })?;
            packages.insert(name.clone(), (epoch, version));
        }

        let mut components = BTreeMap::new();
        for (name, raw) in object
            .get("components")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ProjectionError::Corrupt("runtime floor has no components".to_string())
            })?
        {
            let entry = raw.as_object().ok_or_else(|| {
                ProjectionError::Corrupt("runtime component entry is not an object".to_string())
            })?;
            let sha256 = entry
                .get("sha256")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ProjectionError::Corrupt("runtime component digest is missing".to_string())
                })?
                .to_string();
            require_digest(&sha256).map_err(ProjectionError::Corrupt)?;
            components.insert(
                name.clone(),
                ComponentFloor {
                    path: entry
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    sha256,
                    size: entry
                        .get("size")
                        .and_then(Value::as_u64)
                        .unwrap_or_default(),
                    dev: 0,
                    ino: 0,
                },
            );
        }

        Ok(Self {
            security_epoch: number("security_epoch")?,
            abi: u32::try_from(number("abi")?)
                .map_err(|_| ProjectionError::Corrupt("runtime abi is out of range".to_string()))?,
            protocols,
            packages,
            components,
            floor_generation: number("floor_generation")?,
            floor_sha256,
            updated_at: DateTime::parse_from_rfc3339(&text("updated_at")?)
                .map(|parsed| parsed.with_timezone(&Utc))
                .map_err(|_| {
                    ProjectionError::Corrupt("runtime floor timestamp is invalid".to_string())
                })?,
        })
    }

    /// Does this projection describe exactly `floor`?
    ///
    /// Byte equality against a freshly derived projection, not just a
    /// generation comparison: a projection whose epoch, protocols or
    /// component digests were edited while its generation was left
    /// alone must not look current to the broker that owns it.
    pub fn matches(&self, floor: &Floor) -> bool {
        self.floor_generation == floor.generation
            && self.floor_sha256 == floor.digest
            && self.to_bytes() == Self::from_floor(floor).to_bytes()
    }
}

/// Reader/writer for the unprivileged runtime view.
#[derive(Debug, Clone)]
pub struct ProjectionStore {
    dir: PathBuf,
    allowed_uids: Vec<u32>,
}

impl ProjectionStore {
    pub fn system() -> Self {
        Self {
            dir: PathBuf::from(super::RUNTIME_STATE_DIR),
            allowed_uids: vec![0],
        }
    }

    pub fn under_root(root: &Path) -> Self {
        let mut allowed_uids = vec![0];
        let effective = fsec::effective_uid();
        if effective != 0 {
            allowed_uids.push(effective);
        }
        Self {
            dir: super::signature::joined(root, super::RUNTIME_STATE_DIR),
            allowed_uids,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(super::RUNTIME_FLOOR_FILE)
    }

    /// `true` once this machine has ever published a runtime view. Only
    /// root can make this true, because `/var/lib` is root-owned.
    pub fn is_established(&self) -> bool {
        matches!(fsec::lstat(&self.dir), Ok(meta) if meta.is_dir && !meta.is_symlink)
    }

    /// Read and validate the projection.
    ///
    /// `Ok(None)` only when this machine has never been protected.
    pub fn load(&self) -> Result<Option<RuntimeFloor>, ProjectionError> {
        if !self.is_established() {
            return Ok(None);
        }
        let meta = fsec::require_secure_location(&self.dir, &self.allowed_uids)
            .map_err(|error| ProjectionError::Insecure(error.to_string()))?;
        if !meta.is_dir {
            return Err(ProjectionError::Insecure(format!(
                "{} is not a directory",
                self.dir.display()
            )));
        }
        let handle = fsec::DirHandle::open(&self.dir).map_err(|error| {
            ProjectionError::Unreadable(format!("{}: {error}", self.dir.display()))
        })?;
        let file = handle
            .open_file(super::RUNTIME_FLOOR_FILE)
            .map_err(|error| {
                ProjectionError::Unreadable(format!(
                    "{}: {error}. This system has recorded update-security state; refusing to run \
                 without it.",
                    self.path().display()
                ))
            })?;
        let file_meta = file.meta();
        if !file_meta.is_file {
            return Err(ProjectionError::Insecure(
                "the runtime security floor is not a regular file".to_string(),
            ));
        }
        if file_meta.nlink != 1 {
            return Err(ProjectionError::Insecure(format!(
                "the runtime security floor has {} links",
                file_meta.nlink
            )));
        }
        if !self.allowed_uids.contains(&file_meta.uid) {
            return Err(ProjectionError::Insecure(format!(
                "the runtime security floor is owned by uid {} rather than root",
                file_meta.uid
            )));
        }
        if file_meta.is_group_or_world_writable() {
            return Err(ProjectionError::Insecure(
                "the runtime security floor is group- or world-writable".to_string(),
            ));
        }
        if file_meta.size > MAX_PROJECTION_BYTES {
            return Err(ProjectionError::Corrupt(
                "the runtime security floor is too large".to_string(),
            ));
        }
        let bytes = file.read_bounded(MAX_PROJECTION_BYTES).map_err(|error| {
            ProjectionError::Unreadable(format!("{}: {error}", self.path().display()))
        })?;
        RuntimeFloor::parse(&bytes).map(Some)
    }

    /// Publish `floor`'s projection. Only ever called after the
    /// authoritative floor has been committed.
    pub fn publish(&self, floor: &Floor) -> Result<(), ProjectionError> {
        let projection = RuntimeFloor::from_floor(floor);
        create_dir(&self.dir, 0o755)?;
        let bytes = projection.to_bytes();
        let temp = self.dir.join(format!(
            "{}.new.{}",
            super::RUNTIME_FLOOR_FILE,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&temp);
        write_new_file(&temp, &bytes, 0o644).map_err(|error| {
            ProjectionError::Indeterminate(format!("{}: {error}", temp.display()))
        })?;
        std::fs::rename(&temp, self.path()).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            ProjectionError::Indeterminate(format!("failed to publish: {error}"))
        })?;
        fsec::sync_dir(&self.dir)
            .map_err(|error| ProjectionError::Indeterminate(format!("failed to sync: {error}")))
    }
}

#[cfg(unix)]
fn create_dir(path: &Path, mode: u32) -> Result<(), ProjectionError> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    match fsec::lstat(path) {
        Ok(meta) => {
            if meta.is_symlink || !meta.is_dir {
                return Err(ProjectionError::Insecure(format!(
                    "{} exists and is not a directory",
                    path.display()
                )));
            }
            if meta.mode & 0o7777 != mode {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(
                    |error| ProjectionError::Indeterminate(format!("{}: {error}", path.display())),
                )?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    ProjectionError::Indeterminate(format!("{}: {error}", parent.display()))
                })?;
            }
            std::fs::DirBuilder::new()
                .mode(mode)
                .create(path)
                .map_err(|error| {
                    ProjectionError::Indeterminate(format!("{}: {error}", path.display()))
                })?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|error| {
                ProjectionError::Indeterminate(format!("{}: {error}", path.display()))
            })
        }
        Err(error) => Err(ProjectionError::Unreadable(format!(
            "{}: {error}",
            path.display()
        ))),
    }
}

#[cfg(not(unix))]
fn create_dir(_path: &Path, _mode: u32) -> Result<(), ProjectionError> {
    Err(ProjectionError::Insecure(
        "the security floor requires a Unix host".to_string(),
    ))
}

#[cfg(unix)]
fn write_new_file(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    // `open(2)` applies the caller's umask, and every Claw OS binary
    // that writes this file runs with a private one. The projection has
    // to be world readable — that is its whole purpose — so the mode is
    // set explicitly rather than requested.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn write_new_file(_path: &Path, _bytes: &[u8], _mode: u32) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "the security floor requires a Unix host",
    ))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/update/projection.rs"
    ));
}
