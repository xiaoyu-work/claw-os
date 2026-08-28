//! Durable, per-domain trust generation state.
//!
//! A long-lived daemon (`clawd`, `claw-agentd`) loads the trust store
//! once and would otherwise keep revoked keys and stale verification
//! caches for its entire lifetime. Re-reading and re-hashing every
//! trust file before every launch, disclosure and attach would be
//! correct but far too expensive on the hot path.
//!
//! Each trust *domain* — the root-owned system roots, and one owner's
//! per-user roots — therefore carries a small state file:
//!
//! ```jsonc
//! {
//!   "schema": "claw.trust-state/v1",
//!   "generation": 7,                  // monotonic, bumped on every change
//!   "fingerprint": "sha256:…",        // over the domain's trust file contents
//!   "updated_at": "2026-08-28T…Z"
//! }
//! ```
//!
//! Readers take the cheap path: `lstat` the state file plus each root
//! directory and compare `(dev, ino, size, mtime, ctime)` against what
//! they saw last time. Only when something moved do they re-read the
//! state, recompute the content fingerprint and reload.
//!
//! ## Why not mtime alone
//!
//! An mtime can be preserved across a rewrite, can go backwards when a
//! file is restored from backup, and has coarse granularity on some
//! filesystems. The `fingerprint` is a digest over the actual bytes, so
//! trust files that were changed without the state being re-recorded
//! are detected whatever their timestamps say.
//!
//! ## Fail-closed
//!
//! Once a domain has a state file, it must stay valid. A corrupt state
//! file, a fingerprint that disagrees with the files on disk, or a
//! generation that moves backwards makes the domain contribute
//! **nothing** and raises a diagnostic.
//!
//! A domain with neither trust files nor state has never been
//! initialised — a fresh machine with no operator keys — and is empty,
//! which is fail-closed anyway: no keys means no package verifies. A
//! domain that has trust files but *no* state is a different thing:
//! every command that writes a trust file records the state in the same
//! operation, so the state was removed afterwards. That fails closed
//! too, because otherwise deleting one file would be the way to make a
//! revoked key usable again.
//!
//! ## What the generation does and does not prove
//!
//! Be precise about this, because it is easy to overclaim.
//!
//! The generation and fingerprint are stored **in the same trust domain
//! as the files they describe**, and are writable by whoever owns that
//! domain. So:
//!
//! * They *do* detect a trust file changed, restored or rolled back on
//!   its own, without the state being updated — including a backup
//!   restore that preserves mtimes.
//! * They *do* stop a long-lived daemon from serving a store it loaded
//!   before a revocation: the recorded generation moves, the daemon
//!   notices on its next check, and reloads.
//! * They do **not** detect an attacker who owns the domain and
//!   restores the trust files *and* the state file together. That
//!   snapshot is internally consistent — the fingerprint matches, the
//!   generation is whatever the old state said — and nothing local can
//!   tell it apart from the present. For the owner domain that means
//!   the owner (or same-user malware); for the system domain it means
//!   root, who can already replace the `cos` binary.
//!
//! Detecting a *coordinated* rollback would need a monotonic anchor
//! outside the domain — a TPM counter, a remote attestation service, or
//! a root-owned floor for the owner domains — and there is none here.
//! The claim is therefore the narrower one: a revocation cannot be
//! undone by restoring a single file, and it cannot be outlived by a
//! running daemon. It is not rollback-proof against the domain's own
//! owner.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::fsec;

pub const TRUST_STATE_SCHEMA_V1: &str = "claw.trust-state/v1";

/// File name of the per-domain state file, stored next to the domain's
/// roots (`/etc/cos/trust/state.json`, `~/.config/cos/trust/state.json`).
pub const TRUST_STATE_FILE: &str = "state.json";

/// Which trust domain a root belongs to. Domains version independently
/// so an operator changing their own keys does not invalidate the
/// system domain and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrustDomain {
    /// Root-owned vendor + operator roots under `/usr/lib/cos` and `/etc/cos`.
    System,
    /// One owner's publisher + developer roots, keyed by uid.
    Owner(u32),
}

