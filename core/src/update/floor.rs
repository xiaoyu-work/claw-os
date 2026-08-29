//! The durable local security floor.
//!
//! # Shape
//!
//! ```text
//! /var/lib/cos/security/            0700 root:root  authoritative, private
//! ├── floor.json                    0600 root:root  current generation
//! ├── history.jsonl                 0600 root:root  hash-chained generations
//! └── recovery/                     0700 root:root  one-use authorizations
//! ```
//!
//! The whole tree is private because it holds the recovery
//! authorizations and the generation history. Unprivileged Claw OS
//! processes never read it: the commit publishes a minimal read-only
//! view through [`super::projection`] instead.
//!
//! Integrity comes from ownership — the directory and every ancestor
//! must be a root-owned, non-symlink directory with no group or world
//! write bit, and each state file must be a root-owned regular file
//! with `nlink == 1`, so no unprivileged user can pre-create, hardlink
//! or redirect it.
//!
//! # Rollback detection
//!
//! Every commit writes `floor.json` (rename, then `fsync` of the
//! parent directory) and then appends one line to `history.jsonl`
//! carrying the new generation number, the SHA-256 of the new
//! `floor.json`, and the SHA-256 of the previous one. The two files
//! therefore pin each other:
//!
//! * restoring an older `floor.json` alone leaves its generation
//!   *behind* the history and is refused;
//! * truncating `history.jsonl` alone leaves the history behind the
//!   floor by more than the one generation a crash can explain, and is
//!   refused;
//! * a crash between the two writes leaves the floor exactly one
//!   generation ahead with a matching chain link — the only forward
//!   discrepancy that is accepted, and it is repaired on the next
//!   commit.
//!
//! Restoring *both* files together — a whole-filesystem or
//! whole-state restore performed by local root — is not detectable by
//! state that lives on the same filesystem. That limit is documented
//! rather than papered over.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Map, Value};

use crate::provenance::fsec;

use super::canonical;
use super::debver;
use super::manifest::{require_digest, Manifest};

pub const FORMAT: &str = "claw.security-floor/v1";
pub const HISTORY_FORMAT: &str = "claw.security-floor-history/v1";

const FLOOR_FILE: &str = "floor.json";
const HISTORY_FILE: &str = "history.jsonl";
const RECOVERY_DIR: &str = "recovery";
/// Serializes commits. Carries no state: only the flock on its
/// descriptor matters, so losing the file is harmless.
const LOCK_FILE: &str = "commit.lock";

/// Bound on state read from disk. A floor tracks a handful of packages
/// and components; anything larger is a corruption or an attempt to
/// make the parser work.
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const MAX_HISTORY_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FloorError {
    #[error("security floor state is insecure: {0}")]
    Insecure(String),
    #[error("security floor state is unreadable: {0}")]
    Unreadable(String),
    #[error("security floor state is corrupt: {0}")]
    Corrupt(String),
    #[error("security floor rollback detected: {0}")]
    Rollback(String),
    #[error("security floor state could not be written: {0}")]
    Write(String),
    #[error("security floor changed underneath this commit: {0}")]
    Conflict(String),
}

/// The floor recorded for one package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFloor {
    pub version: String,
    pub security_epoch: u64,
    pub abi: u32,
    pub manifest_sha256: String,
}

/// Content identity of one installed component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentFloor {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub dev: u64,
    pub ino: u64,
}

/// One generation of the floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Floor {
    pub generation: u64,
    pub security_epoch: u64,
    pub abi: u32,
    pub suite: String,
    pub repo_component: String,
    pub protocols: BTreeMap<String, u32>,
    pub packages: BTreeMap<String, PackageFloor>,
    pub components: BTreeMap<String, ComponentFloor>,
    pub revoked_digests: BTreeSet<String>,
    pub trusted_keys: BTreeSet<String>,
    pub updated_at: DateTime<Utc>,
    pub previous_sha256: Option<String>,
    /// SHA-256 of this generation's canonical bytes.
    pub digest: String,
}

