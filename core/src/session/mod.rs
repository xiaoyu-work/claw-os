//! Durable session — the OS-level handle for a piece of agent work.
//!
//! Today's "session" (see [`crate::caps::bootstrap`]) is short-lived: a
//! row in `proc/registry.json` written when a `cos` process starts and
//! removed on `Drop`. That is fine for synchronous CLI invocations, but
//! it gives the user nothing they can `ls`, `pause`, `resume`, hand off
//! to another agent runtime, or rollback days later. This module is
//! the durable counterpart: each session is a directory on disk, owned
//! by no single process. Any agent that holds the [`Lease`] is the
//! current "runner"; when the lease is released or its heartbeat
//! expires, another agent can pick the session up exactly where it was.
//!
//! ## Why a directory and not a database row
//!
//! - Cross-runtime handover is "read the same files" — no RPC contract
//!   to keep in sync between two agent stacks.
//! - Append-only logs (`turns.jsonl`, `mutations.jsonl`) survive
//!   crashes; partial JSONL lines on the tail are discardable.
//! - Atomic small writes (`meta.json`, `caps.json`, `lease.json`,
//!   `state.json`) go through [`crate::filelock`] (write-tmp + rename).
//! - GC / archive is `tar` + `rm -rf`, not a schema migration.
//!
//! ## Layout
//!
//! ```text
//! $COS_DATA_DIR/sessions/<sid>/
//!   meta.json         — purpose / role / parent_sid / status / budget / timestamps
//!   caps.json         — current CapSet (mutable: caps can be granted/revoked)
//!   turns.jsonl       — append-only conversation / tool-call events
//!   mutations.jsonl   — append-only reversible state changes
//!   state.json        — per-runtime opaque scratch (`{"<runtime>": <value>}`)
//!   lease.json        — current owner: {pid, started_at, heartbeat_at}
//!   files/            — session-local scratch (intermediate artifacts)
//! ```
//!
//! ## Scope of this module (Phase 1)
//!
//! Phase 1 ships the data model + disk layout + atomic IO + tests. It
//! does **not** yet:
//!
//! - acquire `lease.json` with `flock` or run a heartbeat thread (Phase 2),
//! - replace the in-process `caps::bootstrap` session (Phase 1.4),
//! - record mutations from every gated verb (Phase 3),
//! - expose a `cos-apid` socket (Phase 4),
//! - surface anything in the user CLI (Phase 5).
//!
//! Everything here is a library: a future `cos-apid` daemon and the
//! existing in-process `cos` binary will both call these same functions.

mod gc;
mod id;
mod meta;
mod mutation;
mod store;
mod turn;

#[cfg(test)]
mod tests;

pub use gc::{archive_path, archive_root, gc_archive, is_archived, GcStats};
pub use id::{InvalidSessionId, SessionId};
pub use meta::{Budget, Lease, SessionMeta, Status};
pub use mutation::{Mutation, MutationRecord};
pub use store::{
    append_turn, create, end, get_caps, get_meta, iter_mutations, iter_turns, list, read_state,
    record_mutation, session_dir, sessions_root, set_caps, update_meta, write_state, SessionError,
};
pub use turn::{Turn, TurnRole};
