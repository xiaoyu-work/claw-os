//! Root-owned App review decisions. These records never grant capabilities.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::apps::permission_review::PermissionReview;
use crate::caps::Manifest;
use crate::provenance::runtime::PackageRef;
use crate::provenance::{PackageKind, VerifiedPackage};

const SCHEMA_VERSION: u32 = 1;
const REVIEW_LIFETIME_SECS: u64 = 30 * 60;
const MAX_PENDING_PER_OWNER: usize = 32;
const MAX_RECORD_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewKind {
    AppInstall,
    AppActivation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewState {
    Pending,
    Approved,
    Denied,
    Consumed,
    Expired,
    Stale,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRecord {
    pub schema_version: u32,
    pub id: String,
    pub owner_uid: u32,
    pub kind: ReviewKind,
    pub source_dir: PathBuf,
    pub package: PackageRef,
    pub manifest: Manifest,
    pub contract_digest: String,
    pub requester: String,
    pub requested_at: u64,
    pub expires_at: u64,
    pub owner_generation: u32,
    pub state: ReviewState,
    pub decided_at: Option<u64>,
    pub decided_by: Option<u32>,
    pub inherited_from: Option<String>,
}

impl ReviewRecord {
    pub fn permission_review(&self) -> Result<PermissionReview, String> {
        PermissionReview::from_manifest(&self.manifest)
    }

    pub fn effective_state(&self) -> Result<ReviewState, String> {
        if self.owner_generation != current_generation(self.owner_uid, &self.manifest.id)? {
            return Ok(ReviewState::Stale);
        }
        if matches!(self.state, ReviewState::Pending | ReviewState::Approved)
            && now()? >= self.expires_at
        {
            return Ok(ReviewState::Expired);
        }
        Ok(self.state)
    }
}

fn root() -> PathBuf {
    super::root().join("system-reviews")
}

fn record_path(id: &str) -> Result<PathBuf, String> {
    let valid = id.strip_prefix("rv-").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if !valid {
        return Err("invalid system review id".into());
    }
    Ok(root().join(format!("{id}.json")))
}

fn now() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| format!("system review clock: {error}"))
}

fn current_generation(owner: u32, app_id: &str) -> Result<u32, String> {
    super::generations::current(Some(owner), &format!("system-review:{app_id}"))
}

fn snapshot(package: &VerifiedPackage) -> Result<(Manifest, PermissionReview), String> {
    if package.kind() != PackageKind::App {
        return Err("system App review requires an authenticated App package".into());
    }
    let text = package.manifest_text().map_err(|error| error.to_string())?;
    if text.len() > MAX_MANIFEST_BYTES {
        return Err("App manifest exceeds the system review size limit".into());
    }
    let manifest = Manifest::from_json(&text).map_err(|error| error.to_string())?;
    if manifest.id != package.id() {
        return Err("App manifest identity does not match the authenticated package".into());
    }
    manifest
        .validate_tools_against_catalog(&crate::ai::tools::list_names())
        .map_err(|error| format!("App review catalog check: {error}"))?;
    let review = PermissionReview::from_manifest(&manifest)?;
    Ok((manifest, review))
}

fn save(record: &ReviewRecord) -> Result<(), String> {
    let body =
        serde_json::to_vec(record).map_err(|error| format!("serialize system review: {error}"))?;
    if body.len() as u64 > MAX_RECORD_BYTES {
        return Err("system review exceeds its record size limit".into());
    }
    super::write_atomic_with(
        &record_path(&record.id)?,
        &body,
        super::Durability::Committed,
    )
    .map_err(|error| format!("persist system review: {error}"))
}