/// Why a floor is being advanced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// The ordinary case: the release is at or above the floor, and the
    /// floor may only move forward.
    Forward,
    /// An operator recorded a one-use authorization for this exact
    /// release. The floor follows the release down — including its
    /// epoch and protocol epochs — because otherwise the recovery
    /// install could not actually run.
    AuthorizedRecovery,
}

impl Floor {
    /// The first generation, seeded from an installed release that has
    /// already been authenticated.
    pub fn bootstrap(
        manifest: &Manifest,
        trusted_keys: BTreeSet<String>,
        components: BTreeMap<String, ComponentFloor>,
        now: DateTime<Utc>,
    ) -> Self {
        let mut packages = BTreeMap::new();
        packages.insert(
            manifest.package.clone(),
            PackageFloor {
                version: manifest.version.clone(),
                security_epoch: manifest.security_epoch,
                abi: manifest.abi,
                manifest_sha256: manifest.digest.clone(),
            },
        );
        let mut floor = Self {
            generation: 1,
            security_epoch: manifest.security_epoch,
            abi: manifest.abi,
            suite: manifest.suite.clone(),
            repo_component: manifest.component.clone(),
            protocols: manifest.protocols.clone(),
            packages,
            components,
            revoked_digests: manifest.revoked_digests.clone(),
            trusted_keys,
            updated_at: now,
            previous_sha256: None,
            digest: String::new(),
        };
        floor.digest = floor.compute_digest();
        floor
    }

    /// Advance this floor with an accepted release. Monotonic unless an
    /// operator's one-use authorization explicitly says otherwise, so a
    /// caller cannot use a commit to walk the floor backwards.
    pub fn advanced(
        &self,
        manifest: &Manifest,
        signing_key: Option<&str>,
        components: BTreeMap<String, ComponentFloor>,
        now: DateTime<Utc>,
        advance: Advance,
    ) -> Result<Self, String> {
        let authorized = advance == Advance::AuthorizedRecovery;
        let mut next = self.clone();
        next.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| "floor generation overflowed".to_string())?;
        next.previous_sha256 = Some(self.digest.clone());
        next.updated_at = now;
        next.security_epoch = if authorized {
            manifest.security_epoch
        } else {
            self.security_epoch.max(manifest.security_epoch)
        };
        next.abi = manifest.abi;
        next.suite = manifest.suite.clone();
        next.repo_component = manifest.component.clone();
        for (name, epoch) in &manifest.protocols {
            let recorded = next.protocols.entry(name.clone()).or_insert(*epoch);
            *recorded = if authorized {
                *epoch
            } else {
                (*recorded).max(*epoch)
            };
        }