impl TrustDomain {
    pub fn as_key(self) -> String {
        match self {
            Self::System => "system".to_string(),
            Self::Owner(uid) => format!("owner:{uid}"),
        }
    }

    /// The uid set permitted to own this domain's state file.
    pub fn allowed_uids(self) -> Vec<u32> {
        match self {
            Self::System => vec![0],
            Self::Owner(uid) => vec![uid],
        }
    }
}

/// On-disk state document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustState {
    pub schema: String,
    pub generation: u64,
    pub fingerprint: String,
    pub updated_at: String,
}

/// Cheap identity of one filesystem node, used to decide whether a
/// re-read is needed at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeStamp {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ctime: i64,
    pub ctime_nsec: i64,
}

/// The stamps a reader compares to decide whether the trust store on
/// disk still matches the one it has in memory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustWatch {
    entries: Vec<(PathBuf, Option<NodeStamp>)>,
}

impl TrustWatch {
    pub fn observe(paths: &[PathBuf]) -> Self {
        let mut entries: Vec<(PathBuf, Option<NodeStamp>)> = paths
            .iter()
            .map(|path| (path.clone(), stamp(path)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(unix)]
fn stamp(path: &Path) -> Option<NodeStamp> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::lstat(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(NodeStamp {
        dev: st.st_dev,
        ino: st.st_ino,
        size: st.st_size.max(0) as u64,
        mtime: st.st_mtime,
        mtime_nsec: st.st_mtime_nsec,
        ctime: st.st_ctime,
        ctime_nsec: st.st_ctime_nsec,
    })
}

#[cfg(not(unix))]
fn stamp(_path: &Path) -> Option<NodeStamp> {
    None
}

/// Digest over one domain's trust file contents.
///
/// Covers the file names *and* their bytes, so adding, removing,
/// renaming or editing an entry all change the value.
pub fn fingerprint_files(files: &[(String, Vec<u8>)]) -> String {
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = crate::crypto::Sha256Stream::new();
    h.update(b"claw-provenance/v1\x00trust-domain\x00");
    h.update(&(sorted.len() as u64).to_le_bytes());
    for (name, body) in sorted {
        h.update(&(name.len() as u64).to_le_bytes());
        h.update(name.as_bytes());
        h.update(&(body.len() as u64).to_le_bytes());
        h.update(body);
    }
    format!("sha256:{}", h.finalize_hex())
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("{path}: {reason}")]
    Io { path: PathBuf, reason: String },
    #[error("{path}: trust state is corrupt: {reason}")]
    Corrupt { path: PathBuf, reason: String },
}

/// Read and structurally validate a domain's state file.
///
/// `Ok(None)` means the domain has never been initialised, which is a
/// legitimate empty state. Any other problem is an error: once a state
/// file exists it must remain readable, well formed and correctly
/// owned, or the domain fails closed.
pub fn read_state(dir: &Path, domain: TrustDomain) -> Result<Option<TrustState>, StateError> {
    let path = dir.join(TRUST_STATE_FILE);
    let meta = match fsec::lstat(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(StateError::Io {
                path,
                reason: e.to_string(),
            })
        }
    };
    if meta.is_symlink || !meta.is_file {
        return Err(StateError::Corrupt {
            path,
            reason: "not a regular file".to_string(),
        });
    }
    if !domain.allowed_uids().contains(&meta.uid) {
        return Err(StateError::Corrupt {
            path,
            reason: format!("owned by uid {}", meta.uid),
        });
    }
    if meta.is_group_or_world_writable() {
        return Err(StateError::Corrupt {
            path,
            reason: format!("mode {:o} is group- or world-writable", meta.mode),
        });
    }
    if meta.size > 64 * 1024 {
        return Err(StateError::Corrupt {
            path,
            reason: format!("{} bytes is implausible for a state file", meta.size),
        });
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| StateError::Io {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    let state: TrustState = serde_json::from_str(&raw).map_err(|e| StateError::Corrupt {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    if state.schema != TRUST_STATE_SCHEMA_V1 {
        return Err(StateError::Corrupt {
            path,
            reason: format!("unsupported schema `{}`", state.schema),
        });
    }
    if !super::envelope::is_sha256_ref(&state.fingerprint) {
        return Err(StateError::Corrupt {
            path,
            reason: "fingerprint is not `sha256:<64 hex>`".to_string(),
        });
    }
    Ok(Some(state))
}

/// Write the domain's state atomically and durably.
///
/// The temp file is fsynced before the rename and the directory is
/// fsynced after it, so a crash leaves either the old state or the new
/// one — never a truncated file that would fail the domain closed.
pub fn write_state(
    dir: &Path,
    domain: TrustDomain,
    fingerprint: &str,
    previous: Option<&TrustState>,
) -> Result<TrustState, StateError> {
    let generation = previous
        .map(|s| s.generation.saturating_add(1))
        .unwrap_or(1);
    let state = TrustState {
        schema: TRUST_STATE_SCHEMA_V1.to_string(),
        generation,
        fingerprint: fingerprint.to_string(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    std::fs::create_dir_all(dir).map_err(|e| StateError::Io {
        path: dir.to_path_buf(),
        reason: e.to_string(),
    })?;
    let path = dir.join(TRUST_STATE_FILE);
    let tmp = dir.join(format!(
        ".{TRUST_STATE_FILE}.tmp-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let body = serde_json::to_vec_pretty(&state).map_err(|e| StateError::Io {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if domain == TrustDomain::System {
                0o644
            } else {
                0o600
            });
        }
        let mut file = options.open(&tmp).map_err(|e| StateError::Io {
            path: tmp.clone(),
            reason: e.to_string(),
        })?;
        use std::io::Write;
        file.write_all(&body).map_err(|e| StateError::Io {
            path: tmp.clone(),
            reason: e.to_string(),
        })?;
        file.sync_all().map_err(|e| StateError::Io {
            path: tmp.clone(),
            reason: e.to_string(),
        })?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        StateError::Io {
            path: path.clone(),
            reason: e.to_string(),
        }
    })?;
    fsec::sync_dir(dir).map_err(|e| StateError::Io {
        path: dir.to_path_buf(),
        reason: e.to_string(),
    })?;
    Ok(state)
}

/// Record a mutation to a domain: recompute the fingerprint from the
/// files now on disk and bump the generation.
///
/// `state_dir` holds the `state.json`; `roots` are the directories
/// whose `*.json` files the fingerprint covers. They differ — a domain
/// state file lives one level above its `publishers.d` / `developer.d`
/// roots — so passing the wrong one records a fingerprint that will
/// never match and fails the domain closed on the next load.
pub fn bump(
    state_dir: &Path,
    domain: TrustDomain,
    roots: &[PathBuf],
) -> Result<TrustState, StateError> {
    let previous = read_state(state_dir, domain).ok().flatten();
    let mut files = Vec::new();
    for root in roots {
        files.extend(read_domain_files(root)?);
    }
    let fingerprint = fingerprint_files(&files);
    write_state(state_dir, domain, &fingerprint, previous.as_ref())
}

/// Convenience for a domain whose state file sits inside its only root.
pub fn bump_in_place(dir: &Path, domain: TrustDomain) -> Result<TrustState, StateError> {
    bump(dir, domain, std::slice::from_ref(&dir.to_path_buf()))
}

/// Read every `*.json` trust file in `dir` (excluding the state file).
pub fn read_domain_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, StateError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => {
            return Err(StateError::Io {
                path: dir.to_path_buf(),
                reason: e.to_string(),
            })
        }
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".json") || name.starts_with('.') || name == TRUST_STATE_FILE {
            continue;
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| StateError::Io {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        if !meta.is_file() {
            continue;
        }
        let body = std::fs::read(&path).map_err(|e| StateError::Io {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        out.push((name, body));
    }
    Ok(out)
}

/// Paths a reader stats to detect a change in one domain: the state
/// file and every root directory that feeds it.
pub fn watch_paths(state_dir: &Path, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = vec![state_dir.join(TRUST_STATE_FILE)];
    out.extend(roots.iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/state.rs"
    ));
}