#[cfg(unix)]
fn read_record(path: &Path) -> Result<ReviewRecord, String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("read system review: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect system review: {error}"))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.len() > MAX_RECORD_BYTES
    {
        return Err("system review must be a private, owned, bounded regular file".into());
    }
    let mut body = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|error| format!("read system review data: {error}"))?;
    if body.len() as u64 > MAX_RECORD_BYTES {
        return Err("system review exceeds its record size limit".into());
    }
    let record: ReviewRecord =
        serde_json::from_slice(&body).map_err(|error| format!("parse system review: {error}"))?;
    if record.schema_version != SCHEMA_VERSION || record_path(&record.id)? != path {
        return Err("system review schema or record identity is invalid".into());
    }
    if record.package.kind != PackageKind::App
        || record.package.id != record.manifest.id
        || record.permission_review()?.contract_digest != record.contract_digest
    {
        return Err("system review does not match its App permission snapshot".into());
    }
    if matches!(
        record.state,
        ReviewState::Approved | ReviewState::Denied | ReviewState::Consumed
    ) && (record.decided_by != Some(record.owner_uid) || record.decided_at.is_none())
    {
        return Err("resolved system review has no matching user decision".into());
    }
    Ok(record)
}

#[cfg(not(unix))]
fn read_record(_path: &Path) -> Result<ReviewRecord, String> {
    Err("protected system reviews require a Unix host".into())
}

pub fn get(owner: u32, id: &str) -> Result<ReviewRecord, String> {
    let record = read_record(&record_path(id)?)?;
    if record.owner_uid != owner {
        return Err("system review is not owned by this user".into());
    }
    Ok(record)
}

fn records(owner: u32) -> Result<Vec<ReviewRecord>, String> {
    let entries = match std::fs::read_dir(root()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("list system reviews: {error}")),
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read system review entry: {error}"))?;
        let path = entry.path();
        if path.extension() != Some(std::ffi::OsStr::new("json"))
            || path
                .file_name()
                .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."))
        {
            continue;
        }
        let record = read_record(&path)?;
        if record.owner_uid == owner {
            records.push(record);
        }
    }
    records.sort_by_key(|record| (record.requested_at, record.id.clone()));
    Ok(records)
}

pub fn pending(owner: u32, limit: usize) -> Result<Vec<ReviewRecord>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut pending = Vec::new();
    for record in records(owner)? {
        if record.effective_state()? == ReviewState::Pending {
            pending.push(record);
            if pending.len() >= limit {
                break;
            }
        }
    }
    Ok(pending)
}

pub fn submit(
    owner: u32,
    kind: ReviewKind,
    package: &VerifiedPackage,
    requester: String,
) -> Result<ReviewRecord, String> {
    let (manifest, review) = snapshot(package)?;
    let package_ref = PackageRef::of(package);
    let submitted_at = now()?;
    let expires_at = submitted_at
        .checked_add(REVIEW_LIFETIME_SECS)
        .ok_or("system review deadline overflow")?;
    super::ensure_dirs().map_err(|error| format!("approvals directory: {error}"))?;
    crate::filelock::with_exclusive_path_lock(&super::grant_lock_path(), || {
        let existing = records(owner)?;
        let mut open = 0;
        for record in &existing {
            let state = record.effective_state()?;
            if state == ReviewState::Pending {
                open += 1;
            }
            if matches!(state, ReviewState::Pending | ReviewState::Approved)
                && record.kind == kind
                && record.package == package_ref
                && record.contract_digest == review.contract_digest
                && record.source_dir == package.dir()
            {
                return Ok(record.clone());
            }
        }
        let owner_generation = current_generation(owner, &manifest.id)?;
        let accepted = existing.iter().rev().find(|record| {
            record.owner_generation == owner_generation
                && record.state == ReviewState::Consumed
                && record.package.id == package_ref.id
                && record.package.publisher_key_id == package_ref.publisher_key_id
                && record.package.tier == package_ref.tier
                && record.contract_digest == review.contract_digest
        });
        if accepted.is_none() && open >= MAX_PENDING_PER_OWNER {
            return Err("too many pending system reviews for this user".into());
        }
        let record = ReviewRecord {
            schema_version: SCHEMA_VERSION,
            id: format!("rv-{}", uuid::Uuid::new_v4().simple()),
            owner_uid: owner,
            kind,
            source_dir: package.dir().to_path_buf(),
            package: package_ref,
            manifest,
            contract_digest: review.contract_digest,
            requester,
            requested_at: submitted_at,
            expires_at,
            owner_generation,
            state: if accepted.is_some() {
                ReviewState::Approved
            } else {
                ReviewState::Pending
            },
            decided_at: accepted.map(|_| submitted_at),
            decided_by: accepted.and_then(|record| record.decided_by),
            inherited_from: accepted.map(|record| record.id.clone()),
        };
        save(&record)?;
        Ok(record)
    })
}