        let package_floor = PackageFloor {
            version: manifest.version.clone(),
            security_epoch: manifest.security_epoch,
            abi: manifest.abi,
            manifest_sha256: manifest.digest.clone(),
        };
        if let Some(existing) = next.packages.get(&manifest.package) {
            let ordering = debver::compare(&manifest.version, &existing.version)?;
            let regresses = manifest.security_epoch < existing.security_epoch
                || (manifest.security_epoch == existing.security_epoch
                    && ordering == std::cmp::Ordering::Less);
            if regresses && !authorized {
                return Err(format!(
                    "refusing to record {} {} below the recorded floor {}",
                    manifest.package, manifest.version, existing.version
                ));
            }
        }
        next.packages
            .insert(manifest.package.clone(), package_floor);
        next.components.extend(components);
        next.revoked_digests
            .extend(manifest.revoked_digests.iter().cloned());
        for revoked in &manifest.revoked_keys {
            next.trusted_keys.remove(revoked);
        }
        if let Some(key) = signing_key {
            if !manifest.revoked_keys.contains(key) {
                next.trusted_keys.insert(key.to_string());
            }
        }
        next.digest = next.compute_digest();
        Ok(next)
    }

    /// Canonical bytes of this generation, with `digest` excluded: a
    /// document cannot contain its own hash.
    pub fn to_bytes(&self) -> Vec<u8> {
        canonical::to_bytes(&self.to_value()).unwrap_or_default()
    }

    fn compute_digest(&self) -> String {
        crate::crypto::sha256_hex(&self.to_bytes())
    }

    fn to_value(&self) -> Value {
        let mut packages = Map::new();
        for (name, entry) in &self.packages {
            packages.insert(
                name.clone(),
                json!({
                    "abi": entry.abi,
                    "manifest_sha256": entry.manifest_sha256,
                    "security_epoch": entry.security_epoch,
                    "version": entry.version,
                }),
            );
        }
        let mut components = Map::new();
        for (name, entry) in &self.components {
            components.insert(
                name.clone(),
                json!({
                    "dev": entry.dev,
                    "ino": entry.ino,
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
        let mut document = json!({
            "abi": self.abi,
            "components": Value::Object(components),
            "format": FORMAT,
            "generation": self.generation,
            "packages": Value::Object(packages),
            "protocols": Value::Object(protocols),
            "repo_component": self.repo_component,
            "revoked_digests": self.revoked_digests.iter().cloned().collect::<Vec<_>>(),
            "security_epoch": self.security_epoch,
            "suite": self.suite,
            "trusted_keys": self.trusted_keys.iter().cloned().collect::<Vec<_>>(),
            "updated_at": self.updated_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        });
        if let Some(previous) = &self.previous_sha256 {
            if let Some(object) = document.as_object_mut() {
                object.insert("previous_sha256".to_string(), json!(previous));
            }
        }
        document
    }

    fn parse(bytes: &[u8]) -> Result<Self, FloorError> {
        let value = canonical::parse_canonical(bytes).map_err(FloorError::Corrupt)?;
        let object = value
            .as_object()
            .ok_or_else(|| FloorError::Corrupt("floor state is not an object".to_string()))?;
        if object.get("format").and_then(Value::as_str) != Some(FORMAT) {
            return Err(FloorError::Corrupt(
                "floor state has an unknown format".to_string(),
            ));
        }
        let generation = u64_at(object, "generation")?;
        if generation == 0 {
            return Err(FloorError::Corrupt(
                "floor generation must start at 1".to_string(),
            ));
        }
        let security_epoch = u64_at(object, "security_epoch")?;
        let abi = u32::try_from(u64_at(object, "abi")?)
            .map_err(|_| FloorError::Corrupt("abi generation is out of range".to_string()))?;
        let suite = string_at(object, "suite")?;
        let repo_component = string_at(object, "repo_component")?;
        let updated_at = DateTime::parse_from_rfc3339(&string_at(object, "updated_at")?)
            .map(|parsed| parsed.with_timezone(&Utc))
            .map_err(|_| FloorError::Corrupt("floor timestamp is invalid".to_string()))?;

        let mut protocols = BTreeMap::new();
        for (name, raw) in map_at(object, "protocols")? {
            let epoch = raw
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| FloorError::Corrupt("protocol epoch is invalid".to_string()))?;
            protocols.insert(name.clone(), epoch);
        }

        let mut packages = BTreeMap::new();
        for (name, raw) in map_at(object, "packages")? {
            let entry = raw.as_object().ok_or_else(|| {
                FloorError::Corrupt("package floor entry is not an object".to_string())
            })?;
            let version = string_at(entry, "version")?;
            if !debver::is_valid(&version) {
                return Err(FloorError::Corrupt(
                    "package floor version is not a Debian version".to_string(),
                ));
            }
            let manifest_sha256 = string_at(entry, "manifest_sha256")?;
            require_digest(&manifest_sha256).map_err(FloorError::Corrupt)?;
            packages.insert(
                name.clone(),
                PackageFloor {
                    version,
                    security_epoch: u64_at(entry, "security_epoch")?,
                    abi: u32::try_from(u64_at(entry, "abi")?).map_err(|_| {
                        FloorError::Corrupt("package abi is out of range".to_string())
                    })?,
                    manifest_sha256,
                },
            );
        }

        let mut components = BTreeMap::new();
        for (name, raw) in map_at(object, "components")? {
            let entry = raw.as_object().ok_or_else(|| {
                FloorError::Corrupt("component floor entry is not an object".to_string())
            })?;
            let sha256 = string_at(entry, "sha256")?;
            require_digest(&sha256).map_err(FloorError::Corrupt)?;
            components.insert(
                name.clone(),
                ComponentFloor {
                    path: string_at(entry, "path")?,
                    sha256,
                    size: u64_at(entry, "size")?,
                    dev: u64_at(entry, "dev")?,
                    ino: u64_at(entry, "ino")?,
                },
            );
        }

        let mut revoked_digests = BTreeSet::new();
        for entry in array_at(object, "revoked_digests")? {
            let raw = entry.as_str().ok_or_else(|| {
                FloorError::Corrupt("revoked digest entry is not a string".to_string())
            })?;
            require_digest(raw).map_err(FloorError::Corrupt)?;
            revoked_digests.insert(raw.to_string());
        }
        let mut trusted_keys = BTreeSet::new();
        for entry in array_at(object, "trusted_keys")? {
            let raw = entry.as_str().ok_or_else(|| {
                FloorError::Corrupt("trusted key entry is not a string".to_string())
            })?;
            trusted_keys
                .insert(super::signature::normalize_key_id(raw).map_err(FloorError::Corrupt)?);
        }
        let previous_sha256 = match object.get("previous_sha256") {
            None => None,
            Some(raw) => {
                let text = raw.as_str().ok_or_else(|| {
                    FloorError::Corrupt("previous floor digest is not a string".to_string())
                })?;
                require_digest(text).map_err(FloorError::Corrupt)?;
                Some(text.to_string())
            }
        };
        if generation == 1 && previous_sha256.is_some() {
            return Err(FloorError::Corrupt(
                "the first floor generation cannot have a predecessor".to_string(),
            ));
        }
        if generation > 1 && previous_sha256.is_none() {
            return Err(FloorError::Corrupt(
                "floor generation is missing its predecessor digest".to_string(),
            ));
        }

        Ok(Self {
            generation,
            security_epoch,
            abi,
            suite,
            repo_component,
            protocols,
            packages,
            components,
            revoked_digests,
            trusted_keys,
            updated_at,
            previous_sha256,
            digest: crate::crypto::sha256_hex(bytes),
        })
    }
}

/// One appended history line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub generation: u64,
    pub floor_sha256: String,
    pub previous_sha256: Option<String>,
    pub reason: String,
    pub recorded_at: DateTime<Utc>,
}

impl HistoryEntry {
    fn to_bytes(&self) -> Vec<u8> {
        let mut document = json!({
            "floor_sha256": self.floor_sha256,
            "format": HISTORY_FORMAT,
            "generation": self.generation,
            "reason": self.reason,
            "recorded_at": self.recorded_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        });
        if let Some(previous) = &self.previous_sha256 {
            if let Some(object) = document.as_object_mut() {
                object.insert("previous_sha256".to_string(), json!(previous));
            }
        }
        canonical::to_bytes(&document).unwrap_or_default()
    }

    fn parse(line: &[u8]) -> Result<Self, FloorError> {
        let value = canonical::parse_canonical(line).map_err(FloorError::Corrupt)?;
        let object = value
            .as_object()
            .ok_or_else(|| FloorError::Corrupt("history entry is not an object".to_string()))?;
        if object.get("format").and_then(Value::as_str) != Some(HISTORY_FORMAT) {
            return Err(FloorError::Corrupt(
                "history entry has an unknown format".to_string(),
            ));
        }
        let floor_sha256 = string_at(object, "floor_sha256")?;
        require_digest(&floor_sha256).map_err(FloorError::Corrupt)?;
        let previous_sha256 = match object.get("previous_sha256") {
            None => None,
            Some(raw) => {
                let text = raw.as_str().ok_or_else(|| {
                    FloorError::Corrupt("history predecessor is not a string".to_string())
                })?;
                require_digest(text).map_err(FloorError::Corrupt)?;
                Some(text.to_string())
            }
        };
        Ok(Self {
            generation: u64_at(object, "generation")?,
            floor_sha256,
            previous_sha256,
            reason: string_at(object, "reason")?,
            recorded_at: DateTime::parse_from_rfc3339(&string_at(object, "recorded_at")?)
                .map(|parsed| parsed.with_timezone(&Utc))
                .map_err(|_| FloorError::Corrupt("history timestamp is invalid".to_string()))?,
        })
    }
}

/// What `load` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FloorState {
    /// Nothing has ever been recorded: a first install.
    Uninitialized,
    /// A validated floor, and whether its history append still has to
    /// be repaired after an interrupted commit.
    Present {
        floor: Box<Floor>,
        history_repair_needed: bool,
    },
}

