//! Mutation record — what every reversible state change writes to
//! `mutations.jsonl`.
//!
//! Phase 1 ships **the schema and append/iter mechanics only**. Wiring
//! every gated verb (`fs.write`, `credential.store`, …) to actually
//! call [`crate::session::record_mutation`] is Phase 3.
//!
//! ## Why typed mutations and not just an audit log
//!
//! The audit log (`/var/log/cos/{audit,caps}.jsonl`) already records
//! *that* an action happened, with enough detail to investigate. But
//! audit lines are opaque: a human reading "fs.write /etc/passwd"
//! cannot ask the OS to undo it without knowing what the file
//! contained before. A mutation record carries the **inverse** —
//! literally the bytes / state needed to roll it back — which lets us
//! ship `cos agent undo <sid>` without an LLM in the loop.
//!
//! ## Layout
//!
//! One JSON object per line in `mutations.jsonl`, append-only. The
//! `inverse` field is the action that would put the system back, in
//! the same vocabulary as the forward mutation. Rollback is "iterate
//! the file in reverse and execute each inverse".
//!
//! ## Storage of the inverse payload
//!
//! For small inversions (a credential value, a single deleted byte)
//! the inverse payload lives inline. For large inversions (the
//! contents of an overwritten file) the inverse references a blob
//! stored under `<session_dir>/files/inverse/<mutation_id>.bin` so the
//! JSONL stays small and greppable. Phase 3 owns the blob convention;
//! Phase 1 just leaves room in the schema for the path/hash pair.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::meta::now_rfc3339;

/// One reversible action. Variants intentionally lean — Phase 3 will
/// add the rest as it wires each gated verb. The enum is
/// `#[non_exhaustive]` so adding a variant in a later phase is not a
/// breaking change for downstream consumers reading old logs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Mutation {
    /// `fs.write` overwrote or created `path`. `prev_blob` is `None`
    /// if the path did not exist (inverse is `fs.delete`); otherwise
    /// it points at the saved previous contents.
    FsWrite {
        path: String,
        /// `None` => path did not exist before; rollback deletes it.
        /// `Some(blob_id)` => previous content lives at
        /// `<session>/files/inverse/<blob_id>.bin`.
        prev_blob: Option<String>,
    },
    /// `fs.delete` removed `path`. `blob_id` is the saved copy for
    /// rollback.
    FsDelete { path: String, blob_id: String },
    /// `fs.rename` moved a path. Inverse is the opposite rename.
    FsRename { from: String, to: String },
    /// `credential.store` wrote a secret. `prev_value` is the saved
    /// previous value, or `None` if the key was new (inverse =
    /// `credential.revoke`).
    CredentialStore {
        namespace: String,
        name: String,
        prev_value: Option<String>,
    },
    /// `credential.revoke` removed a secret. `value` is the saved
    /// previous value so rollback can re-store it.
    CredentialRevoke {
        namespace: String,
        name: String,
        value: String,
    },
    /// A mutation the kernel doesn't have a typed shape for yet.
    /// Escape hatch so apps can record undo info during the long Phase
    /// 3 wiring effort without blocking on a new enum variant per
    /// verb. Forward / inverse are both opaque JSON; rollback will
    /// surface them to the user (or to a future replay tool) rather
    /// than execute them automatically.
    Opaque {
        verb: String,
        forward: Value,
        inverse: Value,
    },
}

/// What gets written to the JSONL: a [`Mutation`] plus identification
/// fields the store adds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MutationRecord {
    /// Monotonic 0-based index within the session.
    #[serde(default)]
    pub seq: u64,

    /// RFC 3339 UTC. Auto-stamped if empty.
    #[serde(default)]
    pub at: String,

    /// Optional turn this mutation was caused by (so the UI can
    /// group: "during turn 14 the agent did these three things").
    /// `None` for mutations recorded outside an LLM turn (e.g. a
    /// user-driven `cos app fs rm`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_seq: Option<u64>,

    /// Optional label for the runtime that recorded this mutation
    /// (`"cos-agent"`, `"langchain-py"`, …). Mirrors `Turn::runtime`
    /// so a cross-runtime session's mutation log shows which agent
    /// owned each change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,

    /// The reversible action itself.
    pub mutation: Mutation,
}

impl MutationRecord {
    /// Convenience constructor — the store fills `seq` and `at`.
    pub fn new(mutation: Mutation) -> Self {
        Self {
            seq: 0,
            at: String::new(),
            turn_seq: None,
            runtime: None,
            mutation,
        }
    }

    pub fn with_turn(mut self, seq: u64) -> Self {
        self.turn_seq = Some(seq);
        self
    }

    pub fn with_runtime(mut self, runtime: impl Into<String>) -> Self {
        self.runtime = Some(runtime.into());
        self
    }

    pub(super) fn stamp_default_time(&mut self) {
        if self.at.is_empty() {
            self.at = now_rfc3339();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_write_round_trip_new_file() {
        let m = MutationRecord::new(Mutation::FsWrite {
            path: "/workspace/notes.md".into(),
            prev_blob: None,
        });
        let json = serde_json::to_string(&m).unwrap();
        let back: MutationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn fs_write_round_trip_overwrite() {
        let m = MutationRecord::new(Mutation::FsWrite {
            path: "/workspace/notes.md".into(),
            prev_blob: Some("blob-abc123".into()),
        });
        let json = serde_json::to_string(&m).unwrap();
        let back: MutationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn credential_round_trip() {
        let m = MutationRecord::new(Mutation::CredentialStore {
            namespace: "openai".into(),
            name: "key".into(),
            prev_value: None,
        });
        let json = serde_json::to_string(&m).unwrap();
        let back: MutationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn opaque_escape_hatch_round_trip() {
        let m = MutationRecord::new(Mutation::Opaque {
            verb: "db.write".into(),
            forward: serde_json::json!({ "table": "x", "row": 1 }),
            inverse: serde_json::json!({ "table": "x", "delete_row": 1 }),
        });
        let json = serde_json::to_string(&m).unwrap();
        let back: MutationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn with_turn_attaches_seq() {
        let m = MutationRecord::new(Mutation::FsRename {
            from: "/a".into(),
            to: "/b".into(),
        })
        .with_turn(42);
        assert_eq!(m.turn_seq, Some(42));
    }

    #[test]
    fn mutation_tag_is_kebab_case() {
        let json = serde_json::to_string(&Mutation::FsWrite {
            path: "/x".into(),
            prev_blob: None,
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"fs-write\""), "{json}");

        let json = serde_json::to_string(&Mutation::CredentialStore {
            namespace: "ns".into(),
            name: "n".into(),
            prev_value: None,
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"credential-store\""), "{json}");
    }
}
