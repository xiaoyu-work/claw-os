//! Replay engine for `mutations.jsonl`.
//!
//! Walks the log newest-first and undoes each entry. The result is a
//! per-mutation report so a CLI / GUI can show "of the 12 actions
//! recorded, 11 were rolled back successfully and 1 needs manual
//! attention".
//!
//! ## What "manual attention" means
//!
//! - [`Mutation::Opaque`] entries are escape-hatch records the kernel
//!   doesn't know how to interpret. We surface them rather than try
//!   to execute their inverse JSON, because doing so would mean
//!   teaching the rollback engine arbitrary verbs.
//! - Credential variants (`CredentialStore`, `CredentialRevoke`) are
//!   undone by re-storing / deleting the value via
//!   [`crate::credential::rollback_restore`] /
//!   [`crate::credential::rollback_delete`]. Tier/TTL metadata is not
//!   captured in the log, so a restored credential comes back at the
//!   default tier with no expiry.
//!
//! ## Idempotency
//!
//! Rollback is idempotent. Each successfully-undone mutation `seq` is
//! recorded in a per-session sidecar marker (`<session_dir>/rolled_back.json`);
//! a subsequent `rollback(sid)` skips those entries and reports them as
//! `AlreadyDone` rather than replaying their inverse (which is not a
//! defined operation). The append-only `mutations.jsonl` is never
//! mutated — the marker is a separate file.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::id::SessionId;
use super::inverse;
use super::mutation::Mutation;
use super::store::{self, SessionError};
use crate::caps::{require, Scope, Verb};

/// Per-mutation result of a rollback pass.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    /// Position in `mutations.jsonl` this entry came from.
    pub seq: u64,
    /// Short label for the verb that was rolled back. Stable strings,
    /// suitable for a GUI badge.
    pub verb: &'static str,
    /// What the rollback engine did with this entry.
    pub status: Status,
    /// Human-readable detail. Always set; empty string is allowed
    /// only when the action is fully self-explanatory from `verb`.
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Inverse applied successfully.
    Restored,
    /// Path was already in the desired post-rollback state (file did
    /// not exist when we tried to delete it back to nothing, etc.).
    /// Counts as success.
    AlreadyDone,
    /// Engine doesn't know how to undo this entry on its own; left
    /// for the user. Includes the opaque escape hatch and credential
    /// variants until Phase 4 wires them.
    Skipped,
    /// Tried and failed. `detail` carries the error text.
    Failed,
}