/// Reader/writer for the floor directory.
#[derive(Debug, Clone)]
pub struct FloorStore {
    dir: PathBuf,
    allowed_uids: Vec<u32>,
}

impl FloorStore {
    /// The system floor. Compiled-in path, root-owned only.
    pub fn system() -> Self {
        Self {
            dir: PathBuf::from(super::SYSTEM_STATE_DIR),
            allowed_uids: vec![0],
        }
    }

    /// A floor under an alternate root.
    ///
    /// Used by `dpkg`'s own `DPKG_ROOT` installs and by tests. The
    /// effective uid is accepted in addition to root so an
    /// unprivileged test can exercise the same code; under `dpkg` the
    /// effective uid *is* root, so production gains nothing.
    pub fn under_root(root: &Path) -> Self {
        let mut allowed_uids = vec![0];
        let effective = fsec::effective_uid();
        if effective != 0 {
            allowed_uids.push(effective);
        }
        Self {
            dir: super::signature::joined(root, super::SYSTEM_STATE_DIR),
            allowed_uids,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn recovery_dir(&self) -> PathBuf {
        self.dir.join(RECOVERY_DIR)
    }

    pub(crate) fn allowed_uids(&self) -> &[u32] {
        &self.allowed_uids
    }

    /// Read and validate the floor.
    pub fn load(&self) -> Result<FloorState, FloorError> {
        let floor_path = self.dir.join(FLOOR_FILE);
        let history_path = self.dir.join(HISTORY_FILE);
        let floor_exists = exists(&floor_path)?;
        let history_exists = exists(&history_path)?;
        if !floor_exists && !history_exists {
            return Ok(FloorState::Uninitialized);
        }
        if !floor_exists {
            return Err(FloorError::Rollback(
                "the floor state file was removed while its generation history remains".to_string(),
            ));
        }
        if !history_exists {
            return Err(FloorError::Corrupt(
                "the floor generation history is missing".to_string(),
            ));
        }
        self.require_secure_dir()?;

        let floor_bytes = self.read_state_file(FLOOR_FILE, MAX_STATE_BYTES)?;
        let floor = Floor::parse(&floor_bytes)?;
        let history_bytes = self.read_state_file(HISTORY_FILE, MAX_HISTORY_BYTES)?;
        let history = parse_history(&history_bytes)?;

        let Some(last) = history.last() else {
            return Err(FloorError::Corrupt(
                "the floor generation history is empty".to_string(),
            ));
        };
        if floor.generation < last.generation {
            return Err(FloorError::Rollback(format!(
                "floor state is generation {} but the history has already reached {}",
                floor.generation, last.generation
            )));
        }
        if floor.generation == last.generation {
            if floor.digest != last.floor_sha256 {
                return Err(FloorError::Rollback(
                    "floor state does not match the digest recorded for its generation".to_string(),
                ));
            }
            return Ok(FloorState::Present {
                floor: Box::new(floor),
                history_repair_needed: false,
            });
        }
        if floor.generation == last.generation + 1
            && floor.previous_sha256.as_deref() == Some(last.floor_sha256.as_str())
        {
            // A commit that was interrupted between the state rename
            // and the history append. The floor is ahead, never
            // behind, so this is safe to accept and repair.
            return Ok(FloorState::Present {
                floor: Box::new(floor),
                history_repair_needed: true,
            });
        }
        Err(FloorError::Rollback(format!(
            "floor state generation {} does not continue the recorded history at {}",
            floor.generation, last.generation
        )))
    }

    /// Durably record `next`.
    ///
    /// The state file is written to a fresh temporary in the same
    /// directory, `fsync`ed, renamed over the current file, and the
    /// directory itself `fsync`ed before the history line is appended
    /// and `fsync`ed. A crash therefore leaves the floor either
    /// unchanged or exactly one generation ahead of its history.
    ///
    /// Commits are serialized by an exclusive lock on a root-owned,
    /// `O_NOFOLLOW` lock file, and the current state is re-read *under
    /// that lock*. Two `postinst` scripts configuring in parallel would
    /// otherwise both read generation *n*, both build generation *n+1*,
    /// and the second rename would silently erase the first component's
    /// advance — losing a floor raise while both commands reported
    /// success.
    pub fn commit(&self, next: &Floor, reason: &str) -> Result<(), FloorError> {
        self.ensure_dir()?;
        let _guard = self.lock()?;
        self.require_continues_current(next)?;
        self.commit_locked(next, reason)
    }

    /// Take the commit lock. The lock file lives in the private floor
    /// directory, is opened `O_NOFOLLOW` so it cannot be aimed
    /// elsewhere, and is validated like every other file there.
    fn lock(&self) -> Result<CommitLock, FloorError> {
        lock_exclusive(&self.dir.join(LOCK_FILE), 0o600, &self.allowed_uids)
    }

    /// Refuse a commit built from a state that is no longer current.
    ///
    /// `next.previous_sha256` names the floor this advance was computed
    /// from. If the file on disk is something else, another process
    /// committed in between and this advance would drop that work.
    fn require_continues_current(&self, next: &Floor) -> Result<(), FloorError> {
        let current = match self.load()? {
            FloorState::Uninitialized => None,
            FloorState::Present { floor, .. } => Some(floor),
        };
        match (&current, &next.previous_sha256) {
            (None, None) => Ok(()),
            (Some(floor), Some(previous)) if &floor.digest == previous => Ok(()),
            (None, Some(_)) => Err(FloorError::Conflict(
                "the floor this advance continues no longer exists".to_string(),
            )),
            (Some(floor), _) => Err(FloorError::Conflict(format!(
                "generation {} ({}) was committed while this advance was prepared; \
                 re-read the floor and retry",
                floor.generation, floor.digest
            ))),
        }
    }

    fn commit_locked(&self, next: &Floor, reason: &str) -> Result<(), FloorError> {
        let bytes = next.to_bytes();
        if crate::crypto::sha256_hex(&bytes) != next.digest {
            return Err(FloorError::Write(
                "floor digest does not match its encoded bytes".to_string(),
            ));
        }
        let temp = self
            .dir
            .join(format!("{FLOOR_FILE}.new.{}", std::process::id()));
        let _ = std::fs::remove_file(&temp);
        write_new_file(&temp, &bytes, 0o600)
            .map_err(|error| FloorError::Write(format!("{}: {error}", temp.display())))?;
        std::fs::rename(&temp, self.dir.join(FLOOR_FILE)).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            FloorError::Write(format!("failed to publish floor state: {error}"))
        })?;
        fsec::sync_dir(&self.dir).map_err(|error| {
            FloorError::Write(format!("failed to sync floor directory: {error}"))
        })?;
        self.append_history(&HistoryEntry {
            generation: next.generation,
            floor_sha256: next.digest.clone(),
            previous_sha256: next.previous_sha256.clone(),
            reason: sanitize_reason(reason),
            recorded_at: next.updated_at,
        })
    }

