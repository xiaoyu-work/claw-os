//! Session metadata — everything that lives in `meta.json` + `lease.json`.
//!
//! These types are written through [`crate::filelock::write_locked`]
//! (atomic tmp-rename) so a crash mid-write cannot leave a partial
//! file. JSONL append-only logs (`turns.jsonl`, `mutations.jsonl`) live
//! in sibling modules.
//!
//! All timestamps are RFC 3339 in UTC, matching the convention used
//! everywhere else in this codebase (see `proc.rs`, `audit.rs`).

use serde::{Deserialize, Serialize};

use crate::caps::Role;

use super::id::SessionId;

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Lifecycle state for a durable session. Stored as the canonical
/// kebab-case string so log greps and GUI labels can use it verbatim.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Created but no agent has ever attached the lease.
    Pending,
    /// A process currently holds the lease and is making progress.
    Running,
    /// An agent voluntarily released the lease (e.g. user hit Ctrl+C
    /// or `cos agent stop`). Resumable by attaching again.
    Paused,
    /// Marked as complete by the runtime. Read-only from here; the GC
    /// can archive once the retention window passes.
    Done,
    /// Terminal failure. Like `Done` but signals that resumption is
    /// not expected to help.
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Running => "running",
            Status::Paused => "paused",
            Status::Done => "done",
            Status::Failed => "failed",
        }
    }

    /// Is the session in a state that accepts new work? Used by the
    /// future api socket to reject `append_turn` on terminal sessions.
    pub fn is_active(self) -> bool {
        matches!(self, Status::Pending | Status::Running | Status::Paused)
    }
}

// ---------------------------------------------------------------------------
// Budget
// ---------------------------------------------------------------------------

/// Per-session resource ceilings. The kernel does not yet enforce
/// these in Phase 1 — they are recorded so the GUI / approval flow can
/// surface "this session has spent 80% of its token budget" and so
/// Phase 4's api socket can reject calls that would overflow.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Hard ceiling on total LLM tokens (prompt + completion) the
    /// session may spend across all `ai.chat*` calls. `None` = no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,

    /// Soft wall-clock ceiling in seconds. `None` = no cap. We say
    /// "soft" because Phase 1 only records it; later phases may pause
    /// the session when reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_seconds: Option<u64>,

    /// Hard ceiling on the number of recorded mutations. Helps catch
    /// "agent went off the rails" loops before they trash the
    /// workspace. `None` = no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutations: Option<u64>,
}

// ---------------------------------------------------------------------------
// Lease
// ---------------------------------------------------------------------------

/// Who currently owns the session.
///
/// Phase 1 reads/writes this as a plain JSON file. Phase 2 will wrap
/// acquisition in `flock(LOCK_EX | LOCK_NB)` so concurrent attaches
/// race against each other safely, and start a background heartbeat
/// thread that bumps `heartbeat_at` on a fixed interval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// OS PID of the holding process.
    pub pid: u32,
    /// Optional human-readable runtime label (`"cos-agent-chat"`,
    /// `"langchain-py"`, …). Recorded purely for display; not part of
    /// any enforcement decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// RFC 3339 — when the current holder acquired the lease.
    pub started_at: String,
    /// RFC 3339 — last heartbeat. If now − heartbeat_at exceeds the
    /// configured TTL (Phase 2), another process may reclaim the lease.
    pub heartbeat_at: String,
}

// ---------------------------------------------------------------------------
// SessionMeta
// ---------------------------------------------------------------------------

