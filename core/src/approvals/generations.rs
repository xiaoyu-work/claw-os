//! Revocation generations for approval grants.
//!
//! An approval that lasts beyond one use has to be revocable, and the
//! revocation has to survive a restore from backup. A "revoked" flag on
//! the record itself achieves neither: the record can be deleted, or
//! rolled back to a copy taken before the flag was set, and the
//! authority would re-arm it.
//!
//! The authority is therefore a monotonic **generation** counter kept
//! outside the records, in root-owned state. A grant captures the
//! generation current when it was approved; every load compares that
//! against the generation current now, and a mismatch means the grant
//! is dead. Revoking is an increment, which no restored copy of an old
//! record can undo:
//!
//! ```text
//!   approve  -> binding.generation = current(owner, session)
//!   use      -> binding.generation == current(owner, session) ?
//!   revoke   -> current(owner, session) += 1     (every older grant dies)
//! ```
//!
//! Counters are kept per `(owner, scope)`, where the scope is either
//! one grant session or the owner as a whole. An owner-wide revocation
//! raises a floor every session counter is compared against, so "retire
//! everything this user approved" is one atomic increment rather than a
//! walk over records that could race a concurrent approval.
//!
//! ## Failure policy
//!
//! Unreadable, unparseable, wrongly-owned, group-writable or symlinked
//! state fails closed: [`current`] returns an error and the caller
//! refuses the grant. A binding carrying no generation at all — written
//! before this existed — also fails closed, because there is nothing it
//! could be compared against.
//!
//! ## Atomicity
//!
//! Writes go through [`super::write_atomic`], which was audited against
//! these claims rather than assumed:
//!
//! * the scratch file is opened `create_new` at mode `0600`, i.e.
//!   `O_CREAT|O_EXCL`, so it never follows a symlink and a pre-planted
//!   path makes the write *fail* instead of being redirected;
//! * its name is `.generations.json.tmp.<id>` where `<id>` is a
//!   non-cryptographic hash of time and pid — it is a collision
//!   breaker, not a secret, and `O_EXCL` is what carries the safety;
//! * the data and metadata are `fsync`ed before the rename;
//! * `rename` is atomic and replaces a symlink *at* the destination
//!   rather than following it, so the destination cannot be redirected
//!   either; a failed rename removes the scratch file and propagates
//!   the error;
//! * the destination keeps the scratch file's `0600` mode, which is
//!   then re-asserted, and its owner is whichever account wrote it —
//!   root for the daemon, which is exactly what [`load`] verifies.
//!
//! The parent-directory `fsync` is **mandatory** here. `store` uses the
//! committed write path, so if the directory entry cannot be durably
//! flushed the write returns an error and [`revoke`] fails. That is the
//! only honest answer: after a failed directory sync the increment may
//! or may not have reached the disk, so reporting "revoked" would tell
//! an operator that authority was retired when a power cut could still
//! bring it back. The caller sees the failure and can retry, and a
//! retry re-reads the current file and increments from whatever it
//! finds — so whichever way the ambiguous write resolved, the counter
//! only ever moves forward and never lands below where it already was.
//!
//! A crash therefore leaves the previous file intact — never a
//! truncated counter, which would read as a *lower* generation and
//! re-arm revoked grants. A pre-planted scratch path can make a
//! revocation fail; the caller sees the error and no grant is
//! retired, so the failure mode is a refusal to revoke rather than a
//! silent one. [`revoke`] reads through [`load`], so state that fails
//! the ownership and mode checks is refused for *writes* as well as
//! reads: an attacker cannot widen the file to make the daemon rebuild
//! the counter from what they left behind, and repairing the mode
//! restores the exact generations.
//!
//! [`revoke`] holds the approvals store lock across its read-modify-
//! write, and that is the same lock a spend holds while it evaluates
//! `is_live`. Revocation and consumption are therefore mutually
//! exclusive: a spend that starts after a revocation completes always
//! observes the new generation. Neither path re-enters the lock, so
//! there is no self-deadlock; the lock is never held across a `/proc`
//! read, a subprocess or an await.
//!
//! ## Threat boundary
//!
//! This defends the *local unprivileged* case: a compromised App,
//! worker or same-uid process cannot resurrect a revoked approval by
//! restoring `approved/<id>.json`, because the counter it is compared
//! against lives in root-owned state it cannot write. It does **not**
//! defend against root. An attacker who can rewrite every trusted file
//! can roll this counter back exactly as they can rewrite the approval
//! record, the process registry or the daemon binary. Making a
//! restore-resistant claim against root would need an anchor outside
//! the filesystem — a TPM counter, a remote log, or signed state with a
//! key the machine does not hold — and there is none here, so the
//! honest scope is: revocation survives backup/restore and file
//! tampering by anyone who is not already root.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// On-disk shape. One file per install, rewritten atomically.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Generations {
    /// `owner -> generation`. Raised by an owner-wide revocation and
    /// used as a floor for every session under that owner.
    #[serde(default)]
    owners: BTreeMap<String, u32>,
    /// `owner/session -> generation`.
    #[serde(default)]
    sessions: BTreeMap<String, u32>,
}