/// Walk `mutations.jsonl` newest-first and undo each entry. Returns a
/// per-entry report. Empty log → empty `Vec`.
///
/// Does not mutate `mutations.jsonl` itself — re-running this is safe
/// from a "we won't lose the log" perspective even though the
/// individual replays are not idempotent (see module docs).
pub fn rollback(sid: &SessionId) -> Result<Vec<Outcome>, SessionError> {
    let mut entries = store::iter_mutations(sid)?;
    entries.reverse();

    // Idempotency: a prior pass may already have undone some seqs.
    // Replaying an inverse twice is not a defined operation, so we skip
    // anything recorded in the per-session rolled-back marker and report
    // it as AlreadyDone. The append-only mutations.jsonl is untouched;
    // the marker is a sidecar (see `rolled_back_path`).
    let mut done = load_rolled_back(sid);
    let mut newly_done: Vec<u64> = Vec::new();

    let mut out = Vec::with_capacity(entries.len());
    for rec in entries {
        let seq = rec.seq;
        if done.contains(&seq) {
            out.push(Outcome {
                seq,
                verb: verb_label(&rec.mutation),
                status: Status::AlreadyDone,
                detail: "already rolled back in a prior pass".to_string(),
            });
            continue;
        }
        let outcome = match rec.mutation {
            Mutation::FsWrite { path, prev_blob } => {
                // Replaying an `fs.write` inverse touches the same
                // path the original write claimed. Re-check caps
                // *now*: the policy may have tightened since the
                // forward action was authorized, and a stolen
                // session log shouldn't be able to coerce the kernel
                // into restoring bytes the operator no longer wants
                // to grant. Either `fs.write` (overwrite path with
                // saved bytes) or `fs.delete` (the inverse of "this
                // path didn't exist before") is plausible; we ask
                // for whichever the inverse actually performs.
                let verb = if prev_blob.is_some() {
                    Verb::FS_WRITE
                } else {
                    Verb::FS_DELETE
                };
                match require(verb, Scope::path(path.clone())) {
                    Ok(()) => undo_fs_write(sid, seq, path, prev_blob),
                    Err(d) => Outcome {
                        seq,
                        verb: "fs.write",
                        status: Status::Skipped,
                        detail: format!("denied by caps: {d}"),
                    },
                }
            }
            Mutation::FsDelete { path, blob_id } => {
                // Inverse of `fs.delete` is `fs.write` (restore).
                match require(Verb::FS_WRITE, Scope::path(path.clone())) {
                    Ok(()) => undo_fs_delete(sid, seq, path, blob_id),
                    Err(d) => Outcome {
                        seq,
                        verb: "fs.delete",
                        status: Status::Skipped,
                        detail: format!("denied by caps: {d}"),
                    },
                }
            }
            Mutation::FsRename { from, to } => {
                // Inverse rename touches both endpoints — require
                // write access to both. We don't try to be clever
                // about source-vs-destination semantics; either is a
                // mutation on disk.
                let from_ok = require(Verb::FS_WRITE, Scope::path(from.clone()));
                let to_ok = require(Verb::FS_WRITE, Scope::path(to.clone()));
                match (from_ok, to_ok) {
                    (Ok(()), Ok(())) => undo_fs_rename(seq, from, to),
                    (Err(d), _) | (_, Err(d)) => Outcome {
                        seq,
                        verb: "fs.rename",
                        status: Status::Skipped,
                        detail: format!("denied by caps: {d}"),
                    },
                }
            }
            Mutation::CredentialStore {
                namespace,
                name,
                prev_value,
            } => {
                // Undo a store: restore the prior value if there was one,
                // otherwise delete the key the store created.
                let (res, action) = match prev_value {
                    Some(value) => (
                        crate::credential::rollback_restore(&namespace, &name, &value),
                        "restored prior value",
                    ),
                    None => (
                        crate::credential::rollback_delete(&namespace, &name),
                        "removed (was newly created)",
                    ),
                };
                match res {
                    Ok(()) => Outcome {
                        seq,
                        verb: "credential.store",
                        status: Status::Restored,
                        detail: format!("{namespace}/{name}: {action}"),
                    },
                    Err(e) => Outcome {
                        seq,
                        verb: "credential.store",
                        status: Status::Failed,
                        detail: format!("{namespace}/{name}: {e}"),
                    },
                }
            }
            Mutation::CredentialRevoke {
                namespace,
                name,
                value,
            } => {
                // Undo a revoke: re-store the saved value.
                match crate::credential::rollback_restore(&namespace, &name, &value) {
                    Ok(()) => Outcome {
                        seq,
                        verb: "credential.revoke",
                        status: Status::Restored,
                        detail: format!("{namespace}/{name}: re-stored"),
                    },
                    Err(e) => Outcome {
                        seq,
                        verb: "credential.revoke",
                        status: Status::Failed,
                        detail: format!("{namespace}/{name}: {e}"),
                    },
                }
            }
            Mutation::Opaque { verb, .. } => Outcome {
                seq,
                verb: "opaque",
                status: Status::Skipped,
                detail: format!("opaque mutation '{verb}' — manual review"),
            },
        };
        if matches!(outcome.status, Status::Restored | Status::AlreadyDone) {
            newly_done.push(seq);
        }
        out.push(outcome);
    }

    // Persist the seqs we just undid so a re-run is a no-op. Best-effort:
    // a failed marker write only costs idempotency, not correctness.
    if !newly_done.is_empty() {
        for s in newly_done {
            done.insert(s);
        }
        let _ = save_rolled_back(sid, &done);
    }
    Ok(out)
}

/// Stable verb label for a mutation, used when reporting an entry that a
/// prior pass already rolled back (so we don't have to run its arm).
fn verb_label(m: &Mutation) -> &'static str {
    match m {
        Mutation::FsWrite { .. } => "fs.write",
        Mutation::FsDelete { .. } => "fs.delete",
        Mutation::FsRename { .. } => "fs.rename",
        Mutation::CredentialStore { .. } => "credential.store",
        Mutation::CredentialRevoke { .. } => "credential.revoke",
        Mutation::Opaque { .. } => "opaque",
        _ => "unknown",
    }
}

