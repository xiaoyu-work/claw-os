//! Explicit recovery authorizations.
//!
//! There is no environment variable, APT option, package parameter or
//! editable configuration file that turns the floor off. When a
//! machine genuinely has to go backwards — a release regression, a
//! broken upgrade, a forensic downgrade — an operator records a single
//! authorization that names *exactly one* package at *exactly one*
//! epoch, version and manifest digest, with a reason and an expiry.
//!
//! Properties:
//!
//! * **Root only, terminal only.** Written by `claw-security-floor
//!   recover authorize` running as uid 0 with a real controlling
//!   terminal, after the operator types a confirmation phrase that
//!   contains the package and version. There is no broker route, no
//!   tool, no App operation and no MCP surface that reaches it, and
//!   the command refuses to run inside an agent worker, App runner or
//!   MCP session.
//! * **Narrow.** A token for `claw-os-agent` cannot authorize
//!   `claw-os-base`; a token for one version cannot authorize another;
//!   a token whose digest does not match the candidate manifest is
//!   refused.
//! * **One use.** Consumption is a `rename` into `consumed/`, which is
//!   atomic on the same filesystem, so two concurrent transactions
//!   cannot both spend it.
//! * **Bound to the floor it was written against.** The authorization
//!   records the floor generation and digest it was created for, so a
//!   token cannot be stored away and replayed after the machine has
//!   moved on.
//! * **Expiring.** An unbounded expiry is refused, and a token that
//!   has expired is refused rather than silently renewed.
//!
//! Local root can ultimately bypass any software-only control on its
//! own filesystem. The point of this path is that a downgrade is never
//! *accidental*, is always attributable, and is always recorded.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{json, Value};

use super::canonical;
use super::debver;
use super::floor::FloorStore;
use super::manifest::require_digest;

pub const FORMAT: &str = "claw.security-recovery/v1";

/// Longest life an authorization may be given.
pub const MAX_LIFETIME_HOURS: i64 = 72;

const CONSUMED_DIR: &str = "consumed";
const MAX_TOKEN_BYTES: u64 = 64 * 1024;

/// A recorded authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorization {
    pub id: String,
    pub package: String,
    pub security_epoch: u64,
    pub version: String,
    pub manifest_sha256: String,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub created_by_uid: u32,
    /// Floor generation the operator saw when authorizing.
    pub floor_generation: u64,
    /// Digest of that floor generation.
    pub floor_sha256: Option<String>,
}

impl Authorization {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut document = json!({
            "created_at": self.created_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            "created_by_uid": self.created_by_uid,
            "expires_at": self.expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            "floor_generation": self.floor_generation,
            "format": FORMAT,
            "id": self.id,
            "manifest_sha256": self.manifest_sha256,
            "package": self.package,
            "reason": self.reason,
            "security_epoch": self.security_epoch,
            "version": self.version,
        });
        if let Some(floor_sha256) = &self.floor_sha256 {
            if let Some(object) = document.as_object_mut() {
                object.insert("floor_sha256".to_string(), json!(floor_sha256));
            }
        }
        canonical::to_bytes(&document).unwrap_or_default()
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value = canonical::parse_canonical(bytes)?;
        let object = value
            .as_object()
            .ok_or_else(|| "recovery authorization is not an object".to_string())?;
        if object.get("format").and_then(Value::as_str) != Some(FORMAT) {
            return Err("recovery authorization has an unknown format".to_string());
        }
        let text = |key: &str| -> Result<String, String> {
            object
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("recovery authorization field `{key}` is missing"))
        };
        let number = |key: &str| -> Result<u64, String> {
            object
                .get(key)
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("recovery authorization field `{key}` is not a number"))
        };
        let time = |key: &str| -> Result<DateTime<Utc>, String> {
            DateTime::parse_from_rfc3339(&text(key)?)
                .map(|parsed| parsed.with_timezone(&Utc))
                .map_err(|_| format!("recovery authorization field `{key}` is not a timestamp"))
        };
        let id = text("id")?;
        require_id(&id)?;
        let version = text("version")?;
        if !debver::is_valid(&version) {
            return Err("recovery authorization version is not a Debian version".to_string());
        }
        let manifest_sha256 = text("manifest_sha256")?;
        require_digest(&manifest_sha256)?;
        let floor_sha256 = match object.get("floor_sha256") {
            None => None,
            Some(raw) => {
                let digest = raw
                    .as_str()
                    .ok_or_else(|| "recovery floor digest is not a string".to_string())?;
                require_digest(digest)?;
                Some(digest.to_string())
            }
        };
        Ok(Self {
            id,
            package: text("package")?,
            security_epoch: number("security_epoch")?,
            version,
            manifest_sha256,
            reason: text("reason")?,
            created_at: time("created_at")?,
            expires_at: time("expires_at")?,
            created_by_uid: u32::try_from(number("created_by_uid")?)
                .map_err(|_| "recovery authorization uid is out of range".to_string())?,
            floor_generation: number("floor_generation")?,
            floor_sha256,
        })
    }

    /// Does this authorization cover exactly this candidate?
    pub fn authorizes(
        &self,
        package: &str,
        version: &str,
        security_epoch: u64,
        manifest_sha256: &str,
        floor_generation: u64,
        floor_sha256: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        if self.package != package {
            return Err(format!(
                "authorization names package `{}`, not `{package}`",
                self.package
            ));
        }
        if self.version != version {
            return Err(format!(
                "authorization names version `{}`, not `{version}`",
                self.version
            ));
        }
        if self.security_epoch != security_epoch {
            return Err(format!(
                "authorization names security epoch {}, not {security_epoch}",
                self.security_epoch
            ));
        }
        if self.manifest_sha256 != manifest_sha256 {
            return Err("authorization names a different release manifest".to_string());
        }
        if self.floor_generation != floor_generation {
            return Err(format!(
                "authorization was written for floor generation {}, which is no longer current",
                self.floor_generation
            ));
        }
        if self.floor_sha256.as_deref() != floor_sha256 {
            return Err("authorization was written against a different floor state".to_string());
        }
        if now > self.expires_at {
            return Err("authorization has expired".to_string());
        }
        if now < self.created_at - Duration::minutes(5) {
            return Err("authorization is dated in the future".to_string());
        }
        Ok(())
    }
}