    /// Re-append the history line a crash lost. Only ever called for a
    /// floor that is exactly one generation ahead of its history.
    pub fn repair_history(&self, floor: &Floor, reason: &str) -> Result<(), FloorError> {
        self.append_history(&HistoryEntry {
            generation: floor.generation,
            floor_sha256: floor.digest.clone(),
            previous_sha256: floor.previous_sha256.clone(),
            reason: sanitize_reason(reason),
            recorded_at: floor.updated_at,
        })
    }

    fn append_history(&self, entry: &HistoryEntry) -> Result<(), FloorError> {
        let path = self.dir.join(HISTORY_FILE);
        let mut file = append_file(&path, 0o600)
            .map_err(|error| FloorError::Write(format!("{}: {error}", path.display())))?;
        file.write_all(&entry.to_bytes())
            .map_err(|error| FloorError::Write(format!("failed to append history: {error}")))?;
        file.sync_all()
            .map_err(|error| FloorError::Write(format!("failed to sync history: {error}")))?;
        fsec::sync_dir(&self.dir)
            .map_err(|error| FloorError::Write(format!("failed to sync floor directory: {error}")))
    }

    /// Create the state directory tree with the modes enforcement
    /// depends on. The whole tree is private: the unprivileged view is
    /// published separately by [`super::projection`].
    pub fn ensure_dir(&self) -> Result<(), FloorError> {
        create_dir(&self.dir, 0o700)?;
        create_dir(&self.recovery_dir(), 0o700)?;
        self.require_secure_dir()
    }

