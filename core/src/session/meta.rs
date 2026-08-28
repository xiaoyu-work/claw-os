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
// SessionClient
// ---------------------------------------------------------------------------

/// Authenticated frontend that initiated a session.
///
/// This is descriptive provenance, not a caller-selected role. Daemon-backed
/// sessions populate it from the broker route and kernel peer facts; local
/// runtimes populate it at their trusted entry point. Missing metadata is
/// deliberately [`Unknown`](SessionSource::Unknown) and unattended.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionSource {
    LocalCli,
    LocalWeb,
    BrokerTask,
    ScheduledTrigger,
    ExternalMcp,
    App,
    System,
    DelegatedAgent,
    #[default]
    Unknown,
}

impl SessionSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalCli => "local-cli",
            Self::LocalWeb => "local-web",
            Self::BrokerTask => "broker-task",
            Self::ScheduledTrigger => "scheduled-trigger",
            Self::ExternalMcp => "external-mcp",
            Self::App => "app",
            Self::System => "system",
            Self::DelegatedAgent => "delegated-agent",
            Self::Unknown => "unknown",
        }
    }
}

/// Trusted interaction metadata carried with a session.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionClient {
    #[serde(default)]
    pub source: SessionSource,
    #[serde(default)]
    pub attended: bool,
    #[serde(default)]
    pub local: bool,
}

impl SessionClient {
    pub const fn new(source: SessionSource, attended: bool, local: bool) -> Self {
        Self {
            source,
            attended,
            local,
        }
    }
}

// ---------------------------------------------------------------------------
// SessionOrigin
// ---------------------------------------------------------------------------

/// How a durable session came to hold its capabilities.
///
/// This is a *provenance marker*, not a role: it says which trusted
/// issuer minted the session, so a reader can tell an ambient
/// conversation apart from a snapshot of authority a user proved (or
/// had approved) when they created an unattended job. It is written
/// only by the daemon-side issuer that already authorised the work —
/// never copied from a request field — and a consumer may act on a
/// delegation variant only after confirming the record itself is
/// root-owned (see [`super::store::record_is_root_owned`]). Absent
/// (`None`) always means "no delegation", so an older or forged record
/// falls back to the minimal baseline.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionOrigin {
    /// Interactive system-Agent work: `cos agent ask/chat`, a `clawd`
    /// transaction. Carries the minimal baseline and nothing else.
    SystemAgentTask,
    /// A `cos cron` job the owner created through the scheduler
    /// authority, which proved or had approved the executor verb
    /// (`proc.spawn`) and each named credential it injects.
    CronDelegation,
    /// A `cos triggers` rule the owner created the same way, whose
    /// executor verb is `agent.spawn`.
    TriggerDelegation,
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

    /// Credential tier captured when the durable session was created.
    /// Kept separate from `role` because capability intersection may
    /// preserve a weaker owner tier than the role's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_tier: Option<u8>,

    /// OS uid that owns this durable session when it represents a
    /// user-scoped daemon object such as a clawd transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uid: Option<u32>,

    /// Trusted issuer that minted this session's capabilities. Only a
    /// daemon-side authority writes it, and only a root-owned record
    /// may be believed. `None` means "no delegation".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOrigin>,

    /// Authenticated frontend and presence metadata for this session.
    ///
    /// For durable authority decisions this field is trusted only when the
    /// containing record is root-owned, exactly like [`SessionOrigin`].
    #[serde(default)]
    pub client: SessionClient,

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
            credential_tier: None,
            owner_uid: None,
            origin: None,
            client: SessionClient::default(),
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/meta.rs"
    ));
}