/// Per-session sidecar listing the mutation seqs already undone. Kept
/// separate from the append-only `mutations.jsonl`.
fn rolled_back_path(sid: &SessionId) -> PathBuf {
    store::session_dir(sid).join("rolled_back.json")
}

/// Load the set of already-rolled-back seqs. Missing or corrupt file →
/// empty set (we'd rather re-attempt an undo than wrongly skip one).
fn load_rolled_back(sid: &SessionId) -> std::collections::BTreeSet<u64> {
    match fs::read_to_string(rolled_back_path(sid)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => std::collections::BTreeSet::new(),
    }
}

/// Persist the set of rolled-back seqs.
fn save_rolled_back(
    sid: &SessionId,
    done: &std::collections::BTreeSet<u64>,
) -> Result<(), SessionError> {
    let path = rolled_back_path(sid);
    let data = serde_json::to_string(done).map_err(SessionError::Encode)?;
    fs::write(&path, data).map_err(|e| SessionError::io(path.clone(), e))
}

fn undo_fs_write(
    sid: &SessionId,
    seq: u64,
    path: String,
    prev_blob: Option<String>,
) -> Outcome {
    let target = PathBuf::from(&path);

    match prev_blob {
        // Path existed before — restore the saved bytes (overwrite
        // whatever is there now).
        Some(blob_id) => match inverse::read_blob(sid, &blob_id) {
            Ok(bytes) => match write_overwrite(&target, &bytes) {
                Ok(()) => Outcome {
                    seq,
                    verb: "fs.write",
                    status: Status::Restored,
                    detail: format!("restored {} ({} bytes)", path, bytes.len()),
                },
                Err(e) => Outcome {
                    seq,
                    verb: "fs.write",
                    status: Status::Failed,
                    detail: format!("write {path}: {e}"),
                },
            },
            Err(e) => Outcome {
                seq,
                verb: "fs.write",
                status: Status::Failed,
                detail: format!("read inverse blob {blob_id}: {e}"),
            },
        },
        // Path did not exist before — wipe whatever the gated write
        // created.
        None => {
            if !target.exists() {
                return Outcome {
                    seq,
                    verb: "fs.write",
                    status: Status::AlreadyDone,
                    detail: format!("{path} already absent"),
                };
            }
            if target.is_dir() {
                return Outcome {
                    seq,
                    verb: "fs.write",
                    status: Status::Failed,
                    detail: format!(
                        "{path} is a directory but mutation expected a created file; refusing"
                    ),
                };
            }
            match fs::remove_file(&target) {
                Ok(()) => Outcome {
                    seq,
                    verb: "fs.write",
                    status: Status::Restored,
                    detail: format!("removed {path}"),
                },
                Err(e) => Outcome {
                    seq,
                    verb: "fs.write",
                    status: Status::Failed,
                    detail: format!("remove {path}: {e}"),
                },
            }
        }
    }
}

fn undo_fs_delete(sid: &SessionId, seq: u64, path: String, blob_id: String) -> Outcome {
    let target = PathBuf::from(&path);
    match inverse::read_blob(sid, &blob_id) {
        Ok(bytes) => match write_overwrite(&target, &bytes) {
            Ok(()) => Outcome {
                seq,
                verb: "fs.delete",
                status: Status::Restored,
                detail: format!("restored {} ({} bytes)", path, bytes.len()),
            },
            Err(e) => Outcome {
                seq,
                verb: "fs.delete",
                status: Status::Failed,
                detail: format!("write {path}: {e}"),
            },
        },
        Err(e) => Outcome {
            seq,
            verb: "fs.delete",
            status: Status::Failed,
            detail: format!("read inverse blob {blob_id}: {e}"),
        },
    }
}