    fn require_secure_dir(&self) -> Result<(), FloorError> {
        let meta = fsec::require_secure_location(&self.dir, &self.allowed_uids)
            .map_err(|error| FloorError::Insecure(error.to_string()))?;
        if !meta.is_dir {
            return Err(FloorError::Insecure(format!(
                "{} is not a directory",
                self.dir.display()
            )));
        }
        Ok(())
    }

    /// Read one state file through the pinned directory descriptor, so
    /// the bytes parsed are the bytes the security checks were applied
    /// to.
    fn read_state_file(&self, name: &str, cap: u64) -> Result<Vec<u8>, FloorError> {
        let handle = fsec::DirHandle::open(&self.dir)
            .map_err(|error| FloorError::Unreadable(format!("{}: {error}", self.dir.display())))?;
        let file = handle
            .open_file(name)
            .map_err(|error| FloorError::Unreadable(format!("{name}: {error}")))?;
        let meta = file.meta();
        if !meta.is_file {
            return Err(FloorError::Insecure(format!(
                "{name} is not a regular file"
            )));
        }
        if meta.nlink != 1 {
            return Err(FloorError::Insecure(format!(
                "{name} has {} links; state files must not be hardlinked",
                meta.nlink
            )));
        }
        if !self.allowed_uids.contains(&meta.uid) {
            return Err(FloorError::Insecure(format!(
                "{name} is owned by uid {} rather than root",
                meta.uid
            )));
        }
        if meta.is_group_or_world_writable() {
            return Err(FloorError::Insecure(format!(
                "{name} is group- or world-writable"
            )));
        }
        if meta.size > cap {
            return Err(FloorError::Corrupt(format!(
                "{name} is larger than {cap} bytes"
            )));
        }
        file.read_bounded(cap)
            .map_err(|error| FloorError::Unreadable(format!("{name}: {error}")))
    }
}