/// Which grants a revocation covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevocationScope {
    /// Every reusable approval this owner holds.
    Owner { uid: Option<u32> },
    /// Every reusable approval bound to one grant session.
    Session { uid: Option<u32>, session: String },
}

impl RevocationScope {
    /// Stable label for the audit trail.
    pub fn kind(&self) -> &'static str {
        match self {
            RevocationScope::Owner { .. } => "owner",
            RevocationScope::Session { .. } => "session",
        }
    }

    pub fn owner_uid(&self) -> Option<u32> {
        match self {
            RevocationScope::Owner { uid } => *uid,
            RevocationScope::Session { uid, .. } => *uid,
        }
    }
}

fn state_path() -> PathBuf {
    crate::paths::caps_data_dir()
        .join("approvals")
        .join("generations.json")
}

fn owner_key(uid: Option<u32>) -> String {
    match uid {
        Some(uid) => uid.to_string(),
        // A record with no owner is system-scoped. Kept distinct from
        // uid 0 so a root approval and an unattributed one cannot
        // revoke each other by accident.
        None => "system".to_string(),
    }
}

fn session_key(uid: Option<u32>, session: &str) -> String {
    format!("{}/{session}", owner_key(uid))
}

/// Read the state, refusing anything that is not the root-owned regular
/// file this module writes.
fn load() -> Result<Generations, String> {
    let path = state_path();
    // `symlink_metadata` deliberately: a symlink planted here must be a
    // refusal, not something we follow to whatever it points at.
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        // No file yet means nothing has been revoked. That is a real
        // answer, not a missing one: generation 0 everywhere.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Generations::default())
        }
        Err(error) => return Err(format!("read approval generations: {error}")),
    };
    if metadata.file_type().is_symlink() {
        return Err("approval generation state is a symlink".to_string());
    }
    if !metadata.file_type().is_file() {
        return Err("approval generation state is not a regular file".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        // The daemon writes this as root. A test harness runs
        // unprivileged against its own `COS_DATA_DIR`, so the check is
        // "not writable by anyone but the file's owner", plus a root
        // requirement when the daemon itself is root.
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err("approval generation state is group- or world-writable".to_string());
        }
        if metadata.nlink() != 1 {
            return Err("approval generation state has extra hard links".to_string());
        }
        if unsafe { libc::geteuid() } == 0 && metadata.uid() != 0 {
            return Err("approval generation state is not root-owned".to_string());
        }
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|error| format!("read approval generations: {error}"))?;
    serde_json::from_str(&data).map_err(|error| format!("parse approval generations: {error}"))
}