fn undo_fs_rename(seq: u64, from: String, to: String) -> Outcome {
    let from_p = PathBuf::from(&from);
    let to_p = PathBuf::from(&to);

    if !to_p.exists() {
        return Outcome {
            seq,
            verb: "fs.rename",
            status: Status::AlreadyDone,
            detail: format!("{to} already absent"),
        };
    }
    if from_p.exists() {
        return Outcome {
            seq,
            verb: "fs.rename",
            status: Status::Failed,
            detail: format!("cannot reverse rename: {from} already exists"),
        };
    }
    if let Some(parent) = from_p.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                return Outcome {
                    seq,
                    verb: "fs.rename",
                    status: Status::Failed,
                    detail: format!("mkdir {}: {e}", parent.display()),
                };
            }
        }
    }
    match fs::rename(&to_p, &from_p) {
        Ok(()) => Outcome {
            seq,
            verb: "fs.rename",
            status: Status::Restored,
            detail: format!("renamed {to} -> {from}"),
        },
        Err(e) => Outcome {
            seq,
            verb: "fs.rename",
            status: Status::Failed,
            detail: format!("rename {to} -> {from}: {e}"),
        },
    }
}

/// Atomic-ish write: tmp + rename. Used for both restore-from-blob
/// (fs.write rollback) and undelete (fs.delete rollback).
fn write_overwrite(target: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = target.with_extension(format!(
        "{}.cosrollback.tmp",
        target
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
    ));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::mutation::{Mutation, MutationRecord};
    use crate::session::store;

    /// Audit fix (session/rollback.rs HIGH): every replayed mutation
    /// must re-check caps at replay time rather than trusting the
    /// forward-action authorisation that was recorded weeks ago. If
    /// caps now deny the verb, the inverse must be `Skipped` with a
    /// `denied by caps:` detail — *not* silently replayed.
    ///
    /// We force a denial by running in Strict mode without a session
    /// in the process registry, which is the simplest way to make
    /// `caps::require` return `Err` for every call.
    #[test]
    fn rollback_rechecks_caps() {
        // Serialize against other tests that mutate process-global env.
        let _lock = crate::test_env::lock_env();

        // Isolate the on-disk session store under a tempdir so the
        // test doesn't touch /var/lib/cos or another test's data.
        let dir = tempfile::tempdir().expect("tempdir");
        let prev_data = std::env::var_os("COS_DATA_DIR");
        std::env::set_var("COS_DATA_DIR", dir.path());

        // Force caps::require to deny every call:
        //   - Strict mode: missing session ⇒ denied.
        //   - COS_SESSION unset ⇒ also denied.
        let prev_mode = std::env::var_os("COS_PERMS_MODE");
        let prev_session = std::env::var_os("COS_SESSION");
        let prev_audit = std::env::var_os("COS_CAPS_AUDIT");
        std::env::set_var("COS_PERMS_MODE", "strict");
        std::env::remove_var("COS_SESSION");
        // Quiet the caps audit log writer so the test doesn't try
        // to write to /var/log.
        std::env::set_var("COS_CAPS_AUDIT", "0");

        // Create a session and append one fs.write mutation. We do
        // this AFTER setting strict mode + clearing COS_SESSION so
        // the caps check at replay time hits the "no session" branch.
        let sid = store::create("test").expect("create session");
        let mutation = MutationRecord::new(Mutation::FsWrite {
            path: format!("{}/file.txt", dir.path().display()),
            prev_blob: None,
        });
        store::record_mutation(&sid, mutation).expect("record mutation");

        let outcomes = rollback(&sid).expect("rollback");
        assert_eq!(outcomes.len(), 1, "expected exactly one replay outcome");
        let o = &outcomes[0];
        assert_eq!(
            o.status,
            Status::Skipped,
            "denied entry must be Skipped, got {:?} (detail={:?})",
            o.status,
            o.detail,
        );
        assert!(
            o.detail.contains("denied by caps:"),
            "skip detail must mention caps denial, got {:?}",
            o.detail,
        );

        // Restore env so we don't poison neighbouring tests.
        match prev_data {
            Some(v) => std::env::set_var("COS_DATA_DIR", v),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
        match prev_mode {
            Some(v) => std::env::set_var("COS_PERMS_MODE", v),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
        match prev_session {
            Some(v) => std::env::set_var("COS_SESSION", v),
            None => std::env::remove_var("COS_SESSION"),
        }
        match prev_audit {
            Some(v) => std::env::set_var("COS_CAPS_AUDIT", v),
            None => std::env::remove_var("COS_CAPS_AUDIT"),
        }
    }
}