fn parse_history(bytes: &[u8]) -> Result<Vec<HistoryEntry>, FloorError> {
    let mut entries = Vec::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let entry = HistoryEntry::parse(line)
            .map_err(|error| FloorError::Corrupt(format!("history line {}: {error}", index + 1)))?;
        if let Some(previous) = entries.last() {
            let previous: &HistoryEntry = previous;
            if entry.generation != previous.generation + 1 {
                return Err(FloorError::Rollback(format!(
                    "history jumps from generation {} to {}",
                    previous.generation, entry.generation
                )));
            }
            if entry.previous_sha256.as_deref() != Some(previous.floor_sha256.as_str()) {
                return Err(FloorError::Rollback(
                    "history chain is broken between generations".to_string(),
                ));
            }
        } else {
            if entry.generation != 1 {
                return Err(FloorError::Rollback(format!(
                    "history starts at generation {} rather than 1",
                    entry.generation
                )));
            }
            if entry.previous_sha256.is_some() {
                return Err(FloorError::Corrupt(
                    "the first history entry cannot have a predecessor".to_string(),
                ));
            }
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Hash and stat one installed component.
pub fn measure_component(name: &str, path: &Path) -> Result<ComponentFloor, String> {
    let meta = fsec::lstat(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if meta.is_symlink {
        return Err(format!("{} is a symlink", path.display()));
    }
    if !meta.is_file {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(ComponentFloor {
        path: installed_path(name, path),
        sha256: crate::crypto::sha256_hex(&bytes),
        size: meta.size,
        dev: meta.dev,
        ino: meta.ino,
    })
}

/// The floor records the *installed* absolute path even when the
/// measurement happened under a staging or alternate root, so a floor
/// built during image composition means the same thing at runtime.
fn installed_path(name: &str, measured: &Path) -> String {
    match super::component(name) {
        Some(component) => component.path.to_string(),
        None => measured.to_string_lossy().to_string(),
    }
}

fn sanitize_reason(reason: &str) -> String {
    let cleaned = reason
        .chars()
        .filter(|ch| !ch.is_control())
        .take(200)
        .collect::<String>();
    if cleaned.is_empty() {
        "unspecified".to_string()
    } else {
        cleaned
    }
}

fn exists(path: &Path) -> Result<bool, FloorError> {
    match fsec::lstat(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(FloorError::Unreadable(format!(
            "{}: {error}",
            path.display()
        ))),
    }
}

fn string_at(object: &Map<String, Value>, key: &str) -> Result<String, FloorError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| FloorError::Corrupt(format!("field `{key}` is missing or not a string")))
}

fn u64_at(object: &Map<String, Value>, key: &str) -> Result<u64, FloorError> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| FloorError::Corrupt(format!("field `{key}` is not an unsigned integer")))
}