fn store(generations: &Generations) -> Result<(), String> {
    let path = state_path();
    let payload = serde_json::to_string_pretty(generations)
        .map_err(|error| format!("serialize approval generations: {error}"))?;
    // Committed, not best effort: a lost increment silently re-arms
    // authority the user retired, so the write is not allowed to report
    // success unless the directory entry is durably committed.
    super::write_atomic_with(&path, payload.as_bytes(), super::Durability::Committed)
        .map_err(|error| format!("write approval generations: {error}"))
}

/// The generation a grant for `(uid, session)` must carry to be live.
///
/// The owner floor is folded in, so raising it retires every session
/// under that owner without touching their records.
pub fn current(uid: Option<u32>, session: &str) -> Result<u32, String> {
    Ok(resolve(&load()?, uid, session))
}

fn resolve(generations: &Generations, uid: Option<u32>, session: &str) -> u32 {
    let owner = generations
        .owners
        .get(&owner_key(uid))
        .copied()
        .unwrap_or(0);
    let session = generations
        .sessions
        .get(&session_key(uid, session))
        .copied()
        .unwrap_or(0);
    owner.max(session)
}

/// Retire every reusable approval in `scope`, returning the generation
/// now in force.
///
/// The read, the increment and the write happen under the approvals
/// store lock — the same lock consumption takes — so a revocation
/// cannot interleave with a spend, and two concurrent revocations
/// cannot both read one value and write the same successor.
///
/// An `Err` means the revocation did **not** take effect as far as this
/// process can tell. It may have reached the disk and it may not, so
/// the caller must treat authority as still live and retry rather than
/// report a retirement. A retry re-reads the file and increments from
/// what it finds, so the counter only ever moves forward.
pub fn revoke(scope: &RevocationScope) -> Result<u32, String> {
    super::ensure_dirs().map_err(|error| format!("approvals dir: {error}"))?;
    crate::filelock::with_exclusive_path_lock(&super::grant_lock_path(), || {
        let mut generations = load()?;
        let next = match scope {
            RevocationScope::Owner { uid } => {
                let entry = generations.owners.entry(owner_key(*uid)).or_insert(0);
                *entry = entry.saturating_add(1);
                let raised = *entry;
                // Session counters at or below the new floor are
                // redundant; dropping them bounds the file's growth and
                // can lower nothing, because `resolve` takes the
                // maximum of the two.
                let prefix = format!("{}/", owner_key(*uid));
                generations
                    .sessions
                    .retain(|key, value| !key.starts_with(&prefix) || *value > raised);
                raised
            }
            RevocationScope::Session { uid, session } => {
                let floor = generations
                    .owners
                    .get(&owner_key(*uid))
                    .copied()
                    .unwrap_or(0);
                let entry = generations
                    .sessions
                    .entry(session_key(*uid, session))
                    .or_insert(0);
                *entry = (*entry).max(floor).saturating_add(1);
                *entry
            }
        };
        store(&generations)?;
        Ok(next)
    })
}

/// Retire everything for one session without failing the operation that
/// asked for it.
///
/// Used on teardown paths — a session finishing, a task being
/// cancelled, a worker lease lapsing — which must clean up but must not
/// turn a cleanup failure into a request failure. The failure is logged
/// loudly, because a reusable approval outliving its session is exactly
/// what this exists to prevent.
pub fn revoke_session_best_effort(uid: Option<u32>, session: &str) {
    let scope = RevocationScope::Session {
        uid,
        session: session.to_string(),
    };
    match revoke(&scope) {
        Ok(generation) => {
            crate::clawd::audit::record_approval_revocation(&scope, session, generation);
        }
        Err(error) => {
            tracing::error!(
                error = %error,
                "could not retire reusable approvals for a finished session"
            );
        }
    }
}

#[cfg(test)]
pub(crate) fn state_path_for_test() -> PathBuf {
    state_path()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/approvals/generations.rs"
    ));
}