/// Reader/writer for the `recovery/` directory of one floor store.
#[derive(Debug, Clone)]
pub struct RecoveryStore {
    dir: PathBuf,
}

impl RecoveryStore {
    pub fn new(floor: &FloorStore) -> Self {
        Self {
            dir: floor.recovery_dir(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every pending authorization, oldest file name first.
    pub fn pending(&self) -> Result<Vec<(PathBuf, Authorization)>, String> {
        let mut found = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
            Err(error) => return Err(format!("{}: {error}", self.dir.display())),
        };
        let mut paths = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let meta = crate::provenance::fsec::lstat(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if meta.is_symlink || !meta.is_file || meta.nlink != 1 {
                return Err(format!(
                    "{}: recovery authorizations must be plain unlinked files",
                    path.display()
                ));
            }
            if meta.mode & 0o077 != 0 {
                return Err(format!(
                    "{}: recovery authorization is readable by other accounts",
                    path.display()
                ));
            }
            if meta.size > MAX_TOKEN_BYTES {
                return Err(format!(
                    "{}: recovery authorization is too large",
                    path.display()
                ));
            }
            let bytes =
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            let authorization = Authorization::parse(&bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            found.push((path, authorization));
        }
        Ok(found)
    }

    /// Find an authorization that covers this candidate exactly.
    #[allow(clippy::too_many_arguments)]
    pub fn find(
        &self,
        package: &str,
        version: &str,
        security_epoch: u64,
        manifest_sha256: &str,
        floor_generation: u64,
        floor_sha256: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Option<(PathBuf, Authorization)>, String> {
        for (path, authorization) in self.pending()? {
            if authorization
                .authorizes(
                    package,
                    version,
                    security_epoch,
                    manifest_sha256,
                    floor_generation,
                    floor_sha256,
                    now,
                )
                .is_ok()
            {
                return Ok(Some((path, authorization)));
            }
        }
        Ok(None)
    }

    /// Spend an authorization. The rename is the commit point: it
    /// either moves the file out of the pending set or fails, so a
    /// second transaction cannot spend the same token.
    pub fn consume(&self, path: &Path, authorization: &Authorization) -> Result<PathBuf, String> {
        let consumed_dir = self.dir.join(CONSUMED_DIR);
        create_private_dir(&consumed_dir)?;
        let target = consumed_dir.join(format!("{}.json", authorization.id));
        if target.exists() {
            return Err("recovery authorization has already been used".to_string());
        }
        std::fs::rename(path, &target)
            .map_err(|error| format!("failed to consume recovery authorization: {error}"))?;
        crate::provenance::fsec::sync_dir(&consumed_dir)
            .map_err(|error| format!("failed to sync consumed authorizations: {error}"))?;
        crate::provenance::fsec::sync_dir(&self.dir)
            .map_err(|error| format!("failed to sync recovery directory: {error}"))?;
        Ok(target)
    }

    /// Write a new authorization. Fails if one with the same id exists.
    pub fn write(&self, authorization: &Authorization) -> Result<PathBuf, String> {
        create_private_dir(&self.dir)?;
        let path = self.dir.join(format!("{}.json", authorization.id));
        write_private_file(&path, &authorization.to_bytes())?;
        crate::provenance::fsec::sync_dir(&self.dir)
            .map_err(|error| format!("failed to sync recovery directory: {error}"))?;
        Ok(path)
    }

    /// Drop a pending authorization without using it.
    pub fn revoke(&self, id: &str) -> Result<bool, String> {
        require_id(id)?;
        let path = self.dir.join(format!("{id}.json"));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }
}

/// Authorization ids are opaque hex so they can be used as file names
/// without any escaping question.
pub fn require_id(id: &str) -> Result<(), String> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("recovery authorization id is malformed".to_string());
    }
    Ok(())
}

/// A fresh, unpredictable id.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Clamp an operator-supplied lifetime.
pub fn checked_lifetime(hours: i64) -> Result<Duration, String> {
    if hours <= 0 {
        return Err("recovery authorizations must expire in the future".to_string());
    }
    if hours > MAX_LIFETIME_HOURS {
        return Err(format!(
            "recovery authorizations may not last longer than {MAX_LIFETIME_HOURS} hours"
        ));
    }
    Ok(Duration::hours(hours))
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    if path.exists() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("{}: {error}", path.display()))?;
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(not(unix))]
fn create_private_dir(_path: &Path) -> Result<(), String> {
    Err("recovery authorizations require a Unix host".to_string())
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(not(unix))]
fn write_private_file(_path: &Path, _bytes: &[u8]) -> Result<(), String> {
    Err("recovery authorizations require a Unix host".to_string())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/update/recovery.rs"
    ));
}
