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
//!   stubbed in Phase 3: the [`Outcome`] reports them as `Skipped` so
//!   downstream sees them, and the credential-namespace work in Phase
//!   4 will fill in the actual restore.
//!
//! ## Idempotency
//!
//! Rollback is *not* idempotent today. Calling `rollback(sid)` twice
//! will undo the original work the first time and try to undo the
//! "now-rolled-back" state the second time, which generally fails
//! cleanly (the file is back to its prior state, the inverse blobs
//! still exist, etc.) but is not a defined operation. A future
//! `mutations.jsonl` "rolled-back marker" line could fix this; we'll
//! add it when a real workflow demands it.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::id::SessionId;
use super::inverse;
use super::mutation::Mutation;
use super::store::{self, SessionError};

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

    let mut out = Vec::with_capacity(entries.len());
    for rec in entries {
        let seq = rec.seq;
        let outcome = match rec.mutation {
            Mutation::FsWrite { path, prev_blob } => undo_fs_write(sid, seq, path, prev_blob),
            Mutation::FsDelete { path, blob_id } => undo_fs_delete(sid, seq, path, blob_id),
            Mutation::FsRename { from, to } => undo_fs_rename(seq, from, to),
            Mutation::CredentialStore { namespace, name, .. } => Outcome {
                seq,
                verb: "credential.store",
                status: Status::Skipped,
                detail: format!(
                    "credential rollback not implemented (Phase 4): {namespace}/{name}"
                ),
            },
            Mutation::CredentialRevoke { namespace, name, .. } => Outcome {
                seq,
                verb: "credential.revoke",
                status: Status::Skipped,
                detail: format!(
                    "credential rollback not implemented (Phase 4): {namespace}/{name}"
                ),
            },
            Mutation::Opaque { verb, .. } => Outcome {
                seq,
                verb: "opaque",
                status: Status::Skipped,
                detail: format!("opaque mutation '{verb}' — manual review"),
            },
        };
        out.push(outcome);
    }
    Ok(out)
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