fn require_package(record: &ReviewRecord, package: &VerifiedPackage) -> Result<(), String> {
    let (_, review) = snapshot(package)?;
    if record.package != PackageRef::of(package) || record.contract_digest != review.contract_digest
    {
        return Err("App package changed after system review; review the current package".into());
    }
    Ok(())
}

/// The caller is the broker after authenticating the privileged OS decision helper.
pub fn decide(
    owner: u32,
    id: &str,
    approve: bool,
    package: Option<&VerifiedPackage>,
) -> Result<ReviewRecord, String> {
    super::ensure_dirs().map_err(|error| format!("approvals directory: {error}"))?;
    crate::filelock::with_exclusive_path_lock(&super::grant_lock_path(), || {
        let mut record = get(owner, id)?;
        if record.effective_state()? != ReviewState::Pending {
            return Err("system review is no longer pending; refresh its state".into());
        }
        if approve {
            require_package(
                &record,
                package.ok_or("App review requires fresh package verification")?,
            )?;
        }
        record.state = if approve {
            ReviewState::Approved
        } else {
            ReviewState::Denied
        };
        let decided_at = now()?;
        record.decided_at = Some(decided_at);
        if approve {
            record.expires_at = decided_at
                .checked_add(REVIEW_LIFETIME_SECS)
                .ok_or("system review deadline overflow")?;
        }
        record.decided_by = Some(owner);
        save(&record)?;
        Ok(record)
    })
}

pub fn consume(owner: u32, id: &str, package: &VerifiedPackage) -> Result<ReviewRecord, String> {
    super::ensure_dirs().map_err(|error| format!("approvals directory: {error}"))?;
    crate::filelock::with_exclusive_path_lock(&super::grant_lock_path(), || {
        let mut record = get(owner, id)?;
        if record.effective_state()? != ReviewState::Approved {
            return Err("App installation requires an approved, current system review".into());
        }
        require_package(&record, package)?;
        record.state = ReviewState::Consumed;
        save(&record)?;
        Ok(record)
    })
}

pub fn invalidate(owner: u32, id: &str) -> Result<(), String> {
    super::ensure_dirs().map_err(|error| format!("approvals directory: {error}"))?;
    crate::filelock::with_exclusive_path_lock(&super::grant_lock_path(), || {
        let mut record = get(owner, id)?;
        if matches!(record.state, ReviewState::Pending | ReviewState::Approved) {
            record.state = ReviewState::Stale;
            save(&record)?;
        }

        Ok(())
    })
}

pub fn has_accepted(owner: u32, package: &VerifiedPackage) -> Result<bool, String> {
    let (manifest, review) = snapshot(package)?;
    let package_ref = PackageRef::of(package);
    let generation = current_generation(owner, &manifest.id)?;
    Ok(records(owner)?.iter().any(|record| {
        record.owner_generation == generation
            && record.state == ReviewState::Consumed
            && record.package.id == package_ref.id
            && record.package.publisher_key_id == package_ref.publisher_key_id
            && record.package.tier == package_ref.tier
            && record.contract_digest == review.contract_digest
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/approvals/system_review.rs"
    ));
}