/// Everything stored in `meta.json`. Mutable; written atomically each
/// time it changes via [`crate::filelock::write_locked`].
///
/// Notable design choices:
///
/// - `purpose` is a free-form string label. The kernel never validates
///   it; it exists solely so the GUI / `cos agent ls` can show
///   something more useful than a raw sid. ("整理发票", the original
///   user prompt, a workflow name, an app manifest summary — all
///   legitimate values.)
///
/// - `caps` lives in a **separate** file (`caps.json`) because it
///   changes on a different cadence than the rest of meta (every cap
///   grant/revoke vs. status transitions) and we want the smaller,
///   more frequent rewrites to not invalidate readers of the bigger
///   meta blob.
///
/// - `parent_session` powers sub-agent delegation. The Phase 2 lease
///   logic will cross-check that a child's caps are covered by the
///   parent's (`CapSet::covers_all`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: SessionId,

    /// Human-readable label. May be empty for sessions created
    /// programmatically before the agent has any context yet.
    #[serde(default)]
    pub purpose: String,

    /// Role bundle the session was minted with. Kept for audit /
    /// display; the source of truth for enforcement is the CapSet in
    /// `caps.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,

    /// Parent session, if this one was spawned by a sub-agent
    /// delegation. `None` for top-level user-initiated sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<SessionId>,

    pub status: Status,

    #[serde(default)]
    pub budget: Budget,

    /// RFC 3339.
    pub created_at: String,

    /// RFC 3339. Set when `status` transitions to `Done` or `Failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,

    /// Optional free-form label for the agent runtime that created
    /// this session (`"cos-agent"`, `"langchain-py"`, …). Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_runtime: Option<String>,
}

impl SessionMeta {
    /// Build a fresh meta block for a brand-new session. Status is
    /// `Pending` because no agent has attached yet.
    pub fn fresh(id: SessionId, purpose: impl Into<String>) -> Self {
        Self {
            id,
            purpose: purpose.into(),
            role: None,
            parent_session: None,
            status: Status::Pending,
            budget: Budget::default(),
            created_at: now_rfc3339(),
            ended_at: None,
            creator_runtime: None,
        }
    }
}

/// RFC 3339 UTC timestamp at second resolution. Mirrors the helper in
/// `caps/bootstrap.rs` and the format that `audit.rs` already writes.
pub(super) fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_active_flags() {
        assert!(Status::Pending.is_active());
        assert!(Status::Running.is_active());
        assert!(Status::Paused.is_active());
        assert!(!Status::Done.is_active());
        assert!(!Status::Failed.is_active());
    }

    #[test]
    fn status_serializes_as_kebab() {
        assert_eq!(serde_json::to_string(&Status::Done).unwrap(), "\"done\"");
        assert_eq!(serde_json::to_string(&Status::Paused).unwrap(), "\"paused\"");
    }

    #[test]
    fn meta_round_trip_default() {
        let m = SessionMeta::fresh(SessionId::generate(), "test purpose");
        let json = serde_json::to_string(&m).unwrap();
        let back: SessionMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        // Default budget round-trips empty.
        assert!(back.budget.tokens.is_none());
        assert!(back.budget.wall_seconds.is_none());
        assert!(back.budget.mutations.is_none());
    }

    #[test]
    fn meta_round_trip_full() {
        let m = SessionMeta {
            id: SessionId::generate(),
            purpose: "整理发票".into(),
            role: Some(Role::Automator),
            parent_session: Some(SessionId::generate()),
            status: Status::Running,
            budget: Budget {
                tokens: Some(100_000),
                wall_seconds: Some(3600),
                mutations: Some(500),
            },
            created_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            creator_runtime: Some("cos-agent".into()),
        };
        let json = serde_json::to_string_pretty(&m).unwrap();
        let back: SessionMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn lease_round_trip() {
        let l = Lease {
            pid: 1234,
            runtime: Some("cos-agent-chat".into()),
            started_at: "2026-01-01T00:00:00Z".into(),
            heartbeat_at: "2026-01-01T00:00:05Z".into(),
        };
        let json = serde_json::to_string(&l).unwrap();
        let back: Lease = serde_json::from_str(&json).unwrap();
        assert_eq!(l, back);
    }

    #[test]
    fn budget_skips_none_fields_in_json() {
        let b = Budget {
            tokens: Some(100),
            wall_seconds: None,
            mutations: None,
        };
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains("tokens"));
        assert!(!json.contains("wall_seconds"));
        assert!(!json.contains("mutations"));
    }
}