fn map_at<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Map<String, Value>, FloorError> {
    object
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| FloorError::Corrupt(format!("field `{key}` is not an object")))
}

fn array_at<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a Vec<Value>, FloorError> {
    object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| FloorError::Corrupt(format!("field `{key}` is not an array")))
}

/// An exclusive advisory lock on the floor directory's lock file.
///
/// Held for the duration of a commit. Dropping the guard closes the
/// descriptor, which releases the lock — including when the holder is
/// killed mid-transaction, so a crashed `postinst` cannot wedge every
/// later upgrade.
#[derive(Debug)]
pub struct CommitLock {
    #[allow(dead_code)]
    file: std::fs::File,
}

#[cfg(unix)]
fn lock_exclusive(path: &Path, mode: u32, allowed_uids: &[u32]) -> Result<CommitLock, FloorError> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| FloorError::Write(format!("{}: {error}", path.display())))?;
    // The lock file is inside the private tree, but it is still opened
    // by name, so confirm the descriptor really is a root-owned regular
    // file before trusting the serialization it provides.
    let meta = fsec::require_secure_fd(file.as_raw_fd(), allowed_uids)
        .map_err(|error| FloorError::Insecure(format!("{}: {error}", path.display())))?;
    if !meta.is_file {
        return Err(FloorError::Insecure(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        return Err(FloorError::Write(format!(
            "failed to lock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(CommitLock { file })
}

#[cfg(not(unix))]
fn lock_exclusive(
    _path: &Path,
    _mode: u32,
    _allowed_uids: &[u32],
) -> Result<CommitLock, FloorError> {
    Err(FloorError::Insecure(
        "the security floor requires a Unix host".to_string(),
    ))
}

#[cfg(unix)]
fn create_dir(path: &Path, mode: u32) -> Result<(), FloorError> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    match fsec::lstat(path) {
        Ok(meta) => {
            if meta.is_symlink || !meta.is_dir {
                return Err(FloorError::Insecure(format!(
                    "{} exists and is not a directory",
                    path.display()
                )));
            }
            if meta.mode & 0o7777 != mode {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                    .map_err(|error| FloorError::Write(format!("{}: {error}", path.display())))?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| FloorError::Write(format!("{}: {error}", parent.display())))?;
            }
            std::fs::DirBuilder::new()
                .mode(mode)
                .create(path)
                .map_err(|error| FloorError::Write(format!("{}: {error}", path.display())))?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .map_err(|error| FloorError::Write(format!("{}: {error}", path.display())))
        }
        Err(error) => Err(FloorError::Unreadable(format!(
            "{}: {error}",
            path.display()
        ))),
    }
}

#[cfg(not(unix))]
fn create_dir(_path: &Path, _mode: u32) -> Result<(), FloorError> {
    Err(FloorError::Insecure(
        "the security floor requires a Unix host".to_string(),
    ))
}

#[cfg(unix)]
fn write_new_file(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_new_file(_path: &Path, _bytes: &[u8], _mode: u32) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "the security floor requires a Unix host",
    ))
}

#[cfg(unix)]
fn append_file(path: &Path, mode: u32) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(not(unix))]
fn append_file(_path: &Path, _mode: u32) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::other(
        "the security floor requires a Unix host",
    ))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/update/floor.rs"
    ));
}
